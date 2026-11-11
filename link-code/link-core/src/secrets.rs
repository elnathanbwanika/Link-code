//! API-key storage.
//!
//! Keys are held in the OS keyring through **libsecret** (GNOME Keyring /
//! KWallet via the Secret Service D-Bus API). They are never written to disk in
//! plain text, never logged, and never included in an exported model config.
//!
//! When no keyring is reachable (headless CI, a container without a session
//! bus) the app degrades to an in-memory store for the running session and
//! warns the user, rather than silently writing a plaintext file.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::error::Result;

/// Namespace prefix used for every Link Code secret.
pub const KEY_PREFIX: &str = "model:";

/// Secret lookup key for a model configuration id.
pub fn key_for(model_id: &str) -> String {
    format!("{KEY_PREFIX}{model_id}")
}

/// Abstract keyring. Implementations must never persist plaintext to disk.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
    /// Keys currently known to this store (best effort).
    fn keys(&self) -> Vec<String>;
    /// Convenience: look up an API key for a model configuration id.
    fn get_model_key(&self, model_id: &str) -> Option<String> {
        self.get(&key_for(model_id))
    }
    fn set_model_key(&self, model_id: &str, value: &str) -> Result<()> {
        self.set(&key_for(model_id), value)
    }
    fn delete_model_key(&self, model_id: &str) -> Result<()> {
        self.delete(&key_for(model_id))
    }
}

/// Session-only store. Used by tests and as the fallback when no keyring is
/// available. It never touches the filesystem.
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<BTreeMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let store = Self::new();
        {
            let mut map = store.map.lock().expect("secret mutex poisoned");
            for (key, value) in pairs {
                map.insert(key, value);
            }
        }
        store
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map
            .lock()
            .expect("secret mutex poisoned")
            .get(key)
            .cloned()
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.map
            .lock()
            .expect("secret mutex poisoned")
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.map.lock().expect("secret mutex poisoned").remove(key);
        Ok(())
    }

    fn keys(&self) -> Vec<String> {
        self.map
            .lock()
            .expect("secret mutex poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

/// The store the app should use. Uses the keyring when the `keyring` feature is
/// compiled in *and* a Secret Service is reachable; otherwise an in-memory
/// store for this session.
pub fn default_store() -> Arc<dyn SecretStore> {
    #[cfg(feature = "keyring")]
    {
        if keyring::LibSecretStore::is_available() {
            return Arc::new(keyring::LibSecretStore::new());
        }
        tracing::warn!(
            "no Secret Service keyring reachable; API keys will be kept in memory for this session only"
        );
    }
    Arc::new(MemoryStore::new())
}

#[cfg(feature = "keyring")]
pub mod keyring {
    //! libsecret-backed implementation.
    use super::{Result, SecretStore};
    use crate::error::LinkError;
    use std::collections::HashMap;

    const SCHEMA_NAME: &str = "org.linkecosystem.LinkCode";

    /// GNOME Keyring / KWallet implementation.
    #[derive(Debug, Default)]
    pub struct LibSecretStore;

    impl LibSecretStore {
        pub fn new() -> Self {
            Self
        }

        /// True when a Secret Service is present on the session bus.
        pub fn is_available() -> bool {
            libsecret::service().is_some()
        }

        fn schema() -> libsecret::Schema {
            libsecret::Schema::new(
                SCHEMA_NAME,
                libsecret::SchemaFlags::NONE,
                &[("model", libsecret::SchemaAttributeType::String)],
            )
        }

        fn attributes(key: &str) -> HashMap<&str, &str> {
            let mut map = HashMap::new();
            map.insert("model", key);
            map
        }
    }

    impl SecretStore for LibSecretStore {
        fn get(&self, key: &str) -> Option<String> {
            libsecret::password_lookup_sync(Some(&Self::schema()), Self::attributes(key), gio::Cancellable::NONE)
                .ok()
                .flatten()
                .map(|value| value.to_string())
        }

        fn set(&self, key: &str, value: &str) -> Result<()> {
            libsecret::password_store_sync(
                Some(&Self::schema()),
                Self::attributes(key),
                None,
                &format!("Link Code API key for {key}"),
                value,
                gio::Cancellable::NONE,
            )
            .map_err(|e| LinkError::other(format!("keyring refused to store the API key: {e}")))
        }

        fn delete(&self, key: &str) -> Result<()> {
            libsecret::password_clear_sync(Some(&Self::schema()), Self::attributes(key), gio::Cancellable::NONE)
                .map(|_| ())
                .map_err(|e| LinkError::other(format!("keyring refused to delete the API key: {e}")))
        }

        fn keys(&self) -> Vec<String> {
            // libsecret has no cheap enumeration for a schema; the model
            // registry is the source of truth for which keys exist.
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::new();
        assert!(store.get("missing").is_none());
        store.set("a", "1").unwrap();
        assert_eq!(store.get("a").as_deref(), Some("1"));
        store.delete("a").unwrap();
        assert!(store.get("a").is_none());
    }

    #[test]
    fn model_keys_are_namespaced() {
        let store = MemoryStore::new();
        store.set_model_key("ollama-local", "secret").unwrap();
        assert_eq!(store.get_model_key("ollama-local").as_deref(), Some("secret"));
        assert!(store.get("ollama-local").is_none());
        assert_eq!(store.keys(), vec!["model:ollama-local".to_string()]);
    }

    #[test]
    fn default_store_never_panics_without_a_keyring() {
        let store = default_store();
        let _ = store.keys();
    }
}
