// SPDX-License-Identifier: GPL-3.0-or-later
//! Application setup, shared state, command line and `rdp://` / `.rdp` handling.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use transom_view::core::engine::Finished;
use transom_view::core::profile::ProfileStore;
use transom_view::core::secrecy::SecretString;
use transom_view::core::trust::TrustStore;
use transom_view_freerdp::FreeRdpEngine;

use crate::session::Session;
use crate::window::MainWindow;

/// Application ID (PLAN §10.1: proposed, not final; change it here only).
pub const APP_ID: &str = "io.github.jm2.Transom";

/// App shortcuts, disabled while a remote session has keyboard focus.
pub fn app_shortcuts() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        ("app.quit", &["<Control>q"]),
        ("app.new-window", &["<Control><Shift>n"]),
        ("win.new-connection", &["<Control>n"]),
        ("win.import", &["<Control>o"]),
        ("win.search", &["<Control>f"]),
    ]
}

/// State shared by all windows.
pub struct AppState {
    pub profiles: ProfileStore,
    pub trust: RefCell<TrustStore>,
    pub engine: FreeRdpEngine,
    /// Engines still shutting down after their tab closed; awaited at exit.
    pub draining: RefCell<Vec<Finished>>,
    sessions: RefCell<Vec<Rc<Session>>>,
    windows: RefCell<Vec<Rc<MainWindow>>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    fn new() -> Self {
        let config = glib::user_config_dir().join("transom");
        let trust_path = config.join("known-hosts.toml");
        let trust = TrustStore::load(&trust_path).unwrap_or_else(|e| {
            // Keep the unreadable file for the user to inspect; never overwrite it.
            let aside = trust_path.with_extension("toml.broken");
            tracing::error!(
                "{}: {e}; moved to {}",
                trust_path.display(),
                aside.display()
            );
            let _ = std::fs::rename(&trust_path, &aside);
            TrustStore::load(&trust_path).expect("empty trust store")
        });
        Self {
            profiles: ProfileStore::new(config.join("profiles")),
            trust: RefCell::new(trust),
            engine: FreeRdpEngine::new(glib::user_cache_dir().join("transom").join("freerdp")),
            draining: RefCell::new(Vec::new()),
            sessions: RefCell::new(Vec::new()),
            windows: RefCell::new(Vec::new()),
        }
    }

    pub fn add_session(&self, s: Rc<Session>) {
        self.sessions.borrow_mut().push(s);
    }

    pub fn session_for(&self, child: &gtk::Widget) -> Option<Rc<Session>> {
        self.sessions
            .borrow()
            .iter()
            .find(|s| s.root.upcast_ref::<gtk::Widget>() == child)
            .cloned()
    }

    pub fn take_session(&self, child: &gtk::Widget) -> Option<Rc<Session>> {
        let mut v = self.sessions.borrow_mut();
        let i = v
            .iter()
            .position(|s| s.root.upcast_ref::<gtk::Widget>() == child)?;
        Some(v.remove(i))
    }

    pub fn window_for(&self, widget: &impl IsA<gtk::Widget>) -> Option<Rc<MainWindow>> {
        let root = widget.root()?;
        self.windows
            .borrow()
            .iter()
            .find(|w| w.window.upcast_ref::<gtk::Root>() == &root)
            .cloned()
    }

    pub fn forget_window(&self, w: &MainWindow) {
        self.windows.borrow_mut().retain(|x| !std::ptr::eq(&**x, w));
    }

    pub fn refresh_all(&self) {
        for w in self.windows.borrow().iter() {
            w.refresh();
        }
    }

    /// Close every session and wait (bounded) for the engines to exit, so FreeRDP
    /// is never torn down by process exit.
    fn shutdown(&self) {
        for s in self.sessions.borrow_mut().drain(..) {
            s.close();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        for f in self.draining.borrow_mut().drain(..) {
            let left = deadline.saturating_duration_since(Instant::now());
            if !f.wait(left) {
                tracing::warn!("an RDP engine did not shut down in time");
            }
        }
    }
}

pub fn new_window(app: &adw::Application, state: &Rc<AppState>) -> Rc<MainWindow> {
    let w = MainWindow::new(app, state.clone());
    state.windows.borrow_mut().push(w.clone());
    w
}

fn active_window(app: &adw::Application, state: &Rc<AppState>) -> Rc<MainWindow> {
    let current = app.active_window();
    let found = current.and_then(|cw| {
        state
            .windows
            .borrow()
            .iter()
            .find(|w| w.window.upcast_ref::<gtk::Window>() == &cw)
            .cloned()
    });
    found.unwrap_or_else(|| {
        let w = new_window(app, state);
        w.window.present();
        w
    })
}

thread_local! {
    static CLI_PASSWORD: RefCell<Option<SecretString>> = const { RefCell::new(None) };
}

/// See the `connect_open` handler in [`run`]: only macOS routes files through `open`.
#[cfg(target_os = "macos")]
const OPEN_FLAGS: gio::ApplicationFlags = gio::ApplicationFlags::HANDLES_OPEN;
#[cfg(not(target_os = "macos"))]
const OPEN_FLAGS: gio::ApplicationFlags = gio::ApplicationFlags::empty();

/// Imports a `.rdp` file; `fallback` names it if it has no basename.
fn open_rdp_file(w: &Rc<MainWindow>, file: &gio::File, fallback: &str) {
    let name = file
        .basename()
        .map(|b| b.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_owned());
    match file.load_contents(gio::Cancellable::NONE) {
        Ok((bytes, _)) => w.open_rdp_bytes(&bytes, &name),
        Err(e) => w.toast(&format!("Couldn't open {name}: {e}")),
    }
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        // Command line, not HANDLES_OPEN: GIO's GFile parsing mangles Microsoft-style
        // links (rdp://full%20address=s:host:port&… becomes rdp://full%20address=s:0/),
        // so rdp:// arguments must be read raw. macOS also needs HANDLES_OPEN (below).
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE | OPEN_FLAGS)
        .build();
    app.add_main_option(
        "password-file",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::Filename,
        "Read the password for an address given on the command line from FILE",
        Some("FILE"),
    );
    app.connect_handle_local_options(|_, dict| {
        if let Some(path) = dict.lookup::<PathBuf>("password-file").ok().flatten() {
            match std::fs::read_to_string(&path) {
                Ok(s) => CLI_PASSWORD.with(|c| {
                    *c.borrow_mut() = Some(SecretString::from(
                        s.lines().next().unwrap_or("").to_string(),
                    ))
                }),
                Err(e) => {
                    eprintln!("{}: {e}", path.display());
                    return std::ops::ControlFlow::Break(glib::ExitCode::from(2));
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    });

    let state: Rc<RefCell<Option<Rc<AppState>>>> = Rc::new(RefCell::new(None));
    let st = state.clone();
    app.connect_startup(move |app| {
        let s = Rc::new(AppState::new());
        *st.borrow_mut() = Some(s.clone());
        setup_app_actions(app, &s);
    });
    let st = state.clone();
    app.connect_activate(move |app| {
        let Some(s) = st.borrow().clone() else { return };
        active_window(app, &s).window.present();
    });
    let st = state.clone();
    app.connect_command_line(move |app, cmd| {
        let Some(s) = st.borrow().clone() else {
            return glib::ExitCode::FAILURE;
        };
        let mut targets = Vec::new();
        let mut args = cmd.arguments().into_iter().skip(1);
        while let Some(a) = args.next() {
            let a = a.to_string_lossy().into_owned();
            if a == "--password-file" {
                args.next(); // consumed by handle-local-options
                continue;
            }
            if a.starts_with("--password-file=") || a == "--" {
                continue;
            }
            targets.push(a);
        }
        let w = active_window(app, &s);
        w.window.present();
        for t in targets {
            if t.to_ascii_lowercase().starts_with("rdp://") {
                let password = CLI_PASSWORD.with(|c| c.borrow_mut().take());
                open_link(&w, &t, password);
            } else {
                open_rdp_file(&w, &cmd.create_file_for_arg(&t), &t);
            }
        }
        glib::ExitCode::SUCCESS
    });
    // macOS delivers Finder's "Open With" (and `open file.rdp`) as an Apple Event,
    // which GTK forwards to the open signal, never to the command line. rdp:// links
    // (kAEGetURL) come through transom-macos instead (below).
    #[cfg(target_os = "macos")]
    {
        let st = state.clone();
        app.connect_open(move |app, files, _hint| {
            let Some(s) = st.borrow().clone() else { return };
            let w = active_window(app, &s);
            w.window.present();
            for file in files {
                if file.has_uri_scheme("rdp") {
                    open_link(&w, &file.uri(), None);
                } else {
                    open_rdp_file(&w, file, &file.uri());
                }
            }
        });
    }
    let st = state.clone();
    app.connect_shutdown(move |_| {
        if let Some(s) = st.borrow().as_ref() {
            s.shutdown();
        }
    });

    // macOS delivers links as Apple Events, which GTK doesn't forward (see
    // transom-macos). Registered before the app runs, so the link an app launch was
    // started for isn't missed; the handler runs on the main thread.
    #[cfg(target_os = "macos")]
    {
        let app = app.downgrade();
        let st = state.clone();
        transom_macos::on_open_url(move |url| {
            let (app, st) = (app.clone(), st.clone());
            // From the main loop: at launch the event can come before startup.
            glib::idle_add_local_once(move || {
                let (Some(app), Some(s)) = (app.upgrade(), st.borrow().clone()) else {
                    tracing::warn!("link before the app started: {url}");
                    return;
                };
                let w = active_window(&app, &s);
                w.window.present();
                open_link(&w, &url, None);
            });
            // The event is handled inside GDK's NSRunLoop wait, which an idle source
            // added from this thread doesn't interrupt: without a wakeup the link waits
            // for the next unrelated event (mouse, focus) when the app is already running.
            glib::MainContext::default().wakeup();
        });
    }

    crate::dev::install(&app);
    app.run()
}

/// Connect to an `rdp://` link (command line, or a link opened on macOS).
fn open_link(w: &Rc<MainWindow>, link: &str, password: Option<SecretString>) {
    match transom_view::core::rdpfile::RdpFile::from_uri(link)
        .ok()
        .and_then(|f| transom_view::core::profile::Profile::from_rdp(&f))
    {
        Some(p) => w.open_session(p, true, password),
        None => w.toast(&format!("Invalid link: {link}")),
    }
}

fn setup_app_actions(app: &adw::Application, state: &Rc<AppState>) {
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            for w in app.windows() {
                w.close();
            }
            app.quit();
        }
    ));
    app.add_action(&quit);

    let new_win = gio::SimpleAction::new("new-window", None);
    let s = state.clone();
    new_win.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| new_window(&app, &s).window.present()
    ));
    app.add_action(&new_win);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Transom")
                .application_icon(APP_ID)
                .version(env!("CARGO_PKG_VERSION"))
                .comments("A focused, HiDPI-correct remote desktop client")
                .license_type(gtk::License::Gpl30)
                .website("https://github.com/jm2/transom")
                .build();
            dialog.add_credit_section(
                Some("Built on"),
                &[&format!("FreeRDP {}", FreeRdpEngine::freerdp_version())],
            );
            dialog.present(app.active_window().as_ref());
        }
    ));
    app.add_action(&about);

    for (action, accels) in app_shortcuts() {
        app.set_accels_for_action(action, accels);
    }
}
