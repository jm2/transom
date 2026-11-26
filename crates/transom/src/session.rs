// SPDX-License-Identifier: GPL-3.0-or-later
//! One remote session: a `TransomView` plus connecting / error overlays.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use transom_view::TransomView;
use transom_view::core::engine::{AuthKind, CertDecision, ConnectStage, Credentials};
use transom_view::core::profile::Profile;
use transom_view::core::secrecy::SecretString;
use transom_view::core::trust::TrustStatus;

use crate::app::AppState;
use crate::{dialogs, secrets};

/// How long the connecting page waits for the remote's first picture after
/// connecting before showing the session anyway (a server that sends nothing must not
/// leave it hidden behind the page).
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Session {
    pub profile: RefCell<Profile>,
    /// Opened from a URI, file or quick connect: not saved, passwords not remembered.
    pub temporary: bool,
    pub view: TransomView,
    pub root: gtk::Overlay,
    overlay: adw::Bin,
    state: Rc<AppState>,
    page: RefCell<Option<adw::TabPage>>,
    closing: Cell<bool>,
    /// The last server prompt's answer, applied once it logs in: `Some(Some(pw))`
    /// saves `pw`, `Some(None)` (Remember Password off) forgets a saved one.
    pending_remember: RefCell<Option<Option<SecretString>>>,
    auth_failures: Cell<u32>,
    skip_saved_password: Cell<bool>,
    /// Fullscreen windows on the other monitors (multi-monitor mode).
    followers: RefCell<Vec<gtk::Window>>,
    /// The connecting page's progress line, while that page is shown.
    stage: RefCell<Option<gtk::Label>>,
    /// Shows the session if the first picture doesn't arrive (FIRST_FRAME_TIMEOUT).
    reveal_timer: RefCell<Option<glib::SourceId>>,
    /// The devices this session can't share were reported (once, not per reconnect).
    device_problems_shown: Cell<bool>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("profile", &self.profile.borrow().id)
            .field("temporary", &self.temporary)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// See `TransomView::set_command_as_ctrl`; covers multi-monitor followers too.
    pub fn set_command_as_ctrl(&self, on: bool) {
        self.view.set_command_as_ctrl(on);
        for w in self.followers.borrow().iter() {
            if let Some(v) = w.child().and_downcast::<TransomView>() {
                v.set_command_as_ctrl(on);
            }
        }
    }

    pub fn new(state: Rc<AppState>, profile: Profile, temporary: bool) -> Rc<Self> {
        let view = TransomView::new();
        view.set_command_as_ctrl(state.command_as_ctrl.get());
        view.update_property(&[gtk::accessible::Property::Label(&format!(
            "Remote desktop: {}",
            profile.display_name()
        ))]);
        view.set_hexpand(true);
        view.set_vexpand(true);
        // GDK on macOS sees neither password managers' concealed markers nor every
        // pasteboard change: the clipboard bridge asks the pasteboard itself.
        #[cfg(target_os = "macos")]
        view.set_platform_clipboard(|| {
            transom_macos::pasteboard()
                .map(|p| transom_view::PlatformClipboard {
                    concealed: p.concealed,
                    change_count: Some(p.change_count),
                })
                .unwrap_or_default()
        });
        let root = gtk::Overlay::new();
        root.set_child(Some(&view));
        let overlay = adw::Bin::new();
        overlay.add_css_class("background");
        root.add_overlay(&overlay);

        let session = Rc::new(Self {
            profile: RefCell::new(profile),
            temporary,
            view,
            root,
            overlay,
            state,
            page: RefCell::new(None),
            closing: Cell::new(false),
            pending_remember: RefCell::new(None),
            auth_failures: Cell::new(0),
            skip_saved_password: Cell::new(false),
            followers: RefCell::new(Vec::new()),
            stage: RefCell::new(None),
            reveal_timer: RefCell::new(None),
            device_problems_shown: Cell::new(false),
        });
        session.wire();
        session
    }

    pub fn set_page(&self, page: &adw::TabPage) {
        page.set_title(&self.profile.borrow().display_name());
        // Tooltips are markup; the address can come from a file or link.
        page.set_tooltip(&glib::markup_escape_text(&self.profile.borrow().address()));
        *self.page.borrow_mut() = Some(page.clone());
    }

    fn set_loading(&self, loading: bool) {
        if let Some(p) = self.page.borrow().as_ref() {
            p.set_loading(loading);
        }
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.view.set_cert_handler(move |view, prompt| {
            let Some(s) = weak.upgrade() else { return };
            let info = &prompt.info;
            let status =
                s.state
                    .trust
                    .borrow()
                    .check(&info.host, info.port, &info.fingerprint_sha256);
            if status == TrustStatus::Trusted {
                let _ = prompt.reply.send(CertDecision::AcceptOnce);
                return;
            }
            let (host, port, fp) = (
                info.host.clone(),
                info.port,
                info.fingerprint_sha256.clone(),
            );
            let s2 = s.clone();
            let reply = prompt.reply.clone();
            dialogs::certificate(view, &prompt, &status, move |decision| {
                if decision == CertDecision::AcceptAndRemember
                    && let Err(e) = s2.state.trust.borrow_mut().remember(&host, port, &fp)
                {
                    tracing::warn!("could not save trusted certificate: {e}");
                }
                let _ = reply.send(decision);
            });
            drop(prompt);
        });

        let weak = Rc::downgrade(self);
        self.view.set_auth_handler(move |view, req| {
            let Some(s) = weak.upgrade() else { return };
            let gateway = req.kind == AuthKind::Gateway;
            let target = if gateway {
                format!("the RD Gateway {}", req.host)
            } else {
                // Name the actual host: a file's name is whatever its sender chose,
                // and a server may have redirected the connection elsewhere.
                let p = s.profile.borrow();
                let (name, addr) = (p.display_name(), p.address());
                if !req.host.is_empty() && !req.host.eq_ignore_ascii_case(&p.host) {
                    format!("{} (redirected from {addr})", req.host)
                } else if name == addr || name == p.host {
                    addr
                } else {
                    format!("{name} ({addr})")
                }
            };
            let warning = req.unprotected.then_some(
                "This computer didn't accept Network Level Authentication, so your \
                 password goes to it directly. If it normally does, someone may be \
                 intercepting the connection: cancel. (To stop this question for a \
                 computer that never supports NLA, choose TLS security in its settings.)",
            );
            let s2 = s.clone();
            let failed = s.auth_failures.get() > 0;
            // The keyring entry holds the server password; never put a gateway's there.
            let can_remember = !gateway && !s.temporary && secrets::AVAILABLE;
            let (user, domain) = (req.username.clone(), req.domain.clone());
            let prompt = dialogs::CredentialPrompt {
                target: &target,
                username: user.as_deref(),
                domain: domain.as_deref(),
                can_remember,
                failed_before: failed,
                warning,
            };
            dialogs::credentials(view, prompt, move |entered| match entered {
                Some(c) => {
                    // Only the latest server answer counts: an earlier "remember"
                    // must not save a password typed later with it turned off,
                    // and turning it off forgets a saved one. A gateway answer
                    // (never remembered) leaves it alone.
                    if !gateway && can_remember {
                        *s2.pending_remember.borrow_mut() =
                            Some(c.remember.then(|| c.password.clone()));
                    }
                    let mut p = s2.profile.borrow_mut();
                    if gateway {
                        if let Some(g) = p.gateway.as_mut() {
                            g.username = Some(c.username.clone()).filter(|u| !u.is_empty());
                        }
                    } else {
                        p.username = Some(c.username.clone()).filter(|u| !u.is_empty());
                        p.domain = c.domain.clone();
                    }
                    drop(p);
                    let _ = req.reply.send(Some(Credentials {
                        username: c.username,
                        domain: c.domain,
                        password: c.password,
                    }));
                }
                None => {
                    let _ = req.reply.send(None);
                }
            });
        });

        let weak = Rc::downgrade(self);
        self.view.connect_progress(move |_, stage| {
            let Some(s) = weak.upgrade() else { return };
            s.set_stage(match stage {
                ConnectStage::Contacting => "Contacting the remote computer…",
                ConnectStage::Authenticating => "Signing in…",
                ConnectStage::Configuring => "Setting up the session…",
            });
        });

        // Connected: the credentials worked. Keep the connecting page up until the
        // remote's first picture, which can take a while (a Windows sign-in, a new
        // session); until then there is nothing to show but black.
        let weak = Rc::downgrade(self);
        self.view.connect_connected(move |_| {
            let Some(s) = weak.upgrade() else { return };
            s.auth_failures.set(0);
            s.after_success();
            if s.stage.borrow().is_some() {
                s.set_stage("Waiting for the remote desktop…");
                s.cancel_reveal_timer();
                let weak = Rc::downgrade(&s);
                let id = glib::timeout_add_local_once(FIRST_FRAME_TIMEOUT, move || {
                    if let Some(s) = weak.upgrade() {
                        s.reveal_timer.take();
                        tracing::info!("no picture from the remote yet; showing the session");
                        s.reveal();
                    }
                });
                *s.reveal_timer.borrow_mut() = Some(id);
            }
        });
        let weak = Rc::downgrade(self);
        self.view.connect_first_frame(move |_| {
            if let Some(s) = weak.upgrade() {
                s.reveal();
            }
        });

        let weak = Rc::downgrade(self);
        let notice_weak = weak.clone();
        self.view.connect_notice(move |_, text| {
            if let Some(s) = notice_weak.upgrade() {
                s.notice(text, None);
            }
        });
        self.view.connect_disconnected(move |_, reason| {
            let Some(s) = weak.upgrade() else { return };
            s.set_loading(false);
            if s.closing.get() {
                return;
            }
            let auth = reason.contains("authentication");
            if auth {
                s.auth_failures.set(s.auth_failures.get() + 1);
                // A saved password that just failed shouldn't be retried silently.
                s.skip_saved_password.set(true);
            }
            s.show_error(reason, auth);
        });
    }

    /// Persist what the user entered once the connection proved it right.
    fn after_success(self: &Rc<Self>) {
        if self.temporary {
            return;
        }
        let profile = self.profile.borrow().clone();
        if let Err(e) = self.state.profiles.save(&profile) {
            tracing::warn!("saving profile: {e}");
        }
        match self.pending_remember.borrow_mut().take() {
            Some(Some(pw)) => {
                let label = format!("Transom: {}", profile.display_name());
                self.state.secret_op(async move {
                    if let Err(e) = secrets::store(&profile.id, &label, &pw).await {
                        tracing::warn!("saving password: {e}");
                    }
                });
            }
            // Logged in with Remember Password off: don't keep an old one either.
            Some(None) => {
                self.state
                    .secret_op(async move { secrets::clear(&profile.id).await });
            }
            None => {}
        }
        self.skip_saved_password.set(false);
    }

    /// Start (or restart) the connection. `password` overrides any saved one.
    /// Something the user should know about this session, as a toast that stays until
    /// dismissed. The window's toast overlay wraps every session, so it names this
    /// one: it may show while another tab is selected. `copy`: a command the toast
    /// offers to copy (a fix the user runs).
    fn notice(&self, text: &str, copy: Option<String>) {
        let Some(toasts) = self
            .view
            .ancestor(adw::ToastOverlay::static_type())
            .and_downcast::<adw::ToastOverlay>()
        else {
            return;
        };
        let text = format!("{}: {text}", self.profile.borrow().display_name());
        let toast = adw::Toast::new(&glib::markup_escape_text(&text));
        toast.set_timeout(0);
        if let Some(command) = copy {
            toast.set_button_label(Some("_Copy Command"));
            // Weak, and the clipboard rather than the view: the toast stays until
            // dismissed, and must keep neither the overlay nor the session alive.
            let (clipboard, overlay) = (self.view.clipboard(), toasts.downgrade());
            toast.connect_button_clicked(move |_| {
                clipboard.set_text(&command);
                if let Some(overlay) = overlay.upgrade() {
                    overlay.add_toast(adw::Toast::new(
                        "Command copied: run it in a terminal, then restart Transom",
                    ));
                }
            });
        }
        toasts.add_toast(toast);
    }

    /// Devices the profile shares that this session can't (a missing folder, or a
    /// permission the Flatpak wasn't granted): the engine leaves them out; say so,
    /// once per session.
    fn report_device_problems(&self) {
        if self.device_problems_shown.replace(true) {
            return;
        }
        let (_, problems) =
            transom_view::core::devices::usable(&self.profile.borrow().redirections);
        for p in problems {
            self.notice(&format!("Not shared: {p}"), p.fix_command());
        }
    }

    pub fn start(self: &Rc<Self>, password: Option<SecretString>) {
        self.show_connecting();
        self.report_device_problems();
        let s = self.clone();
        glib::spawn_future_local(async move {
            let profile = s.profile.borrow().clone();
            let password = match password {
                Some(p) => Some(p),
                None if profile.kerberos => None,
                None if !s.temporary && !s.skip_saved_password.get() => {
                    secrets::lookup(&profile.id).await
                }
                None => None,
            };
            if s.closing.get() {
                return;
            }
            let config = profile.session_config(password);
            if let Err(e) = s.view.connect_session(&s.state.engine, config) {
                s.show_error(&e.to_string(), false);
            }
        });
    }

    /// Show the session in place of the connecting page.
    fn reveal(&self) {
        self.cancel_reveal_timer();
        if self.stage.take().is_none() {
            return;
        }
        // Take keyboard focus only from the connecting page (or from nowhere): while
        // waiting, the user may have moved it to the window controls (Ctrl+Alt+Home)
        // or to another tab.
        let focus = self.view.root().and_then(|r| r.focus());
        let ours = focus.as_ref().is_none_or(|f| {
            f == self.overlay.upcast_ref::<gtk::Widget>() || f.is_ancestor(&self.overlay)
        });
        self.set_loading(false);
        self.overlay.set_visible(false);
        if ours {
            self.view.grab_focus();
        }
    }

    fn set_stage(&self, text: &str) {
        if let Some(label) = self.stage.borrow().as_ref() {
            label.set_label(text);
        }
    }

    fn cancel_reveal_timer(&self) {
        if let Some(id) = self.reveal_timer.take() {
            id.remove();
        }
    }

    fn show_connecting(self: &Rc<Self>) {
        self.set_loading(true);
        self.cancel_reveal_timer();
        let address = self.profile.borrow().address();
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        let stage = gtk::Label::builder()
            .label("Contacting the remote computer…")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .accessible_role(gtk::AccessibleRole::Status)
            .build();
        stage.add_css_class("dim-label");
        let cancel = gtk::Button::with_mnemonic("_Cancel");
        cancel.add_css_class("pill");
        cancel.set_halign(gtk::Align::Center);
        let weak = Rc::downgrade(self);
        cancel.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.drain();
                s.show_error("Connection cancelled", false);
            }
        });
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 24);
        bx.append(&spinner);
        bx.append(&stage);
        bx.append(&cancel);
        let page = adw::StatusPage::builder()
            .title("Connecting…")
            .description(glib::markup_escape_text(&address))
            .child(&bx)
            .build();
        self.overlay.set_child(Some(&page));
        self.overlay.set_visible(true);
        *self.stage.borrow_mut() = Some(stage);
    }

    fn show_error(self: &Rc<Self>, reason: &str, auth: bool) {
        self.set_loading(false);
        self.cancel_reveal_timer();
        self.stage.take();
        let retry = gtk::Button::with_mnemonic(if auth { "_Try Again" } else { "_Reconnect" });
        retry.add_css_class("pill");
        retry.add_css_class("suggested-action");
        let weak = Rc::downgrade(self);
        retry.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.start(None);
            }
        });
        let close = gtk::Button::with_mnemonic("C_lose");
        close.add_css_class("pill");
        let weak = Rc::downgrade(self);
        close.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade()
                && let Some(page) = s.page.borrow().clone()
                && let Some(tv) = page.child().ancestor(adw::TabView::static_type())
            {
                tv.downcast_ref::<adw::TabView>().unwrap().close_page(&page);
            }
        });
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bx.set_halign(gtk::Align::Center);
        bx.append(&retry);
        bx.append(&close);
        let mut text = reason.to_string();
        if let Some(first) = text.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        let page = adw::StatusPage::builder()
            .icon_name(if auth {
                "dialog-password-symbolic"
            } else {
                "network-offline-symbolic"
            })
            .title(if auth {
                "Authentication Failed"
            } else {
                "Disconnected"
            })
            .description(glib::markup_escape_text(&text))
            .child(&bx)
            .build();
        self.overlay.set_child(Some(&page));
        self.overlay.set_visible(true);
        retry.grab_focus();
    }

    /// Disconnect without blocking; the app waits for teardown at exit.
    fn drain(&self) {
        if let Some(f) = self.view.close() {
            self.state.draining.borrow_mut().push(f);
        }
    }

    /// Close for good (tab closed).
    pub fn close(&self) {
        self.closing.set(true);
        self.cancel_reveal_timer();
        self.exit_multimon();
        self.drain();
    }

    /// Multi-monitor fullscreen: span every monitor. `main` is the window holding
    /// this session; it goes fullscreen on its own monitor and the other monitors
    /// get follower windows. Returns false if there's only one monitor.
    pub fn enter_multimon(&self, main: &gtk::Window) -> bool {
        use transom_view::core::monitors::{self, LocalMonitor};
        let display = WidgetExt::display(main);
        let mons: Vec<gtk::gdk::Monitor> = (0..display.monitors().n_items())
            .filter_map(|i| {
                display
                    .monitors()
                    .item(i)
                    .and_downcast::<gtk::gdk::Monitor>()
            })
            .collect();
        if mons.len() < 2 {
            return false;
        }
        let here = main.surface().and_then(|s| display.monitor_at_surface(&s));
        let lead = here
            .as_ref()
            .and_then(|h| mons.iter().position(|m| m == h))
            .unwrap_or(0);
        let local: Vec<LocalMonitor> = mons
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let g = m.geometry();
                LocalMonitor {
                    x: g.x(),
                    y: g.y(),
                    width: g.width(),
                    height: g.height(),
                    scale: m.scale(),
                    primary: i == lead,
                }
            })
            .collect();
        let layout = monitors::layout(&local, self.profile.borrow().scale);
        let (origin, _) = monitors::bounds(&layout);
        let mut followers = Vec::new();
        for (i, (m, rm)) in mons.iter().zip(&layout).enumerate() {
            let r = rm.region(origin);
            let region = Some((r.x, r.y, r.width, r.height));
            if i == lead {
                self.view.set_region(region);
                main.fullscreen_on_monitor(m);
                continue;
            }
            let view = TransomView::new_follower(&self.view);
            view.set_command_as_ctrl(self.state.command_as_ctrl.get());
            view.set_region(region);
            let w = gtk::Window::builder()
                .title(self.profile.borrow().display_name())
                .child(&view)
                .build();
            w.fullscreen_on_monitor(m);
            w.present();
            followers.push(w);
        }
        *self.followers.borrow_mut() = followers;
        tracing::info!(monitors = layout.len(), ?layout, "multi-monitor fullscreen");
        self.view.set_monitor_layout(Some(layout));
        true
    }

    /// Leave multi-monitor mode: close the follower windows, back to one monitor.
    pub fn exit_multimon(&self) {
        for w in self.followers.borrow_mut().drain(..) {
            w.destroy();
        }
        self.view.set_region(None);
        self.view.set_monitor_layout(None);
    }

    pub fn in_multimon(&self) -> bool {
        !self.followers.borrow().is_empty()
    }

    pub fn send_ctrl_alt_del(&self) {
        self.view.send_ctrl_alt_del();
    }

    /// The current remote screen as a texture (for screenshots).
    pub fn screenshot(&self) -> Option<gtk::gdk::Texture> {
        let frame = self.view.frame()?;
        let f = frame.lock();
        let bytes = glib::Bytes::from(&f.pixels[..f.stride * f.size.height as usize]);
        Some(
            gtk::gdk::MemoryTexture::new(
                f.size.width as i32,
                f.size.height as i32,
                gtk::gdk::MemoryFormat::B8g8r8x8,
                &bytes,
                f.stride,
            )
            .upcast(),
        )
    }
}
