// SPDX-License-Identifier: Apache-2.0
//! `.rdp` connection files and `rdp://` URIs.
//!
//! An `.rdp` file is a list of `key:type:value` lines, where type is `s` (string), `i`
//! (integer) or `b` (hex binary). Keys are case-insensitive. mstsc writes UTF-16LE with
//! a BOM; other tools write UTF-8. Unknown keys are kept, so a file round-trips.
//!
//! `rdp://` comes in two flavours:
//! - Microsoft: `rdp://full%20address=s:host:3389&username=s:user` (`key=type:value`,
//!   `&`-separated and percent-encoded);
//! - simple: `rdp://[user[;domain]@]host[:port]` (Remmina, GNOME Connections).

use std::collections::HashMap;
use std::fmt;

/// Largest `.rdp` file or `rdp://` URI accepted. Real files are a few kilobytes; the
/// cap keeps a hostile file from tying up the UI thread.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Most distinct settings kept from one file (mstsc knows about 150).
pub const MAX_ENTRIES: usize = 2048;

/// A typed value from an `.rdp` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdpValue {
    Str(String),
    Int(i64),
    /// Hex-encoded binary, kept verbatim.
    Bin(String),
}

impl RdpValue {
    fn type_char(&self) -> char {
        match self {
            Self::Str(_) => 's',
            Self::Int(_) => 'i',
            Self::Bin(_) => 'b',
        }
    }
}

impl fmt::Display for RdpValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(s) | Self::Bin(s) => f.write_str(s),
            Self::Int(i) => write!(f, "{i}"),
        }
    }
}

/// Errors reading `.rdp` files or URIs.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RdpFileError {
    #[error("file is not valid UTF-8 or UTF-16")]
    Encoding,
    #[error("not an rdp:// URI")]
    NotRdpUri,
    #[error("invalid URI: {0}")]
    BadUri(String),
    #[error("too large (more than {} KiB)", MAX_INPUT_BYTES / 1024)]
    TooLarge,
}

/// An ordered, case-insensitive set of `.rdp` settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RdpFile {
    /// `(lower-case key, value)` in file order.
    entries: Vec<(String, RdpValue)>,
}

impl RdpFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse file bytes: UTF-16LE/BE (with or without BOM) or UTF-8 (with or without BOM).
    pub fn parse_bytes(bytes: &[u8]) -> Result<Self, RdpFileError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(RdpFileError::TooLarge);
        }
        Ok(Self::parse_str(&decode_text(bytes)?))
    }

    /// Parse text. Malformed lines (and lines with control characters) are skipped, as
    /// mstsc skips malformed lines; at most [`MAX_ENTRIES`] distinct keys are kept.
    pub fn parse_str(text: &str) -> Self {
        let mut f = Self::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for line in text.lines() {
            let Some((k, v)) = parse_line(line.trim_end_matches('\r')) else {
                continue;
            };
            let k = k.to_ascii_lowercase();
            match index.get(&k) {
                Some(&i) => f.entries[i].1 = v,
                None if f.entries.len() < MAX_ENTRIES => {
                    index.insert(k.clone(), f.entries.len());
                    f.entries.push((k, v));
                }
                None => {}
            }
        }
        f
    }

    pub fn get(&self, key: &str) -> Option<&RdpValue> {
        let key = key.to_ascii_lowercase();
        self.entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// A string value, or an integer rendered as a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            RdpValue::Str(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            RdpValue::Int(i) => Some(*i),
            RdpValue::Str(s) => s.trim().parse().ok(),
            RdpValue::Bin(_) => None,
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get_int(key).map(|i| i != 0)
    }

    /// Insert or replace, keeping the original position of an existing key.
    pub fn set(&mut self, key: &str, value: RdpValue) {
        let key = key.to_ascii_lowercase();
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(e) => e.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    /// Control characters are dropped: a CR or LF in a value (a user name, say) would
    /// otherwise add lines of its own to an exported file.
    pub fn set_str(&mut self, key: &str, value: impl Into<String>) {
        let mut v: String = value.into();
        v.retain(|c| !c.is_control() || c == '\t');
        self.set(key, RdpValue::Str(v));
    }

    pub fn set_int(&mut self, key: &str, value: i64) {
        self.set(key, RdpValue::Int(value));
    }

    pub fn remove(&mut self, key: &str) -> Option<RdpValue> {
        let key = key.to_ascii_lowercase();
        let i = self.entries.iter().position(|(k, _)| *k == key)?;
        Some(self.entries.remove(i).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &RdpValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Serialise as text with CRLF line endings.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.entries {
            s.push_str(&format!("{k}:{}:{v}\r\n", v.type_char()));
        }
        s
    }

    /// Serialise the way mstsc writes files: UTF-16LE with a BOM.
    pub fn to_utf16le_bytes(&self) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        for u in self.to_text().encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    /// Parse an `rdp://` URI (either flavour, see module docs) into settings.
    pub fn from_uri(uri: &str) -> Result<Self, RdpFileError> {
        let rest = uri
            .strip_prefix("rdp://")
            .or_else(|| uri.strip_prefix("RDP://"))
            .ok_or(RdpFileError::NotRdpUri)?;
        if uri.len() > MAX_INPUT_BYTES {
            return Err(RdpFileError::TooLarge);
        }
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            return Err(RdpFileError::BadUri("empty".into()));
        }
        // Microsoft flavour: `key=type:value` parts.
        let uri_setting = |part: &str| -> Result<Option<(String, RdpValue)>, RdpFileError> {
            let part = percent_decode(part)?;
            Ok(part
                .split_once('=')
                .and_then(|(k, rest)| parse_line(&format!("{k}:{rest}"))))
        };
        let mut f = Self::new();
        if uri_setting(rest.split('&').next().unwrap_or(""))?.is_some() {
            for part in rest.split('&').filter(|p| !p.is_empty()) {
                let (k, v) = uri_setting(part)?
                    .ok_or_else(|| RdpFileError::BadUri(format!("bad setting {part:?}")))?;
                f.set(&k, v);
            }
            return Ok(f);
        }
        // Simple flavour: [user[;domain]@]host[:port], query string ignored.
        let authority = rest.split(['?', '/']).next().unwrap_or("");
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(percent_decode(u)?), h),
            None => (None, authority),
        };
        if userinfo.as_deref().is_some_and(has_control) || has_control(hostport) {
            return Err(RdpFileError::BadUri("control characters".into()));
        }
        let (host, port) = split_host_port(hostport)
            .ok_or_else(|| RdpFileError::BadUri(format!("bad host {hostport:?}")))?;
        f.set_str("full address", join_host_port(&host, port));
        if let Some(u) = userinfo.filter(|u| !u.is_empty()) {
            match u.split_once(';') {
                Some((user, domain)) => {
                    f.set_str("username", user);
                    f.set_str("domain", domain);
                }
                None => f.set_str("username", u),
            }
        }
        Ok(f)
    }

    /// `(host, port)` from `full address` (and `server port`), defaulting to 3389.
    pub fn address(&self) -> Option<(String, u16)> {
        let full = self.get_str("full address")?;
        let (host, port) = split_host_port(full)?;
        let port = port
            .or_else(|| {
                self.get_int("server port")
                    .and_then(|p| u16::try_from(p).ok())
            })
            .unwrap_or(crate::config::DEFAULT_PORT);
        Some((host, port))
    }
}

/// Control characters other than tab. A value with a CR or LF could otherwise smuggle
/// extra settings into an exported file.
fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control() && c != '\t')
}

/// `key:type:value` → `(key, value)`.
fn parse_line(line: &str) -> Option<(String, RdpValue)> {
    if has_control(line) {
        return None;
    }
    let (key, rest) = line.split_once(':')?;
    let (ty, value) = rest.split_once(':')?;
    let key = key.trim();
    if key.is_empty() || ty.len() != 1 {
        return None;
    }
    let value = match ty {
        "s" | "S" => RdpValue::Str(value.to_string()),
        "i" | "I" => RdpValue::Int(value.trim().parse().ok()?),
        "b" | "B" => RdpValue::Bin(value.trim().to_string()),
        _ => return None,
    };
    Some((key.to_string(), value))
}

/// Split `host`, `host:port`, `[v6]` or `[v6]:port`. Bare IPv6 without brackets is
/// taken as a host without port.
pub fn split_host_port(s: &str) -> Option<(String, Option<u16>)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        return Some((host.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Some((h.to_string(), Some(p.parse().ok()?))),
        _ => Some((s.to_string(), None)),
    }
}

/// Whether `host` is a computer name or an IP address, and nothing else.
///
/// FreeRDP gives some host strings special meanings: one starting with `/` is a Unix
/// socket path, one starting with `|` makes the port number an already-open file
/// descriptor (which would be one of Transom's own), and `vsock://…` is a VM socket.
/// Host names come from files and links, so accept only DNS or NetBIOS names (ASCII
/// or Unicode letters and digits, `-`, `_` and dots between labels) and IP literals
/// (IPv6 optionally in brackets, with a zone).
pub fn valid_host(host: &str) -> bool {
    let h = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    if h.contains(':') {
        let (addr, zone) = match h.split_once('%') {
            Some((a, z)) => (a, Some(z)),
            None => (h, None),
        };
        return addr.parse::<std::net::Ipv6Addr>().is_ok()
            && zone.is_none_or(|z| {
                !z.is_empty()
                    && z.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            });
    }
    // An IPv4 literal is also a valid name by these rules.
    h.split('.').all(|label| {
        let mut chars = label.chars();
        chars.next().is_some_and(char::is_alphanumeric)
            && chars.all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }) || h.ends_with('.') && !h.ends_with("..") && valid_host(&h[..h.len() - 1])
}

/// Inverse of [`split_host_port`]; IPv6 literals get brackets, port 3389 is implied.
pub fn join_host_port(host: &str, port: Option<u16>) -> String {
    let h = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    match port {
        Some(p) if p != crate::config::DEFAULT_PORT => format!("{h}:{p}"),
        _ => h,
    }
}

fn percent_decode(s: &str) -> Result<String, RdpFileError> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = s
                    .get(i + 1..i + 3)
                    .ok_or_else(|| RdpFileError::BadUri("truncated %-escape".into()))?;
                out.push(
                    u8::from_str_radix(hex, 16)
                        .map_err(|_| RdpFileError::BadUri(format!("bad %-escape %{hex}")))?,
                );
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| RdpFileError::BadUri("not UTF-8".into()))
}

fn decode_text(bytes: &[u8]) -> Result<String, RdpFileError> {
    let utf16 = |b: &[u8], le: bool| -> Result<String, RdpFileError> {
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| {
                if le {
                    u16::from_le_bytes(c)
                } else {
                    u16::from_be_bytes(c)
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| RdpFileError::Encoding)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => {
            String::from_utf8(rest.to_vec()).map_err(|_| RdpFileError::Encoding)
        }
        // BOM-less UTF-16: ASCII text has every other byte zero.
        [a, 0, ..] if *a != 0 => utf16(bytes, true),
        [0, b, ..] if *b != 0 => utf16(bytes, false),
        _ => String::from_utf8(bytes.to_vec()).map_err(|_| RdpFileError::Encoding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSTSC: &str = "screen mode id:i:2\r\nuse multimon:i:0\r\ndesktopwidth:i:1920\r\n\
        desktopheight:i:1080\r\nsession bpp:i:32\r\nfull address:s:antec-win.lan:3390\r\n\
        audiomode:i:0\r\nredirectclipboard:i:1\r\nusername:s:CORP\\jm\r\n\
        gatewayhostname:s:\r\nprompt for credentials:i:0\r\nauthentication level:i:2\r\n\
        desktopscalefactor:i:200\r\nsmart sizing:i:1\r\ndynamic resolution:i:1\r\n\
        password 51:b:01000000D08C9DDF0115D1118C7A00C04FC297EB\r\nweird line\r\n";

    #[test]
    fn valid_hosts() {
        for h in [
            "pc",
            "pc.corp.example",
            "pc.corp.example.",
            "10.0.0.1",
            "fe80::1",
            "[fe80::1]",
            "fe80::1%eth0",
            "my_host-2",
            "bücher.example",
            "WIN-SRV01",
        ] {
            assert!(valid_host(h), "{h}");
        }
        for h in [
            "",
            "|5",
            "/run/user/1000/bus",
            "vsock://2",
            "pc corp",
            ".pc",
            "pc..corp",
            "-pc",
            "pc\u{202e}moc",
            "pc\u{200b}",
            "fe80::1%",
            "[fe80::1",
            "a:b",
            "pc/x",
            &"a".repeat(254),
        ] {
            assert!(!valid_host(h), "{h:?}");
        }
    }

    #[test]
    fn parses_mstsc_utf16() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in MSTSC.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let f = RdpFile::parse_bytes(&bytes).unwrap();
        assert_eq!(f.address(), Some(("antec-win.lan".into(), 3390)));
        assert_eq!(f.get_str("Username"), Some("CORP\\jm"));
        assert_eq!(f.get_int("desktopscalefactor"), Some(200));
        assert_eq!(f.get_bool("redirectclipboard"), Some(true));
        assert_eq!(f.get_str("gatewayhostname"), None); // empty
        assert!(matches!(f.get("password 51"), Some(RdpValue::Bin(_))));
        assert_eq!(f.iter().count(), 16); // "weird line" skipped
    }

    #[test]
    fn bomless_and_utf8() {
        let le: Vec<u8> = "full address:s:h\r\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(
            RdpFile::parse_bytes(&le).unwrap().address(),
            Some(("h".into(), 3389))
        );
        let u8b = b"\xEF\xBB\xBFfull address:s:[fe80::1]:4000\n";
        assert_eq!(
            RdpFile::parse_bytes(u8b).unwrap().address(),
            Some(("fe80::1".into(), 4000))
        );
        assert_eq!(
            RdpFile::parse_bytes(&[0xFF, 0xFE, 0x00, 0xD8]),
            Err(RdpFileError::Encoding)
        );
    }

    #[test]
    fn round_trip_preserves_everything() {
        let f = RdpFile::parse_str(MSTSC);
        let again = RdpFile::parse_bytes(&f.to_utf16le_bytes()).unwrap();
        assert_eq!(f, again);
    }

    #[test]
    fn set_replaces_in_place() {
        let mut f = RdpFile::parse_str("a:i:1\r\nb:i:2\r\n");
        f.set_int("A", 5);
        assert_eq!(f.to_text(), "a:i:5\r\nb:i:2\r\n");
    }

    #[test]
    fn microsoft_uri() {
        let f = RdpFile::from_uri(
            "rdp://full%20address=s:mypc.example.com:3389&audiomode=i:2&username=s:jm",
        )
        .unwrap();
        assert_eq!(f.address(), Some(("mypc.example.com".into(), 3389)));
        assert_eq!(f.get_int("audiomode"), Some(2));
        assert_eq!(f.get_str("username"), Some("jm"));
    }

    #[test]
    fn simple_uri() {
        let f = RdpFile::from_uri("rdp://jm%40corp;CORP@[2001:db8::5]:3390/").unwrap();
        assert_eq!(f.address(), Some(("2001:db8::5".into(), 3390)));
        assert_eq!(f.get_str("username"), Some("jm@corp"));
        assert_eq!(f.get_str("domain"), Some("CORP"));
        let f = RdpFile::from_uri("rdp://host").unwrap();
        assert_eq!(f.address(), Some(("host".into(), 3389)));
        assert_eq!(
            RdpFile::from_uri("vnc://host"),
            Err(RdpFileError::NotRdpUri)
        );
        assert!(RdpFile::from_uri("rdp://host:99999").is_err());
        assert!(RdpFile::from_uri("rdp://%zz@host").is_err());
    }

    #[test]
    fn host_port_helpers() {
        assert_eq!(split_host_port("h:1"), Some(("h".into(), Some(1))));
        assert_eq!(split_host_port("::1"), Some(("::1".into(), None)));
        assert_eq!(split_host_port("[::1]x"), None);
        assert_eq!(join_host_port("::1", Some(3389)), "[::1]");
        assert_eq!(join_host_port("h", Some(4000)), "h:4000");
    }

    #[test]
    fn hostile_input_is_bounded() {
        // Lines with control characters are dropped, so a value can't carry extra lines.
        let f = RdpFile::parse_str("full address:s:pc\nusername:s:bob\u{1b}x\n");
        assert_eq!(f.get_str("username"), None);
        assert_eq!(f.get_str("full address"), Some("pc"));
        // A percent-encoded CRLF in a link is rejected, not decoded into a new setting.
        let uri = "rdp://full%20address=s:pc&username=s:bob%0D%0Adrivestoredirect:s:*";
        assert!(RdpFile::from_uri(uri).is_err());
        assert!(RdpFile::from_uri("rdp://bob%0A@pc").is_err());
        // Size and entry caps.
        assert_eq!(
            RdpFile::parse_bytes(&vec![b'a'; MAX_INPUT_BYTES + 1]),
            Err(RdpFileError::TooLarge)
        );
        let many: String = (0..MAX_ENTRIES + 10)
            .map(|i| format!("k{i}:i:1\n"))
            .collect();
        assert_eq!(RdpFile::parse_str(&many).iter().count(), MAX_ENTRIES);
        // Repeated keys keep their first position and the last value.
        let f = RdpFile::parse_str("a:i:1\nb:i:2\nA:i:3\n");
        assert_eq!(f.iter().map(|(k, _)| k).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(f.get_int("a"), Some(3));
    }
}
