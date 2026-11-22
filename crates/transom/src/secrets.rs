// SPDX-License-Identifier: GPL-3.0-or-later
//! Password storage in the OS secret store (ADR 0002): libsecret on Linux (the
//! Secret Service, or its portal in Flatpak), Keychain on macOS and Credential
//! Manager on Windows (keyring-core's native stores, as in Tributary).

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

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use std::sync::Arc;

    use transom_view::core::secrecy::{ExposeSecret, SecretString};

    const SERVICE: &str = "io.github.jm2.Transom";

    fn native_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
        // Built per operation, so a store that failed to open can be retried.
        #[cfg(target_os = "macos")]
        let s: Arc<keyring_core::CredentialStore> =
            apple_native_keyring_store::keychain::Store::new().map_err(|e| e.to_string())?;
        #[cfg(target_os = "windows")]
        let s: Arc<keyring_core::CredentialStore> =
            windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
        Ok(s)
    }

    fn entry(profile_id: &str) -> Result<keyring_core::Entry, String> {
        native_store()?
            .build(SERVICE, profile_id, None)
            .map_err(|e| e.to_string())
    }

    /// Keychain/Credential Manager calls block (and may show a system prompt):
    /// run them off the GTK main thread.
    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        gtk::gio::spawn_blocking(f).await.ok()
    }

    pub async fn lookup(profile_id: &str) -> Option<SecretString> {
        let id = profile_id.to_string();
        let r = blocking(move || entry(&id)?.get_password().map_err(|e| e.to_string())).await?;
        match r {
            Ok(p) => Some(SecretString::from(p)),
            Err(e) => {
                tracing::debug!("no stored password: {e}");
                None
            }
        }
    }

    pub async fn store(
        profile_id: &str,
        _label: &str,
        password: &SecretString,
    ) -> Result<(), String> {
        let id = profile_id.to_string();
        let pw = SecretString::from(password.expose_secret().to_string());
        blocking(move || {
            entry(&id)?
                .set_password(pw.expose_secret())
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|| Err("password storage task failed".into()))
    }

    pub async fn clear(profile_id: &str) {
        let id = profile_id.to_string();
        let r = blocking(move || match entry(&id)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        })
        .await;
        if let Some(Err(e)) = r {
            tracing::warn!("secret clear failed: {e}");
        }
    }

    pub const AVAILABLE: bool = true;
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    use transom_view::core::secrecy::SecretString;

    pub async fn lookup(_profile_id: &str) -> Option<SecretString> {
        None
    }

    pub async fn store(_: &str, _: &str, _: &SecretString) -> Result<(), String> {
        Err("saving passwords isn't supported on this platform".into())
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
