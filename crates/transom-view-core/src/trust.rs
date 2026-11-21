// SPDX-License-Identifier: Apache-2.0
//! Server certificate identity and the trust-on-first-use store.
//!
//! Flow (PLAN §9): FreeRDP validates against the system CA first; only certificates
//! it can't validate reach us. Those are checked against [`TrustStore`]: a known
//! fingerprint connects silently, an unknown one asks the user, and a *changed* one
//! is blocked by default with a prominent warning.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A server certificate as presented to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    pub host: String,
    pub port: u16,
    pub subject: String,
    pub issuer: String,
    /// SHA-256 over the DER certificate, formatted by [`format_fingerprint`].
    pub fingerprint_sha256: String,
}

/// SHA-256 fingerprint of a DER certificate, formatted the way `grdctl` and
/// `openssl x509 -fingerprint -sha256` print it: upper-case hex pairs joined by `:`.
pub fn fingerprint_sha256(der: &[u8]) -> String {
    format_fingerprint(&Sha256::digest(der))
}

/// Format raw digest bytes as `AB:CD:…`.
pub fn format_fingerprint(digest: &[u8]) -> String {
    let mut s = String::with_capacity(digest.len() * 3);
    for (i, b) in digest.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// Normalise a fingerprint typed or pasted by a user (any case, with or without
/// `:`/spaces) for comparison. Returns `None` if it isn't 32 bytes of hex.
pub fn normalize_fingerprint(input: &str) -> Option<String> {
    let hex: String = input
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
        .collect::<Option<_>>()?;
    Some(format_fingerprint(&bytes))
}

/// Result of looking a certificate up in the [`TrustStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// This host:port presented this fingerprint before and the user trusted it.
    Trusted,
    /// Never seen: ask the user (show the fingerprint for out-of-band comparison).
    Unknown,
    /// This host:port previously presented a *different* certificate.
    Changed { previous: String },
}

/// Fingerprints the user chose to trust, persisted as TOML.
#[derive(Debug, Clone)]
pub struct TrustStore {
    path: PathBuf,
    hosts: BTreeMap<String, String>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct TrustFile {
    #[serde(default)]
    hosts: BTreeMap<String, String>,
}

/// Errors loading the trust store.
#[derive(Debug, thiserror::Error)]
pub enum TrustStoreError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("malformed trust store: {0}")]
    Parse(#[from] toml::de::Error),
}

fn host_key(host: &str, port: u16) -> String {
    let h = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let h = h.trim_end_matches('.');
    if h.contains(':') {
        format!("[{h}]:{port}")
    } else {
        format!("{h}:{port}")
    }
}

impl TrustStore {
    /// Load from `path`; a missing file is an empty store.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, TrustStoreError> {
        let path = path.into();
        let hosts = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let f: TrustFile = toml::from_str(&text)?;
                f.hosts
                    .into_iter()
                    .filter_map(|(k, v)| {
                        normalize_fingerprint(&v).map(|fp| (k.to_ascii_lowercase(), fp))
                    })
                    .collect()
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, hosts })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn check(&self, host: &str, port: u16, fingerprint: &str) -> TrustStatus {
        let Some(fp) = normalize_fingerprint(fingerprint) else {
            return TrustStatus::Unknown;
        };
        match self.hosts.get(&host_key(host, port)) {
            Some(known) if *known == fp => TrustStatus::Trusted,
            Some(known) => TrustStatus::Changed {
                previous: known.clone(),
            },
            None => TrustStatus::Unknown,
        }
    }

    /// Trust `fingerprint` for host:port (replacing any previous one) and save.
    pub fn remember(&mut self, host: &str, port: u16, fingerprint: &str) -> io::Result<()> {
        let fp = normalize_fingerprint(fingerprint)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad fingerprint"))?;
        self.hosts.insert(host_key(host, port), fp);
        self.save()
    }

    /// Forget host:port and save. Returns whether it was known.
    pub fn forget(&mut self, host: &str, port: u16) -> io::Result<bool> {
        let was = self.hosts.remove(&host_key(host, port)).is_some();
        if was {
            self.save()?;
        }
        Ok(was)
    }

    fn save(&self) -> io::Result<()> {
        let body = toml::to_string_pretty(&TrustFile {
            hosts: self.hosts.clone(),
        })
        .map_err(io::Error::other)?;
        let text = format!("# Transom: server certificates you chose to trust (SHA-256).\n{body}");
        crate::fsutil::write_atomic(&self.path, text.as_bytes())
    }
}

/// Extract the DER bytes of the first `CERTIFICATE` block in a PEM string.
pub fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let start = pem.find(BEGIN)? + BEGIN.len();
    let end = start + pem[start..].find(END)?;
    base64_decode(&pem[start..end])
}

/// Minimal standard-alphabet base64 decoder that ignores whitespace.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for &c in input.as_bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return None; // data after padding
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (pad <= 2).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_decodes() {
        let pem = "junk\n-----BEGIN CERTIFICATE-----\naGVs\nbG8=\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_to_der(pem).as_deref(), Some(&b"hello"[..]));
        assert_eq!(pem_to_der("no pem"), None);
        assert_eq!(
            base64_decode("aGVsbG8gd29ybGQ="),
            Some(b"hello world".to_vec())
        );
        assert_eq!(base64_decode("a!=="), None);
    }

    #[test]
    fn trust_store_flow() {
        let path = std::env::temp_dir()
            .join(format!("transom-trust-{}", std::process::id()))
            .join("known-hosts.toml");
        let a = fingerprint_sha256(b"cert a");
        let b = fingerprint_sha256(b"cert b");
        let mut s = TrustStore::load(&path).unwrap();
        assert_eq!(s.check("Host.LAN.", 3389, &a), TrustStatus::Unknown);
        s.remember("host.lan", 3389, &a.to_lowercase()).unwrap();
        // Reload from disk; host matching ignores case and a trailing dot.
        let s2 = TrustStore::load(&path).unwrap();
        assert_eq!(s2.check("HOST.lan.", 3389, &a), TrustStatus::Trusted);
        assert_eq!(s2.check("host.lan", 3390, &a), TrustStatus::Unknown);
        assert_eq!(
            s2.check("host.lan", 3389, &b),
            TrustStatus::Changed {
                previous: a.clone()
            }
        );
        let mut s3 = s2.clone();
        s3.remember("[fe80::1]", 3389, &b).unwrap();
        assert_eq!(s3.check("fe80::1", 3389, &b), TrustStatus::Trusted);
        assert!(s3.forget("host.lan", 3389).unwrap());
        assert!(!s3.forget("host.lan", 3389).unwrap());
        assert_eq!(
            TrustStore::load(&path).unwrap().check("host.lan", 3389, &a),
            TrustStatus::Unknown
        );
        std::fs::write(&path, "hosts = 5").unwrap();
        assert!(TrustStore::load(&path).is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn known_digest() {
        // sha256("") = e3b0c442…
        let f = fingerprint_sha256(b"");
        assert!(f.starts_with("E3:B0:C4:42:98:FC"));
        assert_eq!(f.len(), 32 * 3 - 1);
    }

    #[test]
    fn normalize_round_trip() {
        let f = fingerprint_sha256(b"abc");
        assert_eq!(
            normalize_fingerprint(&f.to_lowercase().replace(':', " ")),
            Some(f.clone())
        );
        assert_eq!(normalize_fingerprint(&f), Some(f));
        assert_eq!(normalize_fingerprint("AB:CD"), None);
    }
}
