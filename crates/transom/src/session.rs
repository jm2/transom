// SPDX-License-Identifier: GPL-3.0-or-later
//! One remote session: a `TransomView` plus connecting / error overlays.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use transom_view::TransomView;
use transom_view::core::engine::{AuthKind, CertDecision, Credentials};
use transom_view::core::profile::Profile;
use transom_view::core::secrecy::SecretString;
use transom_view::core::trust::TrustStatus;

use crate::app::AppState;
use crate::{dialogs, secrets};

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
    /// Password typed in the auth dialog with "remember" on; stored after success.
    pending_remember: RefCell<Option<SecretString>>,
    auth_failures: Cell<u32>,
    skip_saved_password: Cell<bool>,
    /// Fullscreen windows on the other monitors (multi-monitor mode).
    followers: RefCell<Vec<gtk::Window>>,
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
    pub fn new(state: Rc<AppState>, profile: Profile, temporary: bool) -> Rc<Self> {
        let view = TransomView::new();
        view.update_property(&[gtk::accessible::Property::Label(&format!(
            "Remote desktop: {}",
            profile.display_name()
        ))]);
        view.set_hexpand(true);
        view.set_vexpand(true);
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
                let host = s.profile.borrow().gateway.as_ref().map(|g| g.host.clone());
                format!("the RD Gateway {}", host.unwrap_or_default())
            } else {
                // Name the actual host: a file's name is whatever its sender chose.
                let p = s.profile.borrow();
                let (name, addr) = (p.display_name(), p.address());
                if name == addr || name == p.host {
                    addr
                } else {
                    format!("{name} ({addr})")
                }
            };
            let s2 = s.clone();
            let failed = s.auth_failures.get() > 0;
            // The keyring entry holds the server password; never put a gateway's there.
            let can_remember = !gateway && !s.temporary && secrets::AVAILABLE;
            let (user, domain) = (req.username.clone(), req.domain.clone());
            dialogs::credentials(
                view,
                &target,
                user.as_deref(),
                domain.as_deref(),
                can_remember,
                failed,
                move |entered| match entered {
                    Some(c) => {
                        // Only the latest server answer counts: an earlier "remember"
                        // must not save a password typed later with it turned off. A
                        // gateway answer (never remembered) leaves it alone.
                        if !gateway {
                            *s2.pending_remember.borrow_mut() =
                                c.remember.then(|| c.password.clone());
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
                },
            );
        });

        let weak = Rc::downgrade(self);
        self.view.connect_connected(move |view| {
            let Some(s) = weak.upgrade() else { return };
            s.auth_failures.set(0);
            s.set_loading(false);
            s.overlay.set_visible(false);
            view.grab_focus();
            s.after_success();
        });

        let weak = Rc::downgrade(self);
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
        if let Some(pw) = self.pending_remember.borrow_mut().take() {
            let label = format!("Transom: {}", profile.display_name());
            glib::spawn_future_local(async move {
                if let Err(e) = secrets::store(&profile.id, &label, &pw).await {
                    tracing::warn!("saving password: {e}");
                }
            });
        }
        self.skip_saved_password.set(false);
    }

    /// Start (or restart) the connection. `password` overrides any saved one.
    pub fn start(self: &Rc<Self>, password: Option<SecretString>) {
        self.show_connecting();
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

    fn show_connecting(self: &Rc<Self>) {
        self.set_loading(true);
        let address = self.profile.borrow().address();
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
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
        bx.append(&cancel);
        let page = adw::StatusPage::builder()
            .title("Connecting…")
            .description(glib::markup_escape_text(&address))
            .child(&bx)
            .build();
        self.overlay.set_child(Some(&page));
        self.overlay.set_visible(true);
    }

    fn show_error(self: &Rc<Self>, reason: &str, auth: bool) {
        self.set_loading(false);
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
