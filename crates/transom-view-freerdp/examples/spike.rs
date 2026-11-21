// SPDX-License-Identifier: Apache-2.0
//! Phase 0 spike: connect with FreeRDP, wait for the screen to settle, write a PNG.
//!
//! ```text
//! spike HOST[:PORT] USER [--domain D] [--password-file FILE] [--trust SHA256]
//!       [--size WxH] [--scale S] [--out frame.png] [--settle-ms N]
//!       [--audio] [--mic] [--stay-ms N]   (audio out / microphone in; stay N ms)
//! ```
//!
//! The password is read from `--password-file` or, if absent, the first line of stdin.
//! It is never accepted on argv or from the environment. Without `--trust`, an
//! untrusted certificate is printed and rejected so you can compare the fingerprint
//! (e.g. with `grdctl rdp show` or the Windows cert store) and re-run.

use std::io::BufRead;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use transom_view_core::config::{CertPolicy, DEFAULT_PORT, SecurityMode, SessionConfig};
use transom_view_core::engine::{CertDecision, EngineEvent, RdpEngine, SharedFrame};
use transom_view_core::geometry::{FitMode, LogicalSize, ScaleMode, desktop_request};
use transom_view_core::secrecy::SecretString;
use transom_view_core::trust::normalize_fingerprint;
use transom_view_freerdp::FreeRdpEngine;

struct Args {
    host: String,
    port: u16,
    user: String,
    domain: Option<String>,
    password_file: Option<String>,
    trust: Option<String>,
    size: (f64, f64),
    scale: f64,
    out: String,
    settle: Duration,
    audio: bool,
    mic: bool,
    stay: Option<Duration>,
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
        size: (1280.0, 800.0),
        scale: 1.0,
        out: "frame.png".into(),
        settle: Duration::from_millis(1500),
        audio: false,
        mic: false,
        stay: None,
    };
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--domain" => a.domain = Some(val()?),
            "--password-file" => a.password_file = Some(val()?),
            "--trust" => {
                a.trust = Some(normalize_fingerprint(&val()?).ok_or("bad --trust fingerprint")?)
            }
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                a.size = (
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                );
            }
            "--scale" => a.scale = val()?.parse().map_err(|_| "bad scale")?,
            "--out" => a.out = val()?,
            "--settle-ms" => {
                a.settle = Duration::from_millis(val()?.parse().map_err(|_| "bad ms")?)
            }
            "--audio" => a.audio = true,
            "--mic" => a.mic = true,
            "--stay-ms" => {
                a.stay = Some(Duration::from_millis(val()?.parse().map_err(|_| "bad ms")?))
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(a)
}

fn read_password(a: &Args) -> Result<SecretString, String> {
    let line = match &a.password_file {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?,
        None => {
            let mut s = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut s)
                .map_err(|e| e.to_string())?;
            s
        }
    };
    let pw = line.lines().next().unwrap_or("").to_string();
    Ok(SecretString::from(pw))
}

fn write_png(path: &str, frame: &SharedFrame) -> Result<(u32, u32), String> {
    let f = frame.lock();
    let (w, h) = (f.size.width, f.size.height);
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for row in f.pixels.chunks_exact(f.stride).take(h as usize) {
        for px in row[..w as usize * 4].as_chunks::<4>().0 {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]); // BGRX -> RGB
        }
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(&rgb).map_err(|e| e.to_string())?;
    Ok((w, h))
}

fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: {e}\nusage: spike HOST[:PORT] USER [--domain D] [--password-file F] [--trust SHA256] [--size WxH] [--scale S] [--out F.png]"
            );
            return ExitCode::from(2);
        }
    };
    let password = match read_password(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error reading password: {e}");
            return ExitCode::from(2);
        }
    };

    let initial = desktop_request(
        ScaleMode::MatchLocal,
        LogicalSize::new(args.size.0, args.size.1),
        args.scale,
    );
    eprintln!("FreeRDP {}", FreeRdpEngine::freerdp_version());
    eprintln!(
        "requesting {}x{} at {}% (device {}%)",
        initial.size.width, initial.size.height, initial.scale.desktop, initial.scale.device
    );
    let config = SessionConfig {
        host: args.host.clone(),
        port: args.port,
        username: Some(args.user.clone()),
        domain: args.domain.clone(),
        password: Some(password),
        security: SecurityMode::Negotiate,
        cert_policy: CertPolicy::Verify,
        scale_mode: ScaleMode::MatchLocal,
        fit_mode: FitMode::Fit,
        clipboard: transom_view_core::config::ClipboardMode::Both,
        audio: if args.audio {
            transom_view_core::config::AudioMode::Local
        } else {
            transom_view_core::config::AudioMode::Off
        },
        microphone: args.mic,
        initial,
    };

    let cfg_dir = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/tmp".into())
        .join("transom-spike-freerdp");
    let (tx, rx) = mpsc::channel();
    let engine = FreeRdpEngine::new(cfg_dir);
    let handle = match engine.connect(config, Box::new(move |ev| drop(tx.send(ev)))) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let started = Instant::now();
    let deadline = started + Duration::from_secs(60) + args.stay.unwrap_or_default();
    let mut frame: Option<SharedFrame> = None;
    let mut last_update = Instant::now();
    let mut updates = 0u32;
    let mut code = ExitCode::FAILURE;
    loop {
        let timeout = if frame.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        };
        match rx.recv_timeout(timeout) {
            Ok(EngineEvent::Connected {
                desktop,
                scale,
                frame: f,
            }) => {
                eprintln!(
                    "connected: desktop {}x{}, scale {}%/{}%",
                    desktop.width, desktop.height, scale.desktop, scale.device
                );
                frame = Some(f);
                last_update = Instant::now();
            }
            Ok(EngineEvent::FrameUpdate { damage }) => {
                updates += 1;
                last_update = Instant::now();
                if updates <= 3 {
                    eprintln!("frame update #{updates}: {} rects", damage.len());
                }
            }
            Ok(EngineEvent::DesktopResized(s)) => {
                eprintln!("desktop resized to {}x{}", s.width, s.height)
            }
            Ok(EngineEvent::ResizeAvailable) => eprintln!("display control available"),
            Ok(EngineEvent::Pointer(_))
            | Ok(EngineEvent::PointerPosition { .. })
            | Ok(EngineEvent::ClipboardRemoteFormats(_))
            | Ok(EngineEvent::ClipboardRemoteData { .. }) => {}
            Ok(EngineEvent::ClipboardDataRequest(_)) => {} // never offered anything
            Ok(EngineEvent::Redirecting { target }) => eprintln!("redirecting to {target}"),
            Ok(EngineEvent::AuthRequired(req)) => {
                eprintln!(
                    "server asked for credentials again ({:?}); giving up",
                    req.kind
                );
                drop(req);
            }
            Ok(EngineEvent::CertificatePrompt(p)) => {
                eprintln!("certificate for {}:{}", p.info.host, p.info.port);
                eprintln!("  subject: {}", p.info.subject);
                eprintln!("  issuer:  {}", p.info.issuer);
                eprintln!("  SHA-256: {}", p.info.fingerprint_sha256);
                if let Some(prev) = &p.previous_fingerprint {
                    eprintln!("  CHANGED from: {prev}");
                }
                let ok = args.trust.as_deref() == Some(p.info.fingerprint_sha256.as_str());
                if ok {
                    eprintln!("  matches --trust; accepting for this session");
                } else {
                    eprintln!("  not trusted; re-run with --trust <SHA-256> after verifying it");
                }
                let _ = p.reply.send(if ok {
                    CertDecision::AcceptOnce
                } else {
                    CertDecision::Reject
                });
            }
            Ok(EngineEvent::Disconnected(r)) => {
                eprintln!("disconnected: {r}");
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let (Some(stay), Some(_)) = (args.stay, &frame) {
            if started.elapsed() >= stay {
                eprintln!("stayed {} ms", stay.as_millis());
                code = ExitCode::SUCCESS;
                break;
            }
            continue;
        }
        if let Some(f) = &frame
            && updates > 0
            && last_update.elapsed() >= args.settle
        {
            match write_png(&args.out, f) {
                Ok((w, h)) => {
                    eprintln!("wrote {} ({w}x{h}) after {updates} updates", args.out);
                    code = ExitCode::SUCCESS;
                }
                Err(e) => eprintln!("error writing PNG: {e}"),
            }
            break;
        }
        if Instant::now() > deadline {
            eprintln!("timed out");
            break;
        }
    }
    drop(handle);
    // Give the engine a moment to disconnect cleanly.
    let _ = rx.recv_timeout(Duration::from_secs(3));
    code
}
