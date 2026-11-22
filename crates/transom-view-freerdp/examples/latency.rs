// SPDX-License-Identifier: Apache-2.0
#![allow(unsafe_code)] // clock_gettime only
//! Input-to-frame latency (`scripts/latency-check.sh`): press a key that makes the
//! in-session probe repaint its whole window, and time until a frame update
//! covering most of the desktop arrives. Prints median / p95 / max.
//!
//! `latency HOST[:PORT] USER --password-file F --trust SHA256 [--samples N]`

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use transom_view_core::config::{
    AudioMode, CertPolicy, ClipboardMode, SecurityMode, SessionConfig,
};
use transom_view_core::engine::{
    CertDecision, EngineCommand, EngineEvent, InputEvent, MouseButton, RdpEngine,
};
use transom_view_core::geometry::{FitMode, LogicalSize, ScaleMode, desktop_request};
use transom_view_core::secrecy::SecretString;
use transom_view_core::trust::normalize_fingerprint;
use transom_view_freerdp::FreeRdpEngine;

/// CLOCK_MONOTONIC seconds (same clock as the probe inside the container).
fn mono() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
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
        eprintln!("usage: latency HOST[:PORT] USER --password-file F --trust SHA256 [--samples N]");
        return ExitCode::from(2);
    };
    let samples: usize = flag("--samples").and_then(|s| s.parse().ok()).unwrap_or(40);
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
        clipboard: ClipboardMode::Off,
        audio: AudioMode::Off,
        microphone: false,
        kerberos: false,
        initial: desktop_request(ScaleMode::MatchLocal, LogicalSize::new(1280.0, 800.0), 1.0),
        monitors: Vec::new(),
        gateway: None,
        video_decode: Default::default(),
        redirections: Default::default(),
    };
    let (tx, rx) = mpsc::channel();
    let engine = FreeRdpEngine::new(std::env::temp_dir().join("transom-latency-freerdp"));
    let handle = engine
        .connect(config, Box::new(move |e| drop(tx.send(e))))
        .expect("connect");
    let mut desktop = None;
    let start = Instant::now();
    // Connect, then let the probe start (launched by the script) and settle.
    while start.elapsed() < Duration::from_secs(9) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(EngineEvent::Connected { desktop: d, .. }) | Ok(EngineEvent::DesktopResized(d)) => {
                desktop = Some(d)
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
                eprintln!("disconnected: {r}");
                return ExitCode::FAILURE;
            }
            _ => {}
        }
    }
    let Some(d) = desktop else {
        eprintln!("never connected");
        return ExitCode::FAILURE;
    };
    let full = d.width as u64 * d.height as u64;
    for down in [true, false] {
        handle.send(EngineCommand::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            down,
            x: d.width / 2,
            y: d.height / 2,
        }));
    }
    std::thread::sleep(Duration::from_millis(800));
    while rx.try_recv().is_ok() {}
    let mut times = Vec::new();
    let mut firsts = Vec::new();
    for _ in 0..samples {
        let t0 = Instant::now();
        println!("sent {:.6}", mono());
        handle.send(EngineCommand::Input(InputEvent::Key {
            scancode: 0x39,
            extended: false,
            down: true,
        }));
        handle.send(EngineCommand::Input(InputEvent::Key {
            scancode: 0x39,
            extended: false,
            down: false,
        }));
        let mut covered = 0u64;
        let mut first = None;
        let deadline = t0 + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(EngineEvent::FrameUpdate { damage, .. }) =
                rx.recv_timeout(Duration::from_millis(5))
            {
                if first.is_none() {
                    println!("frame {:.6}", mono());
                }
                first.get_or_insert_with(|| t0.elapsed());
                covered += damage
                    .iter()
                    .map(|r| r.width as u64 * r.height as u64)
                    .sum::<u64>();
                if covered * 2 >= full {
                    times.push(t0.elapsed());
                    firsts.extend(first);
                    break;
                }
            }
        }
        // Let the stream go idle before the next sample.
        std::thread::sleep(Duration::from_millis(250));
        while rx.try_recv().is_ok() {}
    }
    handle.shutdown(Duration::from_secs(5));
    if times.len() < samples / 2 {
        eprintln!("only {}/{samples} samples completed", times.len());
        return ExitCode::FAILURE;
    }
    times.sort();
    firsts.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "first frame update after key: median {:.1} ms",
        ms(firsts[firsts.len() / 2])
    );
    let pct = |p: f64| ms(times[((times.len() - 1) as f64 * p).round() as usize]);
    println!(
        "latency over {} samples ({}x{}): median {:.1} ms, p95 {:.1} ms, max {:.1} ms",
        times.len(),
        d.width,
        d.height,
        pct(0.5),
        pct(0.95),
        ms(*times.last().unwrap())
    );
    ExitCode::SUCCESS
}
