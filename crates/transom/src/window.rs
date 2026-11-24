// SPDX-License-Identifier: GPL-3.0-or-later
//! The main window: connection list + session tabs.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use transom_view::TransomView;
use transom_view::core::profile::Profile;
use transom_view::core::rdpfile::RdpFile;
use transom_view::core::secrecy::SecretString;

use crate::app::{AppState, app_shortcuts};
use crate::dialogs::{self, EditorResult};
use crate::session::Session;

type WindowAction = Box<dyn Fn(&Rc<MainWindow>)>;

pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    state: Rc<AppState>,
    toolbar: adw::ToolbarView,
    stack: adw::ViewStack,
    tab_view: adw::TabView,
    tab_bar: adw::TabBar,
    list: gtk::ListBox,
    list_stack: gtk::Stack,
    search: gtk::SearchEntry,
    toasts: adw::ToastOverlay,
    session_controls: gtk::Box,
    add_button: gtk::Button,
    switcher: adw::ViewSwitcher,
    profiles: RefCell<Vec<Profile>>,
    inhibited: std::cell::Cell<bool>,
    /// The open "connect to … ?" confirmation, if any (answered by
    /// `win.confirm-connection` in automated checks).
    pending_confirm: RefCell<Option<adw::AlertDialog>>,
}

impl std::fmt::Debug for MainWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainWindow").finish_non_exhaustive()
    }
}

impl MainWindow {
    pub fn new(app: &adw::Application, state: Rc<AppState>) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Transom")
            .default_width(1100)
            .default_height(720)
            .build();

        // --- connections page ---
        let quick = gtk::Entry::builder()
            .placeholder_text("Connect to an address or rdp:// link")
            .primary_icon_name("network-server-symbolic")
            .secondary_icon_name("go-next-symbolic")
            .secondary_icon_tooltip_text("Connect")
            .hexpand(true)
            .build();
        quick.update_property(&[gtk::accessible::Property::Label("Connect to an address")]);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search connections")
            .build();
        search.update_property(&[gtk::accessible::Property::Label("Search connections")]);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let empty_add = gtk::Button::with_mnemonic("_Add Connection");
        empty_add.add_css_class("pill");
        empty_add.add_css_class("suggested-action");
        empty_add.set_halign(gtk::Align::Center);
        empty_add.set_action_name(Some("win.new-connection"));
        let empty = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .title("No Saved Connections")
            .description("Add a connection, import an .rdp file, or type an address above")
            .child(&empty_add)
            .build();
        // Not homogeneous: the list card hugs its rows instead of taking the empty
        // page's height.
        let list_stack = gtk::Stack::builder().vhomogeneous(false).build();
        list_stack.add_named(&list, Some("list"));
        list_stack.add_named(&empty, Some("empty"));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 18);
        column.set_margin_top(24);
        column.set_margin_bottom(24);
        column.set_margin_start(12);
        column.set_margin_end(12);
        column.append(&quick);
        column.append(&search);
        column.append(&list_stack);
        let clamp = adw::Clamp::builder()
            .maximum_size(640)
            .child(&column)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build();

        // --- sessions page ---
        let tab_view = adw::TabView::new();
        let tab_bar = adw::TabBar::builder()
            .view(&tab_view)
            .autohide(false)
            .build();

        let stack = adw::ViewStack::new();
        let p = stack.add_titled(&scroller, Some("connections"), "Connections");
        p.set_icon_name(Some("view-list-symbolic"));
        let p = stack.add_titled(&tab_view, Some("sessions"), "Sessions");
        p.set_icon_name(Some("computer-symbolic"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        // --- header ---
        let header = adw::HeaderBar::new();
        let switcher = adw::ViewSwitcher::builder()
            .stack(&stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        // "Transom" until there are sessions, then the Connections/Sessions switcher.
        let title_stack = gtk::Stack::new();
        title_stack.add_named(&adw::WindowTitle::new("Transom", ""), Some("title"));
        title_stack.add_named(&switcher, Some("switcher"));
        title_stack.set_hhomogeneous(false);
        header.set_title_widget(Some(&title_stack));
        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.set_tooltip_text(Some("New Connection"));
        add_button.update_property(&[gtk::accessible::Property::Label("New Connection")]);
        add_button.set_action_name(Some("win.new-connection"));
        header.pack_start(&add_button);

        let menu = gio::Menu::new();
        let s1 = gio::Menu::new();
        s1.append(Some("_New Window"), Some("app.new-window"));
        s1.append(Some("_Import .rdp File…"), Some("win.import"));
        menu.append_section(None, &s1);
        let s2 = gio::Menu::new();
        s2.append(Some("_About Transom"), Some("app.about"));
        menu.append_section(None, &s2);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        menu_button.update_property(&[gtk::accessible::Property::Label("Main Menu")]);
        header.pack_end(&menu_button);

        let session_menu = gio::Menu::new();
        session_menu.append(Some("Send Ctrl+Alt+_Del"), Some("win.send-cad"));
        session_menu.append(
            Some("Send _Keyboard Shortcuts to Remote"),
            Some("win.pass-shortcuts"),
        );
        session_menu.append(Some("Save _Screenshot…"), Some("win.screenshot"));
        session_menu.append(Some("_Disconnect"), Some("win.disconnect"));
        let session_menu_button = gtk::MenuButton::builder()
            .icon_name("computer-symbolic")
            .menu_model(&session_menu)
            .tooltip_text("Session")
            .build();
        session_menu_button.update_property(&[gtk::accessible::Property::Label("Session Menu")]);
        let fullscreen = gtk::Button::from_icon_name("view-fullscreen-symbolic");
        fullscreen.set_tooltip_text(Some("Fullscreen (Ctrl+Alt+Enter)"));
        fullscreen.update_property(&[gtk::accessible::Property::Label("Fullscreen")]);
        fullscreen.set_action_name(Some("win.fullscreen"));
        let session_controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        session_controls.append(&fullscreen);
        session_controls.append(&session_menu_button);
        header.pack_end(&session_controls);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&tab_bar);
        toolbar.set_content(Some(&toasts));
        window.set_content(Some(&toolbar));

        let win = Rc::new(Self {
            window,
            state,
            toolbar,
            stack,
            tab_view,
            tab_bar,
            list,
            list_stack,
            search,
            toasts,
            session_controls,
            add_button,
            switcher,
            profiles: RefCell::new(Vec::new()),
            inhibited: std::cell::Cell::new(false),
            pending_confirm: RefCell::new(None),
        });
        win.setup(&quick);
        win.refresh();
        win.update_chrome();
        win
    }

    fn setup(self: &Rc<Self>, quick: &gtk::Entry) {
        let weak = Rc::downgrade(self);
        let go = move |e: &gtk::Entry| {
            if let Some(w) = weak.upgrade() {
                let text = e.text().trim().to_string();
                if !text.is_empty() && w.open_address(&text) {
                    e.set_text("");
                }
            }
        };
        let g = go.clone();
        quick.connect_activate(move |e| g(e));
        quick.connect_icon_release(move |e, pos| {
            if pos == gtk::EntryIconPosition::Secondary {
                go(e);
            }
        });

        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(w) = weak.upgrade() {
                w.list.invalidate_filter();
            }
        });
        let weak = Rc::downgrade(self);
        self.list.set_filter_func(move |row| {
            let Some(w) = weak.upgrade() else { return true };
            let q = w.search.text().to_lowercase();
            if q.is_empty() {
                return true;
            }
            let i = row.index();
            w.profiles.borrow().get(i as usize).is_some_and(|p| {
                [
                    p.display_name(),
                    p.host.clone(),
                    p.username.clone().unwrap_or_default(),
                ]
                .iter()
                .any(|s| s.to_lowercase().contains(&q))
            })
        });

        // Tabs.
        let weak = Rc::downgrade(self);
        self.tab_view.connect_close_page(move |tv, page| {
            if let Some(w) = weak.upgrade()
                && let Some(s) = w.state.take_session(&page.child())
            {
                s.close();
            }
            tv.close_page_finish(page, true);
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_n_pages_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                if w.tab_view.n_pages() == 0 {
                    w.stack.set_visible_child_name("connections");
                    if w.window.is_fullscreen() {
                        w.window.unfullscreen();
                    }
                }
                w.update_chrome();
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_create_window(move |_| {
            let w = weak.upgrade()?;
            let app = w
                .window
                .application()?
                .downcast::<adw::Application>()
                .ok()?;
            let new = crate::app::new_window(&app, &w.state);
            new.stack.set_visible_child_name("sessions");
            new.window.present();
            Some(new.tab_view.clone())
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_selected_page_notify(move |tv| {
            if let Some(page) = tv.selected_page() {
                page.child().child_focus(gtk::DirectionType::TabForward);
            }
            if let Some(w) = weak.upgrade() {
                w.update_chrome();
            }
        });
        let weak = Rc::downgrade(self);
        self.stack.connect_visible_child_name_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                w.update_chrome();
            }
        });

        self.setup_actions();
        self.setup_fullscreen();
        self.setup_keyboard();

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let Some(w) = weak.upgrade() {
                for i in (0..w.tab_view.n_pages()).rev() {
                    let page = w.tab_view.nth_page(i);
                    if let Some(s) = w.state.take_session(&page.child()) {
                        s.close();
                    }
                }
                w.state.forget_window(&w);
            }
            glib::Propagation::Proceed
        });
    }

    fn setup_actions(self: &Rc<Self>) {
        // Plan §6: while a session has focus, app/system shortcuts go to the remote
        // (default) or stay local. Re-evaluated on every focus change.
        let pass = gio::SimpleAction::new_stateful("pass-shortcuts", None, &true.to_variant());
        let weak = Rc::downgrade(self);
        pass.connect_activate(move |a, _| {
            let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(true);
            a.set_state(&on.to_variant());
            if let Some(w) = weak.upgrade() {
                w.window.notify("focus-widget");
                w.toast(if on {
                    "Keyboard shortcuts go to the remote computer"
                } else {
                    "Keyboard shortcuts stay on this computer"
                });
            }
        });
        self.window.add_action(&pass);

        let add = |name: &str, f: WindowAction| {
            let action = gio::SimpleAction::new(name, None);
            let weak: Weak<Self> = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&action);
        };
        add(
            "new-connection",
            Box::new(|w| w.edit_profile(Profile::default(), true)),
        );
        add("import", Box::new(|w| w.import_rdp()));
        add(
            "search",
            Box::new(|w| {
                w.stack.set_visible_child_name("connections");
                w.search.grab_focus();
            }),
        );
        add(
            "fullscreen",
            Box::new(|w| {
                if w.window.is_fullscreen() {
                    if let Some(s) = w.active_session() {
                        s.exit_multimon();
                    }
                    w.window.unfullscreen();
                } else if w.tab_view.n_pages() > 0 {
                    w.stack.set_visible_child_name("sessions");
                    // "Use all monitors": follower windows on the other monitors.
                    let multi = w.active_session().is_some_and(|s| {
                        s.profile.borrow().multimon && s.enter_multimon(w.window.upcast_ref())
                    });
                    if !multi {
                        w.window.fullscreen();
                    }
                }
            }),
        );
        add(
            "send-cad",
            Box::new(|w| {
                if let Some(s) = w.active_session() {
                    s.send_ctrl_alt_del();
                }
            }),
        );
        add(
            "disconnect",
            Box::new(|w| {
                if let Some(page) = w.tab_view.selected_page() {
                    w.tab_view.close_page(&page);
                }
            }),
        );
        add("screenshot", Box::new(|w| w.save_screenshot()));
        // For headless checks (TRANSOM_DEBUG_ACTIONS): accept a pending confirmation
        // with its defaults (nothing extra shared).
        add(
            "confirm-connection",
            Box::new(|w| {
                let pending = w.pending_confirm.borrow_mut().take();
                if let Some(d) = pending {
                    d.emit_by_name::<()>("response", &[&"connect"]);
                    d.force_close();
                }
            }),
        );
        add(
            "focus-chrome",
            Box::new(|w| {
                if w.window.is_fullscreen() {
                    w.toolbar.set_reveal_top_bars(true);
                }
                if w.session_controls.is_visible() {
                    w.session_controls
                        .child_focus(gtk::DirectionType::TabForward);
                } else {
                    w.add_button.grab_focus();
                }
            }),
        );
    }

    /// Hide the header in fullscreen; reveal it, over the session, when the pointer
    /// touches the top edge.
    fn setup_fullscreen(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.connect_fullscreened_notify(move |win| {
            let Some(w) = weak.upgrade() else { return };
            let fs = win.is_fullscreen();
            if !fs
                && let Some(s) = w.active_session()
                && s.in_multimon()
            {
                s.exit_multimon();
            }
            // In fullscreen the bars slide in over the session rather than pushing it
            // down: shrinking the session would resize the remote desktop (a round
            // trip and a full repaint) every time they appear. Raised gives them an
            // opaque background over the remote picture.
            w.toolbar.set_extend_content_to_top_edge(fs);
            w.toolbar.set_top_bar_style(if fs {
                adw::ToolbarStyle::Raised
            } else {
                adw::ToolbarStyle::Flat
            });
            w.toolbar.set_reveal_top_bars(!fs);
            w.update_inhibit();
        });
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, _, y| {
            let Some(w) = weak.upgrade() else { return };
            if !w.window.is_fullscreen() {
                return;
            }
            if y <= 3.0 {
                w.toolbar.set_reveal_top_bars(true);
            } else if w.toolbar.reveals_top_bars() && y > w.toolbar.top_bar_height() as f64 + 24.0 {
                w.toolbar.set_reveal_top_bars(false);
            }
        });
        self.window.add_controller(motion);
    }

    /// While a remote session has keyboard focus, application shortcuts go to the
    /// remote (Ctrl+Q, Ctrl+N… are common there). A few mstsc-style chords stay
    /// local, caught in the capture phase before the session sees them:
    /// Ctrl+Alt+Enter / Ctrl+Alt+Break toggle fullscreen, Ctrl+Alt+End sends
    /// Ctrl+Alt+Del, Ctrl+Alt+Home moves keyboard focus out of the session to the
    /// window controls (the only keyboard way out, since the session takes Tab).
    fn setup_keyboard(self: &Rc<Self>) {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let ctrl_alt = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK;
        for (key, action) in [
            (gdk::Key::Return, "win.fullscreen"),
            (gdk::Key::KP_Enter, "win.fullscreen"),
            (gdk::Key::Pause, "win.fullscreen"),
            (gdk::Key::Break, "win.fullscreen"),
            (gdk::Key::End, "win.send-cad"),
            (gdk::Key::KP_End, "win.send-cad"),
            (gdk::Key::Home, "win.focus-chrome"),
            (gdk::Key::KP_Home, "win.focus-chrome"),
        ] {
            controller.add_shortcut(gtk::Shortcut::new(
                Some(gtk::KeyvalTrigger::new(key, ctrl_alt)),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.window.add_controller(controller);

        let weak = Rc::downgrade(self);
        self.window.connect_focus_widget_notify(move |win| {
            let Some(w) = weak.upgrade() else { return };
            let in_session = GtkWindowExt::focus(win).is_some_and(|f| f.is::<TransomView>());
            let pass = win
                .lookup_action("pass-shortcuts")
                .and_then(|a| a.state())
                .and_then(|v| v.get::<bool>())
                .unwrap_or(true);
            let in_session = in_session && pass;
            if let Some(app) = win.application() {
                for (action, accels) in app_shortcuts() {
                    let a: &[&str] = if in_session { &[] } else { accels };
                    app.set_accels_for_action(action, a);
                }
            }
            w.update_inhibit();
        });
    }

    /// In fullscreen with a session focused, ask the compositor to pass system
    /// shortcuts (Super, Alt+Tab…) to us (GNOME asks the user the first time).
    fn update_inhibit(&self) {
        let pass = self
            .window
            .lookup_action("pass-shortcuts")
            .and_then(|a| a.state())
            .and_then(|v| v.get::<bool>())
            .unwrap_or(true);
        let want = pass
            && self.window.is_fullscreen()
            && GtkWindowExt::focus(&self.window).is_some_and(|f| f.is::<TransomView>());
        if want == self.inhibited.get() {
            return;
        }
        let Some(toplevel) = self
            .window
            .surface()
            .and_then(|s| s.downcast::<gdk::Toplevel>().ok())
        else {
            return;
        };
        if want {
            toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
        } else {
            toplevel.restore_system_shortcuts();
        }
        self.inhibited.set(want);
    }

    fn update_chrome(&self) {
        let has_sessions = self.tab_view.n_pages() > 0;
        let in_sessions = self.stack.visible_child_name().as_deref() == Some("sessions");
        if !has_sessions && in_sessions {
            self.stack.set_visible_child_name("connections");
        }
        if let Some(page) = self.stack.child_by_name("sessions") {
            self.stack.page(&page).set_visible(has_sessions);
        }
        let in_sessions = in_sessions && has_sessions;
        if let Some(ts) = self.switcher.parent().and_downcast::<gtk::Stack>() {
            ts.set_visible_child_name(if has_sessions { "switcher" } else { "title" });
        }
        self.tab_bar.set_visible(in_sessions);
        self.session_controls.set_visible(in_sessions);
        self.add_button.set_visible(!in_sessions);
        for (name, on) in [
            ("fullscreen", has_sessions),
            ("send-cad", in_sessions),
            ("disconnect", in_sessions),
            ("screenshot", in_sessions),
        ] {
            if let Some(a) = self.window.lookup_action(name) {
                a.downcast::<gio::SimpleAction>().unwrap().set_enabled(on);
            }
        }
    }

    pub fn active_session(&self) -> Option<Rc<Session>> {
        self.state
            .session_for(&self.tab_view.selected_page()?.child())
    }

    /// Reload the profile list.
    pub fn refresh(&self) {
        let (profiles, errors) = self.state.profiles.load_all();
        for e in errors {
            tracing::warn!("{e}");
        }
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        for (i, p) in profiles.iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&p.display_name()))
                .activatable(true)
                .build();
            let sub = match &p.username {
                Some(u) => format!("{u} @ {}", p.address()),
                None => p.address(),
            };
            row.set_subtitle(&glib::markup_escape_text(&sub));
            // Decorative: the row title already says what this is.
            row.add_prefix(
                &gtk::Image::builder()
                    .icon_name("computer-symbolic")
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build(),
            );
            let edit = gtk::Button::from_icon_name("document-edit-symbolic");
            edit.update_property(&[gtk::accessible::Property::Label(&format!(
                "Edit {}",
                p.display_name()
            ))]);
            edit.add_css_class("flat");
            edit.set_valign(gtk::Align::Center);
            edit.set_tooltip_text(Some("Edit"));
            let state = self.state.clone();
            let id = p.id.clone();
            edit.connect_clicked(move |b| {
                if let Some(w) = state.window_for(b)
                    && let Some(p) = w.profiles.borrow().iter().find(|p| p.id == id).cloned()
                {
                    w.edit_profile(p, false);
                }
            });
            row.add_suffix(&edit);
            let state = self.state.clone();
            row.connect_activated(move |r| {
                if let Some(w) = state.window_for(r)
                    && let Some(p) = w.profiles.borrow().get(i).cloned()
                {
                    w.open_session(p, false, None);
                }
            });
            self.list.append(&row);
        }
        self.list_stack
            .set_visible_child_name(if profiles.is_empty() { "empty" } else { "list" });
        self.search.set_visible(profiles.len() > 5);
        *self.profiles.borrow_mut() = profiles;
    }

    /// Show `text` (plain text: escaped, since toast titles are Pango markup and
    /// messages often quote user input such as `rdp://…&username=…`).
    pub fn toast(&self, text: &str) {
        self.toasts
            .add_toast(adw::Toast::new(&glib::markup_escape_text(text)));
    }

    /// Quick connect / command line: `host[:port]`, `user@host`, or an `rdp://` URI.
    pub fn open_address(self: &Rc<Self>, text: &str) -> bool {
        let uri = if text.contains("://") {
            text.to_string()
        } else {
            format!("rdp://{text}")
        };
        match RdpFile::from_uri(&uri)
            .ok()
            .and_then(|f| Profile::from_rdp(&f))
        {
            Some(p) => {
                self.open_session(p, true, None);
                true
            }
            None => {
                self.toast(&format!("“{text}” isn't a valid address"));
                false
            }
        }
    }

    /// Open an `.rdp` file and connect (like double-clicking it in a file manager),
    /// after confirmation. `password`: from `--password-file` on the command line.
    pub fn open_rdp_bytes(
        self: &Rc<Self>,
        bytes: &[u8],
        name: &str,
        password: Option<SecretString>,
    ) {
        match RdpFile::parse_bytes(bytes)
            .ok()
            .and_then(|f| Profile::from_rdp(&f))
        {
            Some(mut p) => {
                if p.name.is_empty() {
                    p.name = name.trim_end_matches(".rdp").to_string();
                }
                self.open_untrusted(p, &format!("the file “{name}”"), password);
            }
            None => self.toast(&format!("{name} isn't a valid .rdp file")),
        }
    }

    /// A connection from a file or link: confirm first (see
    /// [`dialogs::confirm_untrusted`]), then connect as a temporary session.
    pub fn open_untrusted(
        self: &Rc<Self>,
        profile: Profile,
        source: &str,
        password: Option<SecretString>,
    ) {
        self.window.present();
        let weak = Rc::downgrade(self);
        let dialog = dialogs::confirm_untrusted(&self.window, profile, source, move |p| {
            let Some(w) = weak.upgrade() else { return };
            w.pending_confirm.borrow_mut().take();
            if let Some(p) = p {
                w.open_session(p, true, password);
            }
        });
        *self.pending_confirm.borrow_mut() = Some(dialog);
    }

    pub fn open_session(
        self: &Rc<Self>,
        profile: Profile,
        temporary: bool,
        password: Option<SecretString>,
    ) {
        let session = Session::new(self.state.clone(), profile, temporary);
        let page = self.tab_view.append(&session.root);
        session.set_page(&page);
        self.state.add_session(session.clone());
        self.tab_view.set_selected_page(&page);
        self.stack.set_visible_child_name("sessions");
        self.update_chrome();
        // Let the view get its real allocation before computing the first request.
        let s = session.clone();
        glib::idle_add_local_once(move || s.start(password));
    }

    fn edit_profile(self: &Rc<Self>, profile: Profile, is_new: bool) {
        let weak = Rc::downgrade(self);
        dialogs::profile_editor(&self.window, profile, is_new, move |result| {
            let Some(w) = weak.upgrade() else { return };
            match result {
                EditorResult::Saved(p) => {
                    if let Err(e) = w.state.profiles.save(&p) {
                        w.toast(&format!("Couldn't save: {e}"));
                    }
                }
                EditorResult::Deleted(id) => {
                    if let Err(e) = w.state.profiles.delete(&id) {
                        w.toast(&format!("Couldn't delete: {e}"));
                    }
                    glib::spawn_future_local(async move { crate::secrets::clear(&id).await });
                }
            }
            w.state.refresh_all();
        });
    }

    fn import_rdp(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Remote Desktop Files"));
        filter.add_suffix("rdp");
        filter.add_mime_type("application/x-rdp");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import Connection")
            .filters(&filters)
            .build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let (Some(w), Ok(file)) = (weak.upgrade(), res) else {
                return;
            };
            let name = file
                .basename()
                .map(|b| b.to_string_lossy().into_owned())
                .unwrap_or_default();
            file.load_contents_async(gio::Cancellable::NONE, move |res| match res {
                Ok((bytes, _)) => {
                    match RdpFile::parse_bytes(&bytes)
                        .ok()
                        .and_then(|f| Profile::from_rdp(&f))
                    {
                        Some(mut p) => {
                            p.name = name.trim_end_matches(".rdp").to_string();
                            w.edit_profile(p, true);
                        }
                        None => w.toast(&format!("{name} isn't a valid .rdp file")),
                    }
                }
                Err(e) => w.toast(&format!("Couldn't read {name}: {e}")),
            });
        });
    }

    fn save_screenshot(self: &Rc<Self>) {
        let Some(tex) = self.active_session().and_then(|s| s.screenshot()) else {
            return;
        };
        let name = self
            .active_session()
            .map(|s| s.profile.borrow().display_name())
            .unwrap_or_default();
        let stamp = glib::DateTime::now_local()
            .and_then(|d| d.format("%Y-%m-%d %H-%M-%S"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        let dialog = gtk::FileDialog::builder()
            .title("Save Screenshot")
            .initial_name(format!("{name} {stamp}.png"))
            .build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let (Some(w), Ok(file)) = (weak.upgrade(), res) else {
                return;
            };
            if let Some(path) = file.path()
                && let Err(e) = tex.save_to_png(&path)
            {
                w.toast(&format!("Couldn't save screenshot: {e}"));
            }
        });
    }
}
