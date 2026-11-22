// SPDX-License-Identifier: GPL-3.0-or-later
//! Certificate, credential and profile-editor dialogs.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use transom_view::core::config::GatewayConfig;
use transom_view::core::config::{AudioMode, CertPolicy, ClipboardMode, SecurityMode};
use transom_view::core::engine::{CertDecision, CertPrompt};
use transom_view::core::geometry::{FitMode, ScaleMode};
use transom_view::core::profile::{Gateway, Profile};
use transom_view::core::rdpfile::split_host_port;
use transom_view::core::secrecy::SecretString;
use transom_view::core::trust::TrustStatus;

/// Ask whether to trust a certificate the system CA couldn't validate.
///
/// A *changed* certificate is the dangerous case: the default and close responses
/// cancel, and trusting it is styled destructive.
pub fn certificate(
    parent: &impl IsA<gtk::Widget>,
    prompt: &CertPrompt,
    status: &TrustStatus,
    done: impl FnOnce(CertDecision) + 'static,
) {
    let info = &prompt.info;
    let (heading, body) = match status {
        TrustStatus::Changed { .. } => (
            "Server Identity Changed".to_string(),
            format!(
                "The certificate presented by {} is different from the one you trusted \
                 before. This can happen after a reinstall or certificate renewal, but it \
                 can also mean someone is intercepting the connection.\n\nOnly continue if \
                 you know why it changed.",
                info.host
            ),
        ),
        _ => (
            "Unverified Server".to_string(),
            format!(
                "The identity of {} can't be verified automatically. Compare the \
                 fingerprint below with the one shown on the server (for GNOME Remote \
                 Desktop: Settings › System › Remote Desktop, or grdctl status).",
                info.host
            ),
        ),
    };
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));

    let details = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let mut lines = vec![
        ("Subject", info.subject.clone()),
        ("Issuer", info.issuer.clone()),
        ("SHA-256", info.fingerprint_sha256.clone()),
    ];
    if let TrustStatus::Changed { previous } = status {
        lines.push(("Previously", previous.clone()));
    }
    for (k, v) in lines {
        let label = gtk::Label::builder()
            .label(format!(
                "<b>{k}</b>\n<tt>{}</tt>",
                glib::markup_escape_text(&v)
            ))
            .use_markup(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::Char)
            .selectable(true)
            .xalign(0.0)
            .build();
        details.append(&label);
    }
    dialog.set_extra_child(Some(&details));

    dialog.add_response("cancel", "_Cancel");
    dialog.add_response("once", "Connect _Once");
    dialog.add_response("trust", "_Trust and Connect");
    dialog.set_close_response("cancel");
    match status {
        TrustStatus::Changed { .. } => {
            dialog.set_response_appearance("trust", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
        }
        _ => {
            dialog.set_response_appearance("trust", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("trust"));
        }
    }
    let done = RefCell::new(Some(done));
    dialog.connect_response(None, move |_, response| {
        let d = match response {
            "trust" => CertDecision::AcceptAndRemember,
            "once" => CertDecision::AcceptOnce,
            _ => CertDecision::Reject,
        };
        if let Some(f) = done.borrow_mut().take() {
            f(d);
        }
    });
    dialog.present(Some(parent));
}

/// Entered credentials.
#[derive(Debug)]
pub struct EnteredCredentials {
    pub username: String,
    pub domain: Option<String>,
    pub password: SecretString,
    pub remember: bool,
}

/// Ask for a user name and password. `None` = cancelled.
pub fn credentials(
    parent: &impl IsA<gtk::Widget>,
    target: &str,
    username: Option<&str>,
    domain: Option<&str>,
    can_remember: bool,
    failed_before: bool,
    done: impl FnOnce(Option<EnteredCredentials>) + 'static,
) {
    let body = if failed_before {
        format!("The user name or password for {target} wasn't accepted. Try again.")
    } else {
        format!("Log in to {target}.")
    };
    let dialog = adw::AlertDialog::new(Some("Authentication Required"), Some(&body));
    let group = adw::PreferencesGroup::new();
    let user = adw::EntryRow::builder()
        .title("User Name")
        .text(username.unwrap_or_default())
        .build();
    let dom = adw::EntryRow::builder()
        .title("Domain (optional)")
        .text(domain.unwrap_or_default())
        .build();
    let pass = adw::PasswordEntryRow::builder().title("Password").build();
    let remember = adw::SwitchRow::builder()
        .title("Remember Password")
        .active(can_remember)
        .sensitive(can_remember)
        .build();
    group.add(&user);
    group.add(&dom);
    group.add(&pass);
    if can_remember {
        group.add(&remember);
    }
    dialog.set_extra_child(Some(&group));
    dialog.add_response("cancel", "_Cancel");
    dialog.add_response("login", "_Log In");
    dialog.set_response_appearance("login", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("login"));
    dialog.set_close_response("cancel");
    // Enter in the password field activates the default response ("login").
    pass.set_activates_default(true);
    if username.is_some_and(|u| !u.is_empty()) {
        pass.grab_focus();
    } else {
        user.grab_focus();
    }
    let done = RefCell::new(Some(done));
    dialog.connect_response(None, move |_, response| {
        let result = (response == "login").then(|| EnteredCredentials {
            username: user.text().to_string(),
            domain: Some(dom.text().to_string()).filter(|d| !d.is_empty()),
            password: SecretString::from(pass.text().to_string()),
            remember: can_remember && remember.is_active(),
        });
        if let Some(f) = done.borrow_mut().take() {
            f(result);
        }
    });
    dialog.present(Some(parent));
}

/// Profile editor outcome.
pub enum EditorResult {
    Saved(Box<Profile>),
    Deleted(String),
}

const SCALE_CHOICES: &[(&str, ScaleMode)] = &[
    ("Match This Display (Sharpest)", ScaleMode::MatchLocal),
    ("Fixed 100%", ScaleMode::Fixed(100)),
    ("Fixed 125%", ScaleMode::Fixed(125)),
    ("Fixed 150%", ScaleMode::Fixed(150)),
    ("Fixed 200%", ScaleMode::Fixed(200)),
    ("Legacy (Stretch 100%)", ScaleMode::Legacy100),
];
const CLIPBOARD_CHOICES: &[(&str, ClipboardMode)] = &[
    ("Both Directions", ClipboardMode::Both),
    ("Only to Remote", ClipboardMode::ToRemote),
    ("Only from Remote", ClipboardMode::FromRemote),
    ("Off", ClipboardMode::Off),
];
const AUDIO_CHOICES: &[(&str, AudioMode)] = &[
    ("Play on This Computer", AudioMode::Local),
    ("Play on Remote Computer", AudioMode::Remote),
    ("Off", AudioMode::Off),
];
const SECURITY_CHOICES: &[(&str, SecurityMode)] = &[
    ("Automatic", SecurityMode::Negotiate),
    ("NLA Only", SecurityMode::Nla),
    ("TLS Only", SecurityMode::Tls),
    ("Allow Legacy RDP Security", SecurityMode::Rdp),
];

fn combo<T: PartialEq + Copy>(title: &str, choices: &[(&str, T)], current: T) -> adw::ComboRow {
    let model = gtk::StringList::new(&choices.iter().map(|(l, _)| *l).collect::<Vec<_>>());
    let row = adw::ComboRow::builder().title(title).model(&model).build();
    row.set_selected(choices.iter().position(|(_, v)| *v == current).unwrap_or(0) as u32);
    row
}

/// Edit (or create) a profile.
pub fn profile_editor(
    parent: &impl IsA<gtk::Widget>,
    profile: Profile,
    is_new: bool,
    done: impl Fn(EditorResult) + 'static,
) {
    let dialog = adw::Dialog::builder()
        .title(if is_new {
            "New Connection"
        } else {
            "Edit Connection"
        })
        .content_width(460)
        .build();
    let page = adw::PreferencesPage::new();

    let general = adw::PreferencesGroup::new();
    let name = adw::EntryRow::builder()
        .title("Name")
        .text(&profile.name)
        .build();
    let host = adw::EntryRow::builder()
        .title("Host")
        .text(&profile.host)
        .build();
    let port = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    port.set_title("Port");
    port.set_value(profile.port as f64);
    let user = adw::EntryRow::builder()
        .title("User Name")
        .text(profile.username.clone().unwrap_or_default())
        .build();
    let domain = adw::EntryRow::builder()
        .title("Domain")
        .text(profile.domain.clone().unwrap_or_default())
        .build();
    for w in [&name, &host] {
        general.add(w);
    }
    general.add(&port);
    general.add(&user);
    general.add(&domain);
    page.add(&general);

    let display = adw::PreferencesGroup::builder().title("Display").build();
    let scale = combo("Scaling", SCALE_CHOICES, profile.scale);
    let fit = adw::SwitchRow::builder()
        .title("Scale to Window")
        .subtitle("When the remote desktop can't be resized, shrink it to fit instead of scrolling")
        .active(profile.fit == FitMode::Fit)
        .build();
    display.add(&scale);
    display.add(&fit);
    let multimon = adw::SwitchRow::builder()
        .title("Use All Monitors in Fullscreen")
        .subtitle("Each monitor shows its part of one remote desktop, at its own scale")
        .active(profile.multimon)
        .build();
    display.add(&multimon);
    page.add(&display);

    let devices = adw::PreferencesGroup::builder().title("Sharing").build();
    let clip = combo("Clipboard", CLIPBOARD_CHOICES, profile.clipboard);
    devices.add(&clip);
    let audio = combo("Sound", AUDIO_CHOICES, profile.audio);
    devices.add(&audio);
    let mic = adw::SwitchRow::builder()
        .title("Microphone")
        .subtitle("Let the remote computer use this computer's microphone")
        .active(profile.microphone)
        .build();
    devices.add(&mic);
    page.add(&devices);
    let (local_devices, redirections) = crate::devices_ui::group(&profile.redirections);
    page.add(&local_devices);

    let security = adw::PreferencesGroup::builder().title("Security").build();
    let sec = combo("Protocol Security", SECURITY_CHOICES, profile.security);
    let ignore_cert = adw::SwitchRow::builder()
        .title("Ignore Certificate Problems")
        .subtitle("Insecure: anyone on the network could impersonate this server")
        .active(profile.certificate == CertPolicy::Ignore)
        .build();
    ignore_cert.add_css_class("warning");
    let kerberos = adw::SwitchRow::builder()
        .title("Use Kerberos Ticket")
        .subtitle("Single sign-on with your Kerberos login (kinit); no password needed")
        .active(profile.kerberos)
        .build();
    security.add(&sec);
    security.add(&kerberos);
    security.add(&ignore_cert);
    page.add(&security);

    let gateway = adw::PreferencesGroup::builder()
        .title("RD Gateway")
        .description("Connect through a Remote Desktop Gateway (HTTPS)")
        .build();
    let gw_host = adw::EntryRow::builder()
        .title("Gateway Address (optional)")
        .text(
            profile
                .gateway
                .as_ref()
                .map(gateway_address)
                .unwrap_or_default(),
        )
        .build();
    let gw_same = adw::SwitchRow::builder()
        .title("Use the Same Credentials")
        .subtitle("Log in to the gateway with this connection's user name and password")
        .active(profile.gateway.as_ref().is_none_or(|g| g.same_credentials))
        .build();
    gateway.add(&gw_host);
    gateway.add(&gw_same);
    page.add(&gateway);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    buttons.set_halign(gtk::Align::Center);
    buttons.set_margin_bottom(12);
    let save = gtk::Button::with_mnemonic(if is_new { "_Add" } else { "_Save" });
    save.add_css_class("suggested-action");
    save.add_css_class("pill");
    buttons.append(&save);
    let export = gtk::Button::with_mnemonic("_Export…");
    export.add_css_class("pill");
    export.set_tooltip_text(Some("Save as an .rdp file"));
    if !is_new {
        buttons.append(&export);
    }
    let delete = gtk::Button::with_mnemonic("_Delete");
    delete.add_css_class("destructive-action");
    delete.add_css_class("pill");
    if !is_new {
        buttons.append(&delete);
    }
    let footer = adw::PreferencesGroup::new();
    footer.add(&buttons);
    page.add(&footer);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    let validate = glib::clone!(
        #[weak]
        save,
        #[weak]
        host,
        move || save.set_sensitive(!host.text().trim().is_empty())
    );
    validate();
    host.connect_changed(move |_| validate());

    let done = Rc::new(done);
    let d = done.clone();
    let original = profile.clone();
    save.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let opt =
                |e: &adw::EntryRow| Some(e.text().trim().to_string()).filter(|s| !s.is_empty());
            let mut p = original.clone();
            p.name = name.text().trim().to_string();
            p.host = host
                .text()
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string();
            p.port = port.value() as u16;
            p.username = opt(&user);
            p.domain = opt(&domain);
            p.scale = SCALE_CHOICES[scale.selected() as usize % SCALE_CHOICES.len()].1;
            p.multimon = multimon.is_active();
            p.kerberos = kerberos.is_active();
            p.fit = if fit.is_active() {
                FitMode::Fit
            } else {
                FitMode::Scroll
            };
            p.clipboard = CLIPBOARD_CHOICES[clip.selected() as usize % CLIPBOARD_CHOICES.len()].1;
            p.audio = AUDIO_CHOICES[audio.selected() as usize % AUDIO_CHOICES.len()].1;
            p.microphone = mic.is_active();
            p.redirections = redirections.borrow().clone();
            p.gateway = split_host_port(&gw_host.text()).map(|(host, port)| Gateway {
                host,
                port: port.filter(|&p| p != GatewayConfig::DEFAULT_PORT),
                username: original.gateway.as_ref().and_then(|g| g.username.clone()),
                same_credentials: gw_same.is_active(),
            });
            p.security = SECURITY_CHOICES[sec.selected() as usize % SECURITY_CHOICES.len()].1;
            p.certificate = if ignore_cert.is_active() {
                CertPolicy::Ignore
            } else {
                CertPolicy::Verify
            };
            d(EditorResult::Saved(Box::new(p)));
            dialog.close();
        }
    ));
    let export_profile = profile.clone();
    export.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let p = export_profile.clone();
            let file_dialog = gtk::FileDialog::builder()
                .title("Export Connection")
                .initial_name(format!("{}.rdp", p.display_name()))
                .build();
            let parent = dialog.root().and_downcast::<gtk::Window>();
            file_dialog.save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res
                    && let Some(path) = file.path()
                    && let Err(e) = std::fs::write(&path, p.to_rdp().to_utf16le_bytes())
                {
                    tracing::warn!("exporting {}: {e}", path.display());
                }
            });
        }
    ));
    let id = profile.id.clone();
    delete.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            done(EditorResult::Deleted(id.clone()));
            dialog.close();
        }
    ));
    dialog.present(Some(parent));
}

/// `host` or `host:port` (port shown only when not 443; IPv6 literals bracketed).
fn gateway_address(g: &Gateway) -> String {
    let host = if g.host.contains(':') {
        format!("[{}]", g.host)
    } else {
        g.host.clone()
    };
    match g.port {
        Some(p) if p != GatewayConfig::DEFAULT_PORT => format!("{host}:{p}"),
        _ => host,
    }
}
