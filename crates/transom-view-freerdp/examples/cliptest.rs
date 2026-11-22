// SPDX-License-Identifier: Apache-2.0
//! Clipboard test client for `scripts/clipboard-check.sh`: offers LOCAL_TEXT to the
//! remote and prints any text the remote copies.
//!
//! `cliptest HOST[:PORT] USER --password-file F --trust SHA256 [--seconds N]`

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use transom_view_core::clipboard::{self, ClipFormat};
use transom_view_core::config::{CertPolicy, ClipboardMode, SecurityMode, SessionConfig};
use transom_view_core::engine::{
    CertDecision, EngineCommand, EngineEvent, InputEvent, MouseButton, RdpEngine,
};
use transom_view_core::geometry::{FitMode, LogicalSize, ScaleMode, desktop_request};
use transom_view_core::secrecy::SecretString;
use transom_view_core::trust::normalize_fingerprint;
use transom_view_freerdp::FreeRdpEngine;

const LOCAL_TEXT: &str = "hello from transom ✓\nline two";

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
        eprintln!("usage: cliptest HOST[:PORT] USER --password-file F --trust SHA256");
        return ExitCode::from(2);
    };
    let secs = flag("--seconds").and_then(|s| s.parse().ok()).unwrap_or(25);
    let trust = normalize_fingerprint(&trust);
    let (host, port) = target
        .rsplit_once(':')
        .map_or((target.clone(), 3389), |(h, p)| {
            (h.to_string(), p.parse().unwrap_or(3389))
        });
    let pw = std::fs::read_to_string(&pwf).unwrap_or_default();
    let config = SessionConfig {
        host,
        port,
        username: Some(user.clone()),
        domain: None,
        password: Some(SecretString::from(
            pw.lines().next().unwrap_or("").to_string(),
        )),
        security: SecurityMode::Negotiate,
        cert_policy: CertPolicy::Verify,
        scale_mode: ScaleMode::MatchLocal,
        fit_mode: FitMode::Fit,
        clipboard: ClipboardMode::Both,
        audio: transom_view_core::config::AudioMode::Off,
        microphone: false,
        kerberos: false,
        initial: desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1024.0, 768.0), 1.0),
        monitors: Vec::new(),
        gateway: None,
        video_decode: Default::default(),
        redirections: Default::default(),
    };
    let (tx, rx) = mpsc::channel();
    let engine = FreeRdpEngine::new(std::env::temp_dir().join("transom-cliptest-freerdp"));
    let handle = match engine.connect(config, Box::new(move |e| drop(tx.send(e)))) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let end = Instant::now() + Duration::from_secs(secs);
    let mut clicked = false;
    let mut connected_at = None;
    while Instant::now() < end {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(EngineEvent::Connected { .. }) => {
                println!("connected");
                connected_at = Some(Instant::now());
                let formats = if std::env::var_os("CLIPTEST_IMAGE").is_some() {
                    vec![ClipFormat::Png, ClipFormat::Dib]
                } else {
                    vec![ClipFormat::Text]
                };
                handle.send(EngineCommand::ClipboardLocalFormats(formats));
            }
            Ok(EngineEvent::CertificatePrompt(p)) => {
                let ok = trust.as_deref() == Some(p.info.fingerprint_sha256.as_str());
                let _ = p.reply.send(if ok {
                    CertDecision::AcceptOnce
                } else {
                    CertDecision::Reject
                });
            }
            Ok(EngineEvent::ClipboardDataRequest(f)) => {
                println!("remote requested {f:?}");
                // 40x30 test image (red→green gradient).
                let img = clipboard::Image {
                    width: 40,
                    height: 30,
                    rgba: (0..30u32)
                        .flat_map(|y| {
                            (0..40u32).flat_map(move |x| [(x * 6) as u8, (y * 8) as u8, 60, 255])
                        })
                        .collect(),
                };
                let data = match f {
                    ClipFormat::Text => Some(clipboard::encode_text(LOCAL_TEXT)),
                    ClipFormat::Dib => Some(clipboard::image_to_dib(&img)),
                    _ => None,
                };
                handle.send(EngineCommand::ClipboardReply { format: f, data });
            }
            Ok(EngineEvent::ClipboardRemoteFormats(fs)) => {
                println!("remote offers {fs:?}");
                if fs.contains(&ClipFormat::Text) {
                    handle.send(EngineCommand::ClipboardRequest(ClipFormat::Text));
                }
            }
            Ok(EngineEvent::ClipboardRemoteData { format, data }) => match (format, data) {
                (ClipFormat::Text, Some(d)) => {
                    println!("REMOTE TEXT: {:?}", clipboard::decode_text(&d))
                }
                (f, d) => println!("remote data {f:?}: {:?} bytes", d.map(|d| d.len())),
            },
            Ok(EngineEvent::Disconnected(r)) => {
                eprintln!("disconnected: {r}");
                return ExitCode::FAILURE;
            }
            _ => {}
        }
        // Focus the probe window so it may use the clipboard.
        if !clicked && connected_at.is_some_and(|t| t.elapsed() > Duration::from_secs(7)) {
            clicked = true;
            for down in [true, false] {
                handle.send(EngineCommand::Input(InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down,
                    x: 50,
                    y: 50,
                }));
            }
        }
    }
    handle.shutdown(Duration::from_secs(5));
    ExitCode::SUCCESS
}
