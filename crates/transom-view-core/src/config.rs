// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-session connection settings handed to an engine.

use secrecy::SecretString;

use crate::geometry::{DesktopRequest, FitMode, ScaleMode};

/// Default RDP port.
pub const DEFAULT_PORT: u16 = 3389;

/// Transport security the client will negotiate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecurityMode {
    /// Let the server choose among NLA, TLS (and nothing weaker).
    #[default]
    Negotiate,
    Nla,
    Tls,
    /// Legacy RDP security. Explicit opt-in only.
    Rdp,
}

/// Certificate policy (PLAN.md §9: default is verify).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CertPolicy {
    /// System CA, then the TOFU store, then ask the user.
    #[default]
    Verify,
    /// Accept anything. Explicit per-profile opt-in, shown with a warning.
    Ignore,
}

/// Everything an engine needs to open one session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub domain: Option<String>,
    /// Never logged; never put on argv or in the environment.
    pub password: Option<SecretString>,
    pub security: SecurityMode,
    pub cert_policy: CertPolicy,
    pub scale_mode: ScaleMode,
    pub fit_mode: FitMode,
    /// Initial desktop request, computed from the widget size by the caller.
    pub initial: DesktopRequest,
}

impl SessionConfig {
    /// `host:port` with IPv6 literals bracketed, for display and logs.
    pub fn address(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}
