// SPDX-License-Identifier: Apache-2.0
//! `transom-view-demo HOST[:PORT] USER [options]`: a window with one `TransomView`.
//!
//! Options:
//!   --domain D            NLA domain
//!   --password-file F     read the password from F (else you'll be prompted)
//!   --trust SHA256        accept a certificate with this fingerprint without asking
//!   --scale-mode M        match (default) | legacy | fixed:PERCENT
//!   --fit M               fit (default) | scroll
//!   --size WxH            initial window size in logical pixels (default 1280x800)
//!   --check               after the screen settles, render the widget at device
//!                         scale, compare it with the remote framebuffer, print
//!                         the result and exit (0 = pixel-exact)
//!   --screenshot F.png    with --check: also save the rendered widget
//!   --resize-to WxH       resize the window 4 s after connecting (tests renegotiation)
//!   --check-after-ms N    start checking N ms after connecting (default 3000)
//!   --check-psnr DB       pass the check at PSNR >= DB instead of bit-exact (for
//!                         hardware-decoded video, whose YCbCr conversion is the GPU's)
//!   --video-decode M      auto | software | verify (ADR 0006)
//!   --quit-after-ms N     disconnect cleanly and quit N ms after connecting
//!   --multimon            one fullscreen window per monitor (multi-monitor layout);
//!                         with --check, every monitor's window is checked
//!   --file-clip-test      copy local test files 6 s after connecting and print
//!                         SHA-256 of files arriving on the local clipboard
//!   --clip-test           put test text on the local clipboard 6 s after connecting
//!                         and log every local clipboard change (text / image size)

use std::cell::{Cell, RefCell};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib, graphene};
use transom_view::TransomView;
use transom_view::core::config::{CertPolicy, DEFAULT_PORT, SecurityMode, SessionConfig};
use transom_view::core::engine::{CertDecision, Credentials};
use transom_view::core::geometry::{FitMode, ScaleMode, desktop_request};
use transom_view::core::secrecy::SecretString;
use transom_view::core::trust::normalize_fingerprint;
use transom_view_freerdp::FreeRdpEngine;

#[derive(Clone, Debug)]
struct Args {
    host: String,
    port: u16,
    user: String,
    domain: Option<String>,
    password_file: Option<String>,
    trust: Option<String>,
    scale_mode: ScaleMode,
    fit: FitMode,
    size: (i32, i32),
    check: bool,
    screenshot: Option<String>,
    resize_to: Option<(i32, i32)>,
    check_after_ms: u32,
    check_after_explicit: bool,
    check_psnr: Option<f64>,
    video_decode: transom_view_core::video::VideoDecode,
    redirections: transom_view_core::devices::Redirections,
    quit_after_ms: Option<u64>,
    clip_test: bool,
    file_clip_test: bool,
    multimon: bool,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let target = it.next().ok_or("missing HOST")?;
    let user = it.next().ok_or("missing USER")?;
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().map_err(|_| "bad port")?),
        _ => (target, DEFAULT_PORT),
    };
    let mut a = Args {
        host,
        port,
        user,
        domain: None,
        password_file: None,
        trust: None,
        scale_mode: ScaleMode::MatchLocal,
        fit: FitMode::Fit,
        size: (1280, 800),
        check: false,
        screenshot: None,
        resize_to: None,
        check_after_ms: 3000,
        check_after_explicit: false,
        check_psnr: None,
        video_decode: Default::default(),
        redirections: Default::default(),
        quit_after_ms: None,
        clip_test: false,
        file_clip_test: false,
        multimon: false,
    };
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--domain" => a.domain = Some(val()?),
            "--password-file" => a.password_file = Some(val()?),
            "--trust" => a.trust = Some(normalize_fingerprint(&val()?).ok_or("bad fingerprint")?),
            "--scale-mode" => {
                a.scale_mode = match val()?.as_str() {
                    "match" => ScaleMode::MatchLocal,
                    "legacy" => ScaleMode::Legacy100,
                    v => match v.strip_prefix("fixed:") {
                        Some(p) => ScaleMode::Fixed(p.parse().map_err(|_| "bad percent")?),
                        None => return Err("scale mode: match | legacy | fixed:N".into()),
                    },
                }
            }
            "--fit" => {
                a.fit = match val()?.as_str() {
                    "fit" => FitMode::Fit,
                    "scroll" => FitMode::Scroll,
                    _ => return Err("fit: fit | scroll".into()),
                }
            }
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                a.size = (
                    w.parse().map_err(|_| "bad w")?,
                    h.parse().map_err(|_| "bad h")?,
                );
            }
            "--check" => a.check = true,
            "--resize-to" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--resize-to WxH")?;
                a.resize_to = Some((
                    w.parse().map_err(|_| "bad w")?,
                    h.parse().map_err(|_| "bad h")?,
                ));
            }
            "--check-after-ms" => {
                a.check_after_ms = val()?.parse().map_err(|_| "bad ms")?;
                a.check_after_explicit = true;
            }
            "--check-psnr" => a.check_psnr = Some(val()?.parse().map_err(|_| "bad dB")?),
            "--drive" => {
                // NAME=PATH
                let v = val()?;
                let (n, p) = v.split_once('=').ok_or("--drive NAME=PATH")?;
                a.redirections
                    .drives
                    .push(transom_view_core::devices::DriveShare {
                        name: transom_view_core::devices::drive_name(n),
                        path: p.into(),
                    });
            }
            "--serial" => {
                // COMn=DEVICE
                let v = val()?;
                let (n, p) = v.split_once('=').ok_or("--serial COMn=DEVICE")?;
                a.redirections
                    .serial_ports
                    .push(transom_view_core::devices::SerialPort {
                        name: n.into(),
                        path: p.into(),
                    });
            }
            "--smartcards" => a.redirections.smartcards = true,
            "--printers" => a.redirections.printers = true,
            "--usb" => {
                // VID:PID (hex)
                let v = val()?;
                let (vi, pi) = v.split_once(':').ok_or("--usb VID:PID")?;
                let hex = |s: &str| u16::from_str_radix(s, 16).map_err(|_| "--usb VID:PID (hex)");
                a.redirections
                    .usb
                    .push(transom_view_core::devices::UsbSelector::VidPid {
                        vid: hex(vi)?,
                        pid: hex(pi)?,
                    });
            }
            "--video-decode" => {
                use transom_view_core::video::VideoDecode;
                a.video_decode = match val()?.as_str() {
                    "auto" => VideoDecode::Auto,
                    "software" => VideoDecode::Software,
                    "verify" => VideoDecode::Verify,
                    _ => return Err("--video-decode auto|software|verify".into()),
                }
            }
            "--clip-test" => a.clip_test = true,
            "--file-clip-test" => a.file_clip_test = true,
            "--multimon" => a.multimon = true,
            "--quit-after-ms" => a.quit_after_ms = Some(val()?.parse().map_err(|_| "bad ms")?),
            "--screenshot" => a.screenshot = Some(val()?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    // The resize happens 4 s after connecting: never check before it has settled.
    if a.resize_to.is_some() && !a.check_after_explicit {
        a.check_after_ms = a.check_after_ms.max(9000);
    }
    Ok(a)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: {e}\nusage: transom-view-demo HOST[:PORT] USER [--password-file F] [--trust SHA256] [--scale-mode M] [--fit M] [--size WxH] [--check [--screenshot F.png]]"
            );
            return ExitCode::from(2);
        }
    };
    let _ = CHECK_PSNR.set(args.check_psnr);
    let app = gtk::Application::builder()
        .application_id("io.github.jm2.Transom.ViewDemo")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let code = Rc::new(Cell::new(0u8));
    let c = code.clone();
    app.connect_activate(move |app| build(app, args.clone(), c.clone()));
    app.run_with_args::<&str>(&[]);
    ExitCode::from(code.get())
}

fn build(app: &gtk::Application, args: Args, code: Rc<Cell<u8>>) {
    if args.multimon {
        build_multimon(app, args, code);
        return;
    }
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(format!("{} — transom-view-demo", args.host))
        .default_width(args.size.0)
        .default_height(args.size.1)
        .build();
    let view = TransomView::new();
    window.set_child(Some(&view));

    let trust = args.trust.clone();
    view.set_cert_handler(move |view, prompt| {
        let fp = prompt.info.fingerprint_sha256.clone();
        if trust.as_deref() == Some(fp.as_str()) {
            let _ = prompt.reply.send(CertDecision::AcceptOnce);
            return;
        }
        let changed = prompt
            .previous_fingerprint
            .as_deref()
            .map(|p| format!("\n\nWARNING: this host previously presented\n{p}"))
            .unwrap_or_default();
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!("Trust the certificate of {}?", prompt.info.host))
            .detail(format!(
                "Subject: {}\nIssuer: {}\nSHA-256:\n{fp}{changed}",
                prompt.info.subject, prompt.info.issuer
            ))
            .buttons(["Reject", "Accept Once"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let window = view.root().and_downcast::<gtk::Window>();
        dialog.choose(window.as_ref(), gtk::gio::Cancellable::NONE, move |r| {
            let d = if r == Ok(1) {
                CertDecision::AcceptOnce
            } else {
                CertDecision::Reject
            };
            let _ = prompt.reply.send(d);
        });
    });

    view.set_auth_handler(|view, req| {
        let window = view.root().and_downcast::<gtk::Window>();
        prompt_password(window.as_ref(), req);
    });

    let password = match &args.password_file {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => Some(SecretString::from(
                s.lines().next().unwrap_or("").to_string(),
            )),
            Err(e) => {
                eprintln!("{p}: {e}");
                code.set(2);
                app.quit();
                return;
            }
        },
        None => None,
    };
    let config = SessionConfig {
        host: args.host.clone(),
        port: args.port,
        username: Some(args.user.clone()),
        domain: args.domain.clone(),
        password,
        security: SecurityMode::Negotiate,
        cert_policy: CertPolicy::Verify,
        scale_mode: args.scale_mode,
        fit_mode: args.fit,
        clipboard: transom_view::core::config::ClipboardMode::Both,
        audio: transom_view::core::config::AudioMode::Off,
        microphone: false,
        kerberos: false,
        // Replaced by the view with a request computed from its size and scale.
        initial: desktop_request(args.scale_mode, Default::default(), 1.0),
        monitors: Vec::new(),
        gateway: None,
        video_decode: args.video_decode,
        redirections: args.redirections.clone(),
    };

    let app_weak = app.downgrade();
    let c = code.clone();
    view.connect_disconnected(move |_, reason| {
        eprintln!("disconnected: {reason}");
        if c.get() == 0 {
            c.set(1);
        }
        if let Some(app) = app_weak.upgrade() {
            app.quit();
        }
    });
    if args.file_clip_test {
        install_file_clip_test(&view);
    }
    if args.clip_test {
        let cb = view.clipboard();
        cb.connect_changed(|cb| {
            let formats = cb.formats().to_str().to_string();
            cb.read_text_async(gtk::gio::Cancellable::NONE, move |r| {
                if let Ok(Some(t)) = r {
                    println!("LOCAL CLIPBOARD TEXT: {t:?} [{formats}]");
                }
            });
            if cb.formats().contains_type(gdk::Texture::static_type()) {
                cb.read_texture_async(gtk::gio::Cancellable::NONE, |r| {
                    if let Ok(Some(t)) = r {
                        println!("LOCAL CLIPBOARD IMAGE: {}x{}", t.width(), t.height());
                    }
                });
            }
        });
        view.connect_connected(|v| {
            let v2 = v.clone();
            glib::timeout_add_local_once(Duration::from_secs(6), move || {
                v2.clipboard().set_text("from the local side ✓");
                println!("set local clipboard");
            });
            let v3 = v.clone();
            glib::timeout_add_local_once(Duration::from_secs(18), move || {
                let rgba: Vec<u8> = (0..30u32)
                    .flat_map(|y| {
                        (0..40u32).flat_map(move |x| [(x * 6) as u8, (y * 8) as u8, 60, 255])
                    })
                    .collect();
                let tex = gdk::MemoryTexture::new(
                    40,
                    30,
                    gdk::MemoryFormat::R8g8b8a8,
                    &glib::Bytes::from_owned(rgba),
                    160,
                );
                v3.clipboard().set_texture(&tex);
                println!("set local clipboard image");
            });
        });
    }
    if let Some(ms) = args.quit_after_ms {
        let app = app.clone();
        view.connect_connected(move |v| {
            let (v, app) = (v.clone(), app.clone());
            glib::timeout_add_local_once(Duration::from_millis(ms), move || {
                v.shutdown(Duration::from_secs(5));
                app.quit();
            });
        });
    }
    view.connect_connected(|v| {
        v.grab_focus();
        let s = v
            .native()
            .and_then(|n| n.surface())
            .map(|s| s.scale())
            .unwrap_or(1.0);
        let (w, h) = v.desktop_size().unwrap_or_default();
        eprintln!(
            "connected: desktop {w}x{h}, widget {}x{} @ {s}",
            v.width(),
            v.height()
        );
    });
    view.connect_desktop_resized(|v, w, h| {
        let s = v
            .native()
            .and_then(|n| n.surface())
            .map(|s| s.scale())
            .unwrap_or(1.0);
        eprintln!(
            "desktop resized: {w}x{h} (widget {}x{} @ {s})",
            v.width(),
            v.height()
        );
    });

    let v = view.clone();
    window.connect_close_request(move |_| {
        v.shutdown(Duration::from_secs(5));
        glib::Propagation::Proceed
    });
    window.present();

    // Connect once the window is mapped, so the first request uses the real size
    // and surface scale.
    let engine = FreeRdpEngine::new(glib::user_cache_dir().join("transom-view-demo/freerdp"));
    let started = Rc::new(Cell::new(false));
    let v = view.clone();
    let s = started.clone();
    view.add_tick_callback(move |_, _| {
        if !s.replace(true) {
            eprintln!(
                "FreeRDP {}; widget {}x{} @ {}",
                FreeRdpEngine::freerdp_version(),
                v.width(),
                v.height(),
                v.native()
                    .and_then(|n| n.surface())
                    .map(|s| s.scale())
                    .unwrap_or(1.0)
            );
            if let Err(e) = v.connect_session(&engine, config.clone()) {
                eprintln!("connect failed: {e}");
            }
        }
        glib::ControlFlow::Break
    });

    if let Some((w, h)) = args.resize_to {
        let win = window.clone();
        view.connect_connected(move |_| {
            let win = win.clone();
            glib::timeout_add_local_once(Duration::from_secs(4), move || {
                eprintln!("resizing window to {w}x{h}");
                win.set_default_size(w, h);
            });
        });
    }
    if args.check {
        install_check(
            &view,
            args.screenshot.clone(),
            args.check_after_ms,
            code,
            app.clone(),
        );
    }
}

fn prompt_password(window: Option<&gtk::Window>, req: transom_view::core::engine::AuthRequest) {
    let dialog = gtk::Window::builder()
        .title("Remote desktop login")
        .modal(true)
        .resizable(false)
        .build();
    dialog.set_transient_for(window);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
    bx.set_margin_top(18);
    bx.set_margin_bottom(18);
    bx.set_margin_start(18);
    bx.set_margin_end(18);
    let user = gtk::Entry::builder()
        .placeholder_text("User name")
        .text(req.username.clone().unwrap_or_default())
        .build();
    let pass = gtk::PasswordEntry::builder()
        .placeholder_text("Password")
        .show_peek_icon(true)
        .activates_default(true)
        .build();
    let ok = gtk::Button::with_label("Log In");
    ok.add_css_class("suggested-action");
    bx.append(&user);
    bx.append(&pass);
    bx.append(&ok);
    dialog.set_child(Some(&bx));
    dialog.set_default_widget(Some(&ok));
    let req = Rc::new(RefCell::new(Some(req)));
    let d = dialog.clone();
    let r = req.clone();
    ok.connect_clicked(move |_| {
        if let Some(req) = r.borrow_mut().take() {
            let _ = req.reply.send(Some(Credentials {
                username: user.text().to_string(),
                domain: req.domain.clone(),
                password: SecretString::from(pass.text().to_string()),
            }));
        }
        d.close();
    });
    // Closing the window drops the request, which cancels.
    dialog.connect_close_request(move |_| {
        req.borrow_mut().take();
        glib::Propagation::Proceed
    });
    dialog.present();
}

/// `--check`: wait for the screen to settle, then render the widget at device
/// scale through GSK and compare it with the framebuffer.
fn install_check(
    view: &TransomView,
    screenshot: Option<String>,
    after_ms: u32,
    code: Rc<Cell<u8>>,
    app: gtk::Application,
) {
    let start = after_ms.div_ceil(250).max(1);
    let quiet = Rc::new(Cell::new(0u32));
    let q = quiet.clone();
    view.connect_connected(move |_| q.set(0));
    let v = view.clone();
    glib::timeout_add_local(Duration::from_millis(250), move || {
        if !v.is_connected() {
            return glib::ControlFlow::Continue;
        }
        let n = quiet.get() + 1;
        quiet.set(n);
        // Give resize negotiation time to finish.
        if n < start {
            return glib::ControlFlow::Continue;
        }
        // Every second from then on: the remote may still be refining the image
        // (progressive codecs) for a while after a resize.
        if !(n - start).is_multiple_of(4) {
            return glib::ControlFlow::Continue;
        }
        let ok = run_check(&v, screenshot.as_deref());
        if !ok && std::env::var_os("TRANSOM_CHECK_INVALIDATE").is_some() {
            println!("check: invalidating tiles and re-checking");
            v.invalidate();
            run_check(&v, None);
        }
        if !ok && n < start + 4 * 2 {
            return glib::ControlFlow::Continue;
        }
        code.set(if ok { 0 } else { 3 });
        if !v.shutdown(Duration::from_secs(5)) {
            eprintln!("warning: engine did not shut down within 5 s");
        }
        app.quit();
        glib::ControlFlow::Break
    });
}

/// `--check-psnr`, set once in `main`.
static CHECK_PSNR: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();

fn run_check(view: &TransomView, screenshot: Option<&str>) -> bool {
    run_check_region(view, screenshot, None)
}

/// Compare a view's rendering with `region` (x, y, w, h) of the framebuffer, or all
/// of it.
fn run_check_region(
    view: &TransomView,
    screenshot: Option<&str>,
    region: Option<(u32, u32, u32, u32)>,
) -> bool {
    let Some(native) = view.native() else {
        return false;
    };
    let Some(renderer) = native.renderer() else {
        return false;
    };
    let s = native.surface().map(|s| s.scale()).unwrap_or(1.0);
    let Some(frame) = view.frame() else {
        return false;
    };

    // Render the whole window (so origin snapping is exercised exactly as on screen)
    // at device resolution.
    // Render in *surface* coordinates (the window content sits at the CSD shadow
    // offset), so the device-pixel grid matches the real surface exactly.
    let root = native.upcast_ref::<gtk::Widget>();
    let (sx, sy) = native.surface_transform();
    let (rw, rh) = (root.width() as f64, root.height() as f64);
    let paintable = gtk::WidgetPaintable::new(Some(root));
    let snap = gtk::Snapshot::new();
    snap.scale(s as f32, s as f32);
    snap.translate(&graphene::Point::new(sx as f32, sy as f32));
    paintable.snapshot(&snap, rw, rh);
    let Some(node) = snap.to_node() else {
        return false;
    };
    let (dw, dh) = (((rw + sx) * s).ceil(), ((rh + sy) * s).ceil());
    let tex = renderer.render_texture(
        &node,
        Some(&graphene::Rect::new(0.0, 0.0, dw as f32, dh as f32)),
    );
    let mut dl = gdk::TextureDownloader::new(&tex);
    dl.set_format(gdk::MemoryFormat::B8g8r8a8);
    let (bytes, stride) = dl.download_bytes();
    let (tw, th) = (tex.width() as usize, tex.height() as usize);

    if let Some(path) = screenshot {
        let _ = tex.save_to_png(path);
        eprintln!("saved {path}");
    }

    // Where should the frame be? The view's top-left in device pixels plus a
    // centring offset; search a small window around it.
    let origin = view
        .compute_point(root, &graphene::Point::new(0.0, 0.0))
        .map(|p| (((p.x() as f64 + sx) * s), ((p.y() as f64 + sy) * s)))
        .unwrap_or((0.0, 0.0));
    let f = frame.lock();
    let (rx, ry, fw, fh) = match region {
        Some((x, y, w, h)) => (x as usize, y as usize, w as usize, h as usize),
        None => (0, 0, f.size.width as usize, f.size.height as usize),
    };
    let vw = view.width() as f64 * s;
    let vh = view.height() as f64 * s;
    let cx = origin.0 + (vw - fw as f64) / 2.0;
    let cy = origin.1 + (vh - fh as f64) / 2.0;

    // In PSNR mode, alignment is chosen by squared error: with lossy video, equal-pixel
    // counts can favour a neighbouring offset (chroma bleeds along edges).
    let by_error = CHECK_PSNR.get().copied().flatten().is_some();
    let sample = |ox: i64, oy: i64, step: usize| -> (u64, u64) {
        let (mut total, mut equal) = (0u64, 0u64);
        let mut sse = 0f64;
        for y in (0..fh).step_by(step) {
            let ty = y as i64 + oy;
            if ty < 0 || ty as usize >= th {
                continue;
            }
            for x in (0..fw).step_by(step) {
                let tx = x as i64 + ox;
                if tx < 0 || tx as usize >= tw {
                    continue;
                }
                let a = &f.pixels[(y + ry) * f.stride + (x + rx) * 4..][..3];
                let b = &bytes[ty as usize * stride + tx as usize * 4..][..3];
                total += 1;
                equal += (a == b) as u64;
                sse += (0..3)
                    .map(|i| (a[i] as f64 - b[i] as f64).powi(2))
                    .sum::<f64>();
            }
        }
        if by_error {
            // Score as "equal" = total scaled down by mean error (higher is better), so
            // the comparison below picks the lowest error.
            let mse = sse / (total.max(1) * 3) as f64;
            equal = ((total as f64) / (1.0 + mse)) as u64;
        }
        (total, equal)
    };
    // Try offsets nearest the expected one first, so that ties (uniform content)
    // resolve to the expected placement; count how many offsets tie for best.
    let (ex, ey) = (cx.round() as i64, cy.round() as i64);
    let mut cands: Vec<(i64, i64)> = ((ey - 3)..=(ey + 3))
        .flat_map(|oy| ((ex - 3)..=(ex + 3)).map(move |ox| (ox, oy)))
        .collect();
    cands.sort_by_key(|&(ox, oy)| (ox - ex).abs() + (oy - ey).abs());
    let mut best = (ex, ey, 0u64, 0u64);
    let mut ties = 0;
    for (ox, oy) in cands {
        // Full resolution in PSNR mode: a 1-in-7 subsample of lossy edges can rank a
        // neighbouring offset first.
        let (t, e) = sample(ox, oy, if by_error { 1 } else { 7 });
        if t == 0 {
            continue;
        }
        if std::env::var_os("TRANSOM_CHECK_DEBUG").is_some() {
            println!("check: candidate ({ox},{oy}): {e}/{t}");
        }
        if e * best.2.max(1) > best.3 * t {
            best = (ox, oy, t, e);
            ties = 1;
        } else if e * best.2.max(1) == best.3 * t {
            ties += 1;
        }
    }
    if ties > 4 {
        println!(
            "check: note: region content is too uniform to verify alignment ({ties} offsets match equally)"
        );
    }
    let (ox, oy) = (best.0, best.1);
    let (total, equal) = if by_error {
        let mut t = 0u64;
        let mut e = 0u64;
        for y in 0..fh {
            let ty = y as i64 + oy;
            if ty < 0 || ty as usize >= th {
                continue;
            }
            for x in 0..fw {
                let tx = x as i64 + ox;
                if tx < 0 || tx as usize >= tw {
                    continue;
                }
                let a = &f.pixels[(y + ry) * f.stride + (x + rx) * 4..][..3];
                let b = &bytes[ty as usize * stride + tx as usize * 4..][..3];
                t += 1;
                e += (a == b) as u64;
            }
        }
        (t, e)
    } else {
        sample(ox, oy, 1)
    };
    // Diagnostics: how different are mismatching pixels, and where are they?
    let mut hist = [0u64; 4]; // max channel diff: 1, 2-3, 4-15, 16+
    let mut sse = 0f64;
    let mut n = 0u64;
    let mut bbox = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..fh {
        let ty = y as i64 + oy;
        if ty < 0 || ty as usize >= th {
            continue;
        }
        for x in 0..fw {
            let tx = x as i64 + ox;
            if tx < 0 || tx as usize >= tw {
                continue;
            }
            let a = &f.pixels[(y + ry) * f.stride + (x + rx) * 4..][..3];
            let b = &bytes[ty as usize * stride + tx as usize * 4..][..3];
            let d = (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0);
            n += 3;
            sse += (0..3)
                .map(|i| (a[i] as f64 - b[i] as f64).powi(2))
                .sum::<f64>();
            if d > 0 {
                hist[match d {
                    1 => 0,
                    2..=3 => 1,
                    4..=15 => 2,
                    _ => 3,
                }] += 1;
                bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
            }
        }
    }
    if equal != total
        && let Some(path) = screenshot
    {
        // Save the framebuffer as the reference image next to the screenshot.
        let mut rgba = Vec::with_capacity(fw * fh * 4);
        for y in 0..fh {
            for px in f.pixels[y * f.stride..][..fw * 4].as_chunks::<4>().0 {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        let t = gdk::MemoryTexture::new(
            fw as i32,
            fh as i32,
            gdk::MemoryFormat::B8g8r8a8,
            &glib::Bytes::from_owned(rgba),
            fw * 4,
        );
        let _ = t.save_to_png(path.replace(".png", "-frame.png"));
    }
    if equal != total {
        println!(
            "check: mismatches by max channel diff: 1: {}, 2-3: {}, 4-15: {}, 16+: {}; bbox x {}..{} y {}..{}",
            hist[0], hist[1], hist[2], hist[3], bbox.0, bbox.2, bbox.1, bbox.3
        );
    }
    let pct = 100.0 * equal as f64 / total.max(1) as f64;
    let covered = 100.0 * total as f64 / (fw * fh) as f64;
    let psnr = if sse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / (sse / n.max(1) as f64)).log10()
    };
    println!(
        "check: PSNR {psnr:.2} dB; video layers cover {} px",
        view.video_area()
    );
    let exact = match CHECK_PSNR.get().copied().flatten() {
        Some(min) => psnr >= min && covered > 99.0,
        None => equal == total && covered > 99.0,
    };
    println!(
        "check: scale {s}, surface offset ({sx},{sy}), window {tw}x{th} device px, frame {fw}x{fh} at offset ({ox},{oy}); \
         {equal}/{total} pixels identical ({pct:.4}%), {covered:.2}% of frame visible -> {}",
        match (exact, CHECK_PSNR.get().copied().flatten()) {
            (true, None) => "PIXEL-EXACT".to_string(),
            (false, None) => "NOT pixel-exact".to_string(),
            (true, Some(min)) => format!("PSNR-OK (>= {min} dB)"),
            (false, Some(min)) => format!("PSNR-FAILED (< {min} dB)"),
        }
    );
    exact
}

/// `--file-clip-test`: copy local files, and hash files that arrive from the remote.
fn install_file_clip_test(view: &TransomView) {
    use sha2::{Digest, Sha256};
    fn hash_tree(paths: &[std::path::PathBuf]) -> std::collections::BTreeMap<String, String> {
        fn walk(
            p: &std::path::Path,
            base: &std::path::Path,
            out: &mut std::collections::BTreeMap<String, String>,
        ) {
            if p.is_dir() {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        walk(&e.path(), base, out);
                    }
                }
            } else if let Ok(data) = std::fs::read(p) {
                let rel = p
                    .strip_prefix(base)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, format!("{:x}", Sha256::digest(&data)));
            }
        }
        let mut out = Default::default();
        for p in paths {
            let base = p.parent().unwrap_or(p);
            walk(p, base, &mut out);
        }
        out
    }
    let cb = view.clipboard();
    cb.connect_changed(|cb| {
        if cb.is_local() && !cb.formats().contain_mime_type("text/uri-list") {
            return;
        }
        let local_set = cb
            .formats()
            .contains_type(gtk::gdk::FileList::static_type())
            && cb.is_local()
            && !cb
                .formats()
                .contain_mime_type("x-special/gnome-copied-files");
        if local_set {
            return; // our own copy below
        }
        cb.read_value_async(
            gtk::gdk::FileList::static_type(),
            glib::Priority::DEFAULT,
            gtk::gio::Cancellable::NONE,
            |r| match r.ok().and_then(|v| v.get::<gtk::gdk::FileList>().ok()) {
                Some(list) => {
                    let paths: Vec<_> = list.files().iter().filter_map(|f| f.path()).collect();
                    println!("LOCAL CLIPBOARD FILES: {:?}", hash_tree(&paths));
                }
                None => println!("LOCAL CLIPBOARD FILES: read failed"),
            },
        );
    });
    view.connect_connected(|v| {
        let v = v.clone();
        glib::timeout_add_local_once(Duration::from_secs(6), move || {
            let dir = std::env::temp_dir().join("transom-demo-files");
            let _ = std::fs::create_dir_all(dir.join("sub"));
            let _ = std::fs::write(dir.join("hello.txt"), "hello from the local side\n");
            let blob: Vec<u8> = (0..(2 * 1024 * 1024 + 3) as u32)
                .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
                .collect();
            let _ = std::fs::write(dir.join("sub").join("data.bin"), blob);
            let roots = vec![dir.join("hello.txt"), dir.join("sub")];
            println!("LOCAL FILES SET: {:?}", hash_tree(&roots));
            let files: Vec<gtk::gio::File> = roots.iter().map(gtk::gio::File::for_path).collect();
            let list = gtk::gdk::FileList::from_array(&files);
            v.clipboard()
                .set_content(Some(&gtk::gdk::ContentProvider::for_value(
                    &list.to_value(),
                )))
                .ok();
        });
    });
}

/// `--multimon`: one fullscreen window per monitor; the first monitor's view owns
/// the session, the others follow it and show their region.
fn build_multimon(app: &gtk::Application, args: Args, code: Rc<Cell<u8>>) {
    use transom_view::core::monitors::{self, LocalMonitor};
    let display = gdk::Display::default().expect("display");
    let mons: Vec<gdk::Monitor> = (0..display.monitors().n_items())
        .filter_map(|i| display.monitors().item(i).and_downcast::<gdk::Monitor>())
        .collect();
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
                primary: i == 0,
            }
        })
        .collect();
    let layout = monitors::layout(&local, args.scale_mode);
    let (origin, size) = monitors::bounds(&layout);
    eprintln!("monitors: {local:?}");
    eprintln!(
        "layout: {layout:?} -> {}x{} at {origin:?}",
        size.width, size.height
    );

    let mut views = Vec::new();
    for (i, (m, rm)) in mons.iter().zip(&layout).enumerate() {
        let view = if i == 0 {
            TransomView::new()
        } else {
            TransomView::new_follower(&views[0])
        };
        let r = rm.region(origin);
        view.set_region(Some((r.x, r.y, r.width, r.height)));
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title(format!("monitor {i}"))
            .build();
        window.set_child(Some(&view));
        window.fullscreen_on_monitor(m);
        window.present();
        views.push(view);
    }
    let leader = views[0].clone();
    let trust = args.trust.clone();
    leader.set_cert_handler(move |_, p| {
        let ok = trust.as_deref() == Some(p.info.fingerprint_sha256.as_str());
        let _ = p.reply.send(if ok {
            CertDecision::AcceptOnce
        } else {
            CertDecision::Reject
        });
    });
    let password = args
        .password_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| SecretString::from(s.lines().next().unwrap_or("").to_string()));
    let config = SessionConfig {
        host: args.host.clone(),
        port: args.port,
        username: Some(args.user.clone()),
        domain: args.domain.clone(),
        password,
        security: SecurityMode::Negotiate,
        cert_policy: CertPolicy::Verify,
        scale_mode: args.scale_mode,
        fit_mode: args.fit,
        clipboard: transom_view::core::config::ClipboardMode::Off,
        audio: transom_view::core::config::AudioMode::Off,
        microphone: false,
        kerberos: false,
        initial: desktop_request(args.scale_mode, Default::default(), 1.0),
        monitors: layout.clone(),
        gateway: None,
        video_decode: args.video_decode,
        redirections: args.redirections.clone(),
    };
    leader.connect_desktop_resized(|_, w, h| eprintln!("desktop resized: {w}x{h}"));
    let app2 = app.clone();
    let c = code.clone();
    leader.connect_disconnected(move |_, r| {
        eprintln!("disconnected: {r}");
        if c.get() == 0 {
            c.set(1);
        }
        app2.quit();
    });
    let engine = FreeRdpEngine::new(glib::user_cache_dir().join("transom-view-demo/freerdp"));
    let started = Rc::new(Cell::new(false));
    let l = leader.clone();
    leader.add_tick_callback(move |_, _| {
        if !started.replace(true)
            && let Err(e) = l.connect_session(&engine, config.clone())
        {
            eprintln!("connect failed: {e}");
        }
        glib::ControlFlow::Break
    });
    if args.check {
        let views = views.clone();
        let regions: Vec<_> = layout.iter().map(|m| m.region(origin)).collect();
        let shot = args.screenshot.clone();
        let app = app.clone();
        let ticks = Rc::new(Cell::new(0u32));
        glib::timeout_add_local(Duration::from_millis(250), move || {
            if !views[0].is_connected() {
                return glib::ControlFlow::Continue;
            }
            let n = ticks.get() + 1;
            ticks.set(n);
            // Like the single-view check: start after 6 s, then once a second for up
            // to 3 more tries (Windows 11 resizes again after the monitor layout
            // arrives and repaints for a moment after that).
            if n < 24 || !(n - 24).is_multiple_of(4) {
                return glib::ControlFlow::Continue;
            }
            let (w, h) = views[0].desktop_size().unwrap_or_default();
            eprintln!("remote desktop {w}x{h}");
            let mut all = true;
            for (i, (v, r)) in views.iter().zip(&regions).enumerate() {
                let path = shot
                    .as_ref()
                    .map(|p| p.replace(".png", &format!("-m{i}.png")));
                print!("monitor {i}: ");
                all &= run_check_region(v, path.as_deref(), Some((r.x, r.y, r.width, r.height)));
            }
            if !all && n < 24 + 4 * 3 {
                return glib::ControlFlow::Continue;
            }
            code.set(if all { 0 } else { 3 });
            views[0].shutdown(Duration::from_secs(5));
            app.quit();
            glib::ControlFlow::Break
        });
    }
}
