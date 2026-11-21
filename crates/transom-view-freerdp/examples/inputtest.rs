// SPDX-License-Identifier: GPL-3.0-or-later
//! Sends a fixed input script through the engine, for `scripts/input-check.sh`,
//! which compares it with what the in-session probe received.
//!
//! `inputtest HOST[:PORT] USER --password-file F --trust SHA256 [--wait-ms N]`

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use transom_view_core::config::{CertPolicy, DEFAULT_PORT, SecurityMode, SessionConfig};
use transom_view_core::engine::{
    CertDecision, EngineCommand, EngineEvent, InputEvent, MouseButton, RdpEngine,
};
use transom_view_core::geometry::{FitMode, LogicalSize, ScaleMode, desktop_request};
use transom_view_core::secrecy::SecretString;
use transom_view_core::trust::normalize_fingerprint;
use transom_view_freerdp::FreeRdpEngine;

fn key(scancode: u8, extended: bool, down: bool) -> InputEvent {
    InputEvent::Key {
        scancode,
        extended,
        down,
    }
}

/// The script. Keep in sync with the expectations in `scripts/input-check.sh`.
fn script() -> Vec<(InputEvent, u64)> {
    // Click first: a freshly mapped window in a headless session has no keyboard
    // focus until it's clicked.
    let mut v = vec![
        (InputEvent::MouseMove { x: 50, y: 50 }, 50),
        (
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 50,
                y: 50,
            },
            30,
        ),
        (
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: false,
                x: 50,
                y: 50,
            },
            200,
        ),
    ];
    let tap = |v: &mut Vec<(InputEvent, u64)>, sc: u8, ext: bool| {
        v.push((key(sc, ext, true), 15));
        v.push((key(sc, ext, false), 15));
    };
    // "hello" (US positions: h e l l o)
    for sc in [0x23, 0x12, 0x26, 0x26, 0x18] {
        tap(&mut v, sc, false);
    }
    // Shift+1 -> exclam
    v.push((key(0x2A, false, true), 15));
    tap(&mut v, 0x02, false);
    v.push((key(0x2A, false, false), 15));
    // Extended keys: Left, Right, Home, Delete, KP_Enter, Right Ctrl; F5.
    for (sc, ext) in [
        (0x4B, true),
        (0x4D, true),
        (0x47, true),
        (0x53, true),
        (0x1C, true),
        (0x1D, true),
        (0x3F, false),
    ] {
        tap(&mut v, sc, ext);
    }
    // Unicode input: 'z' (in the server keymap) and 'é' (not in a US keymap).
    for cu in [0x7A, 0xE9] {
        v.push((
            InputEvent::Unicode {
                code_unit: cu,
                down: true,
            },
            15,
        ));
        v.push((
            InputEvent::Unicode {
                code_unit: cu,
                down: false,
            },
            15,
        ));
    }
    // Mouse: move, left click at (100,100), right click at (200,150), middle click.
    v.push((InputEvent::MouseMove { x: 100, y: 100 }, 50));
    for (b, x, y) in [
        (MouseButton::Left, 100, 100),
        (MouseButton::Right, 200, 150),
        (MouseButton::Middle, 300, 200),
    ] {
        v.push((InputEvent::MouseMove { x, y }, 30));
        v.push((
            InputEvent::MouseButton {
                button: b,
                down: true,
                x,
                y,
            },
            30,
        ));
        v.push((
            InputEvent::MouseButton {
                button: b,
                down: false,
                x,
                y,
            },
            60,
        ));
    }
    // Wheel: one notch away (up), one notch towards (down), then right.
    v.push((
        InputEvent::Wheel {
            delta: 120,
            x: 300,
            y: 200,
        },
        80,
    ));
    v.push((
        InputEvent::Wheel {
            delta: -120,
            x: 300,
            y: 200,
        },
        80,
    ));
    v.push((
        InputEvent::HWheel {
            delta: 120,
            x: 300,
            y: 200,
        },
        80,
    ));
    v
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| {
        a.iter()
            .position(|x| x == n)
            .and_then(|i| a.get(i + 1))
            .cloned()
    };
    let (Some(target), Some(user), Some(pwf), Some(trust)) = (
        a.first(),
        a.get(1),
        flag("--password-file"),
        flag("--trust"),
    ) else {
        eprintln!(
            "usage: inputtest HOST[:PORT] USER --password-file F --trust SHA256 [--wait-ms N]"
        );
        return ExitCode::from(2);
    };
    let wait = Duration::from_millis(
        flag("--wait-ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(5000),
    );
    let trust = normalize_fingerprint(&trust);
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(DEFAULT_PORT)),
        None => (target.clone(), DEFAULT_PORT),
    };
    let password = std::fs::read_to_string(&pwf).unwrap_or_default();
    let config = SessionConfig {
        host,
        port,
        username: Some(user.clone()),
        domain: None,
        password: Some(SecretString::from(
            password.lines().next().unwrap_or("").to_string(),
        )),
        security: SecurityMode::Negotiate,
        cert_policy: CertPolicy::Verify,
        scale_mode: ScaleMode::MatchLocal,
        fit_mode: FitMode::Fit,
        clipboard: transom_view_core::config::ClipboardMode::Both,
        audio: transom_view_core::config::AudioMode::Off,
        microphone: false,
        initial: desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1024.0, 768.0), 1.0),
    };
    let dir = std::env::temp_dir().join("transom-inputtest-freerdp");
    let (tx, rx) = mpsc::channel();
    let handle = match FreeRdpEngine::new(dir).connect(config, Box::new(move |e| drop(tx.send(e))))
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut connected_at = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(EngineEvent::Connected { desktop, .. }) => {
                println!("connected {}x{}", desktop.width, desktop.height);
                connected_at = Some(Instant::now());
            }
            Ok(EngineEvent::CertificatePrompt(p)) => {
                let ok = trust.as_deref() == Some(p.info.fingerprint_sha256.as_str());
                let _ = p.reply.send(if ok {
                    CertDecision::AcceptOnce
                } else {
                    CertDecision::Reject
                });
            }
            Ok(EngineEvent::Disconnected(r)) => {
                eprintln!("disconnected early: {r}");
                return ExitCode::FAILURE;
            }
            _ => {}
        }
        if connected_at.is_some_and(|t| t.elapsed() >= wait) {
            break;
        }
        if Instant::now() > deadline {
            eprintln!("timed out");
            return ExitCode::FAILURE;
        }
    }
    for (ev, delay) in script() {
        handle.send(EngineCommand::Input(ev));
        std::thread::sleep(Duration::from_millis(delay));
    }
    std::thread::sleep(Duration::from_millis(800));
    println!("script sent");
    handle.shutdown(Duration::from_secs(5));
    ExitCode::SUCCESS
}
