// SPDX-License-Identifier: GPL-3.0-or-later
//! Server certificate identity. The trust *store* (TOFU persistence) arrives in
//! Phase 1/2; this module defines what we show and compare.

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
