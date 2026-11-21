// SPDX-License-Identifier: GPL-3.0-or-later
//! Password storage in the OS secret store (ADR 0002): libsecret on Linux. macOS
//! (Keychain) and Windows (Credential Manager) are not implemented yet; there,
//! passwords are simply not remembered.

use transom_view::core::secrecy::SecretString;

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    use transom_view::core::secrecy::{ExposeSecret, SecretString};

    fn schema() -> libsecret::Schema {
        let mut attrs = HashMap::new();
        attrs.insert("profile", libsecret::SchemaAttributeType::String);
        libsecret::Schema::new(
            "io.github.jm2.Transom.Password",
            libsecret::SchemaFlags::NONE,
            attrs,
        )
    }

    fn attrs(profile_id: &str) -> HashMap<&str, &str> {
        HashMap::from([("profile", profile_id)])
    }

    pub async fn lookup(profile_id: &str) -> Option<SecretString> {
        match libsecret::password_lookup_future(Some(&schema()), attrs(profile_id)).await {
            Ok(p) => p.map(|p| SecretString::from(p.to_string())),
            Err(e) => {
                tracing::warn!("secret lookup failed: {e}");
                None
            }
        }
    }

    pub async fn store(
        profile_id: &str,
        label: &str,
        password: &SecretString,
    ) -> Result<(), String> {
        libsecret::password_store_future(
            Some(&schema()),
            attrs(profile_id),
            Some(libsecret::COLLECTION_DEFAULT),
            label,
            password.expose_secret(),
        )
        .await
        .map_err(|e| e.to_string())
    }

    pub async fn clear(profile_id: &str) {
        if let Err(e) = libsecret::password_clear_future(Some(&schema()), attrs(profile_id)).await {
            tracing::warn!("secret clear failed: {e}");
        }
    }

    pub const AVAILABLE: bool = true;
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use transom_view::core::secrecy::SecretString;

    pub async fn lookup(_profile_id: &str) -> Option<SecretString> {
        None
    }

    pub async fn store(_: &str, _: &str, _: &SecretString) -> Result<(), String> {
        Err("saving passwords isn't supported on this platform yet".into())
    }

    pub async fn clear(_profile_id: &str) {}

    pub const AVAILABLE: bool = false;
}

/// Whether passwords can be remembered on this platform.
pub const AVAILABLE: bool = imp::AVAILABLE;

pub async fn lookup(profile_id: &str) -> Option<SecretString> {
    imp::lookup(profile_id).await
}

pub async fn store(profile_id: &str, label: &str, password: &SecretString) -> Result<(), String> {
    imp::store(profile_id, label, password).await
}

pub async fn clear(profile_id: &str) {
    imp::clear(profile_id).await
}
