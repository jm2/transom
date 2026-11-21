// SPDX-License-Identifier: GPL-3.0-or-later
//! Connection profiles: the app's saved connections, stored as one TOML file each.
//!
//! Secrets are never part of a profile; the app keeps them in the OS secret store.

use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::config::{CertPolicy, DEFAULT_PORT, SecurityMode, SessionConfig};
use crate::geometry::{FitMode, LogicalSize, ScaleMode, desktop_request};
use crate::rdpfile::{RdpFile, RdpValue, join_host_port};

pub use crate::config::ClipboardMode;

pub use crate::config::AudioMode;

/// RD Gateway settings (used from Phase 4).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Gateway {
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Reuse the server credentials for the gateway.
    pub same_credentials: bool,
}

/// A saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Stable identifier; also the file name. `[a-z0-9-]` only.
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub scale: ScaleMode,
    pub fit: FitMode,
    pub security: SecurityMode,
    pub certificate: CertPolicy,
    pub clipboard: ClipboardMode,
    pub audio: AudioMode,
    pub microphone: bool,
    pub multimon: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Gateway>,
    /// `.rdp` settings Transom doesn't model, kept for export (`key` → `type:value`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub rdp_extra: BTreeMap<String, String>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            host: String::new(),
            port: DEFAULT_PORT,
            username: None,
            domain: None,
            scale: ScaleMode::default(),
            fit: FitMode::default(),
            security: SecurityMode::default(),
            certificate: CertPolicy::default(),
            clipboard: ClipboardMode::default(),
            audio: AudioMode::default(),
            microphone: false,
            multimon: false,
            gateway: None,
            rdp_extra: BTreeMap::new(),
        }
    }
}

/// `.rdp` keys mapped onto profile fields (everything else goes to `rdp_extra`).
const MODELLED_KEYS: &[&str] = &[
    "full address",
    "server port",
    "username",
    "domain",
    "redirectclipboard",
    "audiomode",
    "audiocapturemode",
    "use multimon",
    "smart sizing",
    "enablecredsspsupport",
    "gatewayhostname",
    "gatewayusagemethod",
    "gatewaycredentialssource",
    "promptcredentialonce",
    // Machine-specific values from whoever saved the file; we compute our own.
    "desktopwidth",
    "desktopheight",
    "desktopscalefactor",
    "winposstr",
    "session bpp",
    // Never imported (secrets, or policy we won't take from a file).
    "password 51",
    "authentication level",
];

/// A random-enough local identifier (std's per-process random hasher keys, the
/// time and a counter); profiles are local files, not a security boundary.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    format!("{:016x}", h.finish())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Profile {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            ..Default::default()
        }
    }

    /// What to show in lists: the name, or the address if unnamed.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.address()
        } else {
            self.name.clone()
        }
    }

    /// `host[:port]`, IPv6 bracketed, default port omitted.
    pub fn address(&self) -> String {
        join_host_port(&self.host, Some(self.port))
    }

    /// Build a profile from `.rdp` settings (a file or an `rdp://` URI).
    ///
    /// Deliberately not imported: saved passwords and `authentication level` (a file
    /// must never be able to turn off certificate checks).
    pub fn from_rdp(f: &RdpFile) -> Option<Self> {
        let (host, port) = f.address()?;
        let mut p = Profile {
            port,
            ..Profile::new(String::new(), host)
        };
        let (mut user, mut domain) = (
            f.get_str("username").map(str::to_string),
            f.get_str("domain").map(str::to_string),
        );
        // "DOMAIN\user" is how mstsc stores domain accounts.
        if let Some((d, u)) = user.as_deref().and_then(|u| u.split_once('\\')) {
            domain.get_or_insert_with(|| d.to_string());
            user = Some(u.to_string());
        }
        p.username = user;
        p.domain = domain;
        p.clipboard = match f.get_bool("redirectclipboard") {
            Some(false) => ClipboardMode::Off,
            _ => ClipboardMode::Both,
        };
        p.audio = match f.get_int("audiomode") {
            Some(1) => AudioMode::Remote,
            Some(2) => AudioMode::Off,
            _ => AudioMode::Local,
        };
        p.microphone = f.get_bool("audiocapturemode").unwrap_or(false);
        p.multimon = f.get_bool("use multimon").unwrap_or(false);
        p.fit = FitMode::Fit;
        if f.get_bool("enablecredsspsupport") == Some(false) {
            p.security = SecurityMode::Tls;
        }
        if let Some(gw) = f.get_str("gatewayhostname")
            && f.get_int("gatewayusagemethod").unwrap_or(1) != 0
        {
            let (gh, gp) = crate::rdpfile::split_host_port(gw).unwrap_or((gw.to_string(), None));
            p.gateway = Some(Gateway {
                host: gh,
                port: gp,
                username: None,
                same_credentials: f.get_bool("promptcredentialonce").unwrap_or(true),
            });
        }
        for (k, v) in f.iter() {
            if !MODELLED_KEYS.contains(&k) {
                let ty = match v {
                    RdpValue::Str(_) => 's',
                    RdpValue::Int(_) => 'i',
                    RdpValue::Bin(_) => 'b',
                };
                p.rdp_extra.insert(k.to_string(), format!("{ty}:{v}"));
            }
        }
        Some(p)
    }

    /// Export as `.rdp` settings (unmodelled keys from the import are kept).
    pub fn to_rdp(&self) -> RdpFile {
        let mut f = RdpFile::new();
        f.set_str("full address", self.address());
        if let Some(u) = &self.username {
            f.set_str("username", u);
        }
        if let Some(d) = &self.domain {
            f.set_str("domain", d);
        }
        f.set_int(
            "redirectclipboard",
            (self.clipboard != ClipboardMode::Off) as i64,
        );
        f.set_int(
            "audiomode",
            match self.audio {
                AudioMode::Local => 0,
                AudioMode::Remote => 1,
                AudioMode::Off => 2,
            },
        );
        f.set_int("audiocapturemode", self.microphone as i64);
        f.set_int("use multimon", self.multimon as i64);
        f.set_int("dynamic resolution", 1);
        if self.security == SecurityMode::Tls {
            f.set_int("enablecredsspsupport", 0);
        }
        if let Some(g) = &self.gateway {
            f.set_str(
                "gatewayhostname",
                join_host_port(&g.host, g.port.or(Some(443))),
            );
            f.set_int("gatewayusagemethod", 1);
            f.set_int("promptcredentialonce", g.same_credentials as i64);
        }
        for (k, tv) in &self.rdp_extra {
            if f.get(k).is_none()
                && let Some((k2, v)) = RdpFile::parse_str(&format!("{k}:{tv}")).iter().next()
            {
                f.set(k2, v.clone());
            }
        }
        f
    }

    /// The engine configuration for this profile. `initial` is a placeholder; the
    /// view replaces it with a request computed from its size and scale.
    pub fn session_config(&self, password: Option<SecretString>) -> SessionConfig {
        SessionConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            domain: self.domain.clone(),
            password,
            security: self.security,
            cert_policy: self.certificate,
            scale_mode: self.scale,
            fit_mode: self.fit,
            clipboard: self.clipboard,
            audio: self.audio,
            microphone: self.microphone,
            initial: desktop_request(self.scale, LogicalSize::new(1024.0, 768.0), 1.0),
        }
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// Errors loading one profile file.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid profile id {0:?}")]
    BadId(String),
}

/// A directory of `<id>.toml` profile files.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    dir: PathBuf,
}

impl ProfileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, id: &str) -> Result<PathBuf, ProfileError> {
        if valid_id(id) {
            Ok(self.dir.join(format!("{id}.toml")))
        } else {
            Err(ProfileError::BadId(id.to_string()))
        }
    }

    /// Load every profile, sorted by display name. Broken files are reported, not
    /// fatal. A missing directory is an empty store.
    pub fn load_all(&self) -> (Vec<Profile>, Vec<ProfileError>) {
        let mut profiles = Vec::new();
        let mut errors = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (profiles, errors),
            Err(source) => {
                errors.push(ProfileError::Io {
                    path: self.dir.clone(),
                    source,
                });
                return (profiles, errors);
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !valid_id(stem) {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => match Profile::from_toml(&text) {
                    Ok(mut p) => {
                        p.id = stem.to_string(); // the file name is authoritative
                        profiles.push(p);
                    }
                    Err(source) => errors.push(ProfileError::Parse { path, source }),
                },
                Err(source) => errors.push(ProfileError::Io { path, source }),
            }
        }
        profiles.sort_by_key(|p| p.display_name().to_lowercase());
        (profiles, errors)
    }

    pub fn save(&self, profile: &Profile) -> Result<(), ProfileError> {
        let path = self.path_for(&profile.id)?;
        crate::fsutil::write_atomic(&path, profile.to_toml().as_bytes())
            .map_err(|source| ProfileError::Io { path, source })
    }

    pub fn delete(&self, id: &str) -> Result<(), ProfileError> {
        let path = self.path_for(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ProfileError::Io { path, source }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_round_trip() {
        let mut p = Profile::new("Work PC", "fe80::1");
        p.port = 3390;
        p.username = Some("jm".into());
        p.scale = ScaleMode::Fixed(150);
        p.clipboard = ClipboardMode::ToRemote;
        p.gateway = Some(Gateway {
            host: "gw.example.com".into(),
            ..Default::default()
        });
        p.rdp_extra.insert("keyboardhook".into(), "i:2".into());
        let text = p.to_toml();
        assert!(text.contains("clipboard = \"to-remote\""), "{text}");
        assert_eq!(Profile::from_toml(&text).unwrap(), p);
    }

    #[test]
    fn minimal_toml_gets_defaults() {
        let p = Profile::from_toml("host = \"h\"\nscale = \"legacy\"\n").unwrap();
        assert_eq!(p.port, 3389);
        assert_eq!(p.scale, ScaleMode::Legacy100);
        assert_eq!(p.certificate, CertPolicy::Verify);
        assert!(Profile::from_toml("scale = \"bogus\"").is_err());
    }

    #[test]
    fn from_mstsc_file() {
        let f = RdpFile::parse_str(
            "full address:s:pc.corp:3390\r\nusername:s:CORP\\jm\r\naudiomode:i:2\r\n\
             redirectclipboard:i:0\r\nauthentication level:i:0\r\nkeyboardhook:i:2\r\n\
             password 51:b:0100\r\ngatewayhostname:s:gw.corp\r\ngatewayusagemethod:i:1\r\n",
        );
        let p = Profile::from_rdp(&f).unwrap();
        assert_eq!((p.host.as_str(), p.port), ("pc.corp", 3390));
        assert_eq!(p.username.as_deref(), Some("jm"));
        assert_eq!(p.domain.as_deref(), Some("CORP"));
        assert_eq!(p.audio, AudioMode::Off);
        assert_eq!(p.clipboard, ClipboardMode::Off);
        assert_eq!(
            p.certificate,
            CertPolicy::Verify,
            "never import 'connect anyway'"
        );
        assert_eq!(p.gateway.as_ref().map(|g| g.host.as_str()), Some("gw.corp"));
        assert_eq!(
            p.rdp_extra.get("keyboardhook").map(String::as_str),
            Some("i:2")
        );
        assert!(!p.rdp_extra.contains_key("password 51"));

        let back = p.to_rdp();
        assert_eq!(back.address(), Some(("pc.corp".into(), 3390)));
        assert_eq!(back.get_int("keyboardhook"), Some(2));
        assert_eq!(back.get("password 51"), None);
    }

    #[test]
    fn from_uri() {
        let f = RdpFile::from_uri("rdp://jm;CORP@pc.corp").unwrap();
        let p = Profile::from_rdp(&f).unwrap();
        assert_eq!(p.display_name(), "pc.corp");
        assert_eq!(p.domain.as_deref(), Some("CORP"));
    }

    #[test]
    fn store_save_load_delete() {
        let dir = std::env::temp_dir().join(format!("transom-profiles-{}", new_id()));
        let store = ProfileStore::new(&dir);
        assert!(store.load_all().0.is_empty());
        let b = Profile::new("beta", "b");
        let a = Profile::new("Alpha", "a");
        store.save(&b).unwrap();
        store.save(&a).unwrap();
        std::fs::write(dir.join("broken.toml"), "port = \"x\"").unwrap();
        std::fs::write(dir.join("../escape.toml"), "").ok();
        let (list, errors) = store.load_all();
        assert_eq!(
            list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Alpha", "beta"]
        );
        assert_eq!(errors.len(), 1);
        store.delete(&a.id).unwrap();
        store.delete(&a.id).unwrap(); // idempotent
        assert!(matches!(store.delete("../x"), Err(ProfileError::BadId(_))));
        assert_eq!(store.load_all().0.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ids_are_distinct_and_valid() {
        let ids: std::collections::HashSet<_> = (0..1000).map(|_| new_id()).collect();
        assert_eq!(ids.len(), 1000);
        assert!(ids.iter().all(|i| valid_id(i)));
    }
}
