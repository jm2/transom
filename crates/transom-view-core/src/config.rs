// SPDX-License-Identifier: Apache-2.0
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

/// Which way the clipboard is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardMode {
    #[default]
    Both,
    ToRemote,
    FromRemote,
    Off,
}

impl ClipboardMode {
    pub fn to_remote(self) -> bool {
        matches!(self, Self::Both | Self::ToRemote)
    }

    pub fn from_remote(self) -> bool {
        matches!(self, Self::Both | Self::FromRemote)
    }
}

/// Where remote audio plays (`audiomode` in `.rdp` files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioMode {
    /// Play on this computer.
    #[default]
    Local,
    /// Leave it playing on the remote computer.
    Remote,
    Off,
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
    pub clipboard: ClipboardMode,
    pub audio: AudioMode,
    /// Redirect the local microphone (off by default).
    pub microphone: bool,
    /// Authenticate with the Kerberos ticket cache (single sign-on): no password is
    /// asked for; NLA uses the ticket for `username@domain`.
    pub kerberos: bool,
    /// Initial desktop request, computed from the widget size by the caller.
    pub initial: DesktopRequest,
    /// Multi-monitor layout (see `monitors::layout`). Empty or one entry: a single
    /// monitor described by `initial`.
    pub monitors: Vec<crate::monitors::RemoteMonitor>,
    /// Tunnel through an RD Gateway (MS-TSGU).
    pub gateway: Option<GatewayConfig>,
    /// H.264 decoding: hardware to dmabufs where available (ADR 0006).
    pub video_decode: crate::video::VideoDecode,
    /// Local devices offered to the session (drives, smart cards, printers, serial
    /// ports, USB). The server decides what it accepts.
    pub redirections: crate::devices::Redirections,
}

/// RD Gateway to connect through. The gateway's TLS certificate goes through the same
/// trust store and prompt as the server's (keyed by the gateway's host and port).
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub host: String,
    /// Default 443.
    pub port: u16,
    pub username: Option<String>,
    pub domain: Option<String>,
    /// Never logged; never put on argv or in the environment.
    pub password: Option<SecretString>,
    /// Authenticate to the gateway with the session's credentials.
    pub same_credentials: bool,
}

impl GatewayConfig {
    pub const DEFAULT_PORT: u16 = 443;
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
