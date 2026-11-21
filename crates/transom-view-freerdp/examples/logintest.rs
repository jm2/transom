// SPDX-License-Identifier: Apache-2.0
//! gnome-remote-desktop remote-login test (`scripts/login-check.sh`): connect to the
//! system daemon, log in at the GDM greeter by typing, follow the redirection into
//! the new session and save a screenshot.
//!
//! `logintest HOST[:PORT] RDPUSER --password-file F --trust SHA256
//!            --login-password-file F --out session.png`

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use transom_view_core::config::{
    AudioMode, CertPolicy, ClipboardMode, SecurityMode, SessionConfig,
};
use transom_view_core::engine::{
    CertDecision, EngineCommand, EngineEvent, InputEvent, RdpEngine, SharedFrame,
};
use transom_view_core::geometry::{FitMode, LogicalSize, ScaleMode, desktop_request};
use transom_view_core::keymap::us_ascii;
use transom_view_core::secrecy::{ExposeSecret, SecretString};
use transom_view_core::trust::normalize_fingerprint;
use transom_view_freerdp::FreeRdpEngine;

fn read_pw(path: &str) -> SecretString {
    SecretString::from(
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or("")
            .to_string(),
    )
}

fn key(h: &transom_view_core::engine::EngineHandle, code: u8, down: bool) {
    h.send(EngineCommand::Input(InputEvent::Key {
        scancode: code,
        extended: false,
        down,
    }));
    std::thread::sleep(Duration::from_millis(25));
}

fn type_text(h: &transom_view_core::engine::EngineHandle, text: &str) {
    for c in text.chars() {
        let Some((sc, shift)) = us_ascii(c) else {
            continue;
        };
        if shift {
            key(h, 0x2A, true);
        }
        key(h, sc.code, true);
        key(h, sc.code, false);
        if shift {
            key(h, 0x2A, false);
        }
    }
}

fn save_png(path: &str, frame: &SharedFrame) {
    let f = frame.lock();
    let (w, h) = (f.size.width, f.size.height);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for row in f.pixels.chunks_exact(f.stride).take(h as usize) {
        for px in row[..w as usize * 4].as_chunks::<4>().0 {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgb).unwrap();
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| {
        a.iter()
            .position(|x| x == n)
            .and_then(|i| a.get(i + 1))
            .cloned()
    };
    let (Some(target), Some(user), Some(pwf), Some(trust), Some(lpf), Some(out)) = (
        a.first(),
        a.get(1),
        flag("--password-file"),
        flag("--trust"),
        flag("--login-password-file"),
        flag("--out"),
    ) else {
        eprintln!(
            "usage: logintest HOST[:PORT] RDPUSER --password-file F --trust SHA256 --login-password-file F --out F.png"
        );
        return ExitCode::from(2);
    };
    let trust = normalize_fingerprint(&trust);
    let (host, port) = target
        .rsplit_once(':')
        .map_or((target.clone(), 3389), |(h, p)| {
            (h.to_string(), p.parse().unwrap_or(3389))
        });
    let config = SessionConfig {
        host,
        port,
        username: Some(user.clone()),
        domain: None,
        password: Some(read_pw(&pwf)),
        security: SecurityMode::Negotiate,
        cert_policy: CertPolicy::Verify,
        scale_mode: ScaleMode::MatchLocal,
        fit_mode: FitMode::Fit,
        clipboard: ClipboardMode::Off,
        audio: AudioMode::Off,
        microphone: false,
        kerberos: false,
        initial: desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1280.0, 800.0), 1.0),
        monitors: Vec::new(),
    };
    let login_pw = read_pw(&lpf);
    let (tx, rx) = mpsc::channel();
    let engine = FreeRdpEngine::new(std::env::temp_dir().join("transom-logintest-freerdp"));
    let handle = match engine.connect(config, Box::new(move |e| drop(tx.send(e)))) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let start = Instant::now();
    let mut connects = 0;
    let mut redirects = 0;
    let mut frame: Option<SharedFrame> = None;
    let mut last_connect = Instant::now();
    let mut typed = false;
    let mut redirected_at: Option<Instant> = None;
    while start.elapsed() < Duration::from_secs(90) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(EngineEvent::Connected {
                desktop, frame: f, ..
            }) => {
                connects += 1;
                println!(
                    "connected #{connects}: {}x{}",
                    desktop.width, desktop.height
                );
                frame = Some(f);
                last_connect = Instant::now();
            }
            Ok(EngineEvent::Redirecting { target }) => {
                redirects += 1;
                println!("redirecting #{redirects} to {target}");
                if typed {
                    redirected_at = Some(Instant::now());
                }
            }
            Ok(EngineEvent::CertificatePrompt(p)) => {
                let ok = trust.as_deref() == Some(p.info.fingerprint_sha256.as_str());
                println!("certificate prompt ({}), trusted: {ok}", p.info.host);
                let _ = p.reply.send(if ok {
                    CertDecision::AcceptOnce
                } else {
                    CertDecision::Reject
                });
            }
            Ok(EngineEvent::AuthRequired(r)) => {
                println!("unexpected credential prompt ({:?})", r.kind);
                drop(r);
            }
            Ok(EngineEvent::Disconnected(r)) => {
                println!("disconnected: {r}");
                break;
            }
            _ => {}
        }
        // Greeter shown: select the (only) user, type the password, submit.
        if !typed && connects >= 1 && last_connect.elapsed() > Duration::from_secs(6) {
            typed = true;
            println!("logging in at the greeter");
            key(&handle, 0x1C, true);
            key(&handle, 0x1C, false);
            std::thread::sleep(Duration::from_secs(2));
            type_text(&handle, login_pw.expose_secret());
            key(&handle, 0x1C, true);
            key(&handle, 0x1C, false);
        }
        // After logging in and a later (re)connect, give the session time to draw.
        if let Some(t) = redirected_at
            && t.elapsed() > Duration::from_secs(12)
        {
            break;
        }
    }
    if let Some(f) = &frame {
        save_png(&out, f);
        println!("saved {out}");
    }
    println!("summary: {connects} connects, {redirects} redirects");
    handle.shutdown(Duration::from_secs(5));
    if redirected_at.is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
