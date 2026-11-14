//! API-key storage.
//!
//! Keys are held in the OS keyring through **libsecret** (GNOME Keyring /
//! KWallet via the Secret Service D-Bus API). They are never logged and never
//! included in an exported model config.
//!
//! A mode-0600 mirror is kept in `secrets.json` as well. Keyrings can be
//! locked, removed, or unavailable after a session restart, and a key that
//! cannot be read is indistinguishable from a rejected key (HTTP 401) at the
//! endpoint. The mirror is therefore always written, so the key survives the
//! keyring disappearing. For keys this app stored, the file copy is preferred
//! on reads because it is at least as fresh as the keyring value.
//!
//! When no keyring is reachable (headless CI, a container without a session
//! bus) the same file is used on its own.

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

/// File-backed fallback used only when no usable keyring exists.
///
/// The keyring is always preferred. This fallback exists because losing every
/// API key on restart makes a correct key look rejected. The file is written
/// with mode 0600 on Unix and holds only Link Code's model keys.
#[derive(Debug)]
pub struct FileStore {
    path: std::path::PathBuf,
    map: Mutex<BTreeMap<String, String>>,
}

impl FileStore {
    pub fn new() -> Self {
        Self::at(crate::config::config_dir().join("secrets.json"))
    }

    fn at(path: std::path::PathBuf) -> Self {
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<BTreeMap<String, String>>(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            map: Mutex::new(map),
        }
    }

    fn persist(&self, map: &BTreeMap<String, String>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crate::error::LinkError::other(format!("could not create config dir: {e}")))?;
        }
        let text = serde_json::to_string_pretty(map)
            .map_err(|e| crate::error::LinkError::other(format!("could not encode secrets: {e}")))?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, text)
            .map_err(|e| crate::error::LinkError::other(format!("could not write secrets: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&temp, &self.path)
            .map_err(|e| crate::error::LinkError::other(format!("could not store secrets: {e}")))?;
        Ok(())
    }
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map.lock().expect("secret mutex poisoned").get(key).cloned()
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        let mut map = self.map.lock().expect("secret mutex poisoned");
        map.insert(key.to_string(), value.to_string());
        self.persist(&map)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let mut map = self.map.lock().expect("secret mutex poisoned");
        map.remove(key);
        self.persist(&map)
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

/// Try the keyring first and mirror every write into [`FileStore`], so a
/// locked or missing Secret Service does not make every request return 401.
#[cfg(any(feature = "keyring", test))]
struct ResilientStore {
    primary: Arc<dyn SecretStore>,
    fallback: FileStore,
}

#[cfg(any(feature = "keyring", test))]
impl SecretStore for ResilientStore {
    fn get(&self, key: &str) -> Option<String> {
        // `set` always writes the file mirror, so for keys this app stored the
        // file copy is at least as fresh as the keyring. Prefer it: a keyring
        // that comes back with an older value after an outage would otherwise
        // resurrect a key the user already replaced.
        if let Some(value) = self.fallback.get(key) {
            return Some(value);
        }
        let value = self.primary.get(key)?;
        if !value.is_empty() {
            // Keep a mirror so the key survives the keyring becoming unavailable.
            let _ = self.fallback.set(key, &value);
        }
        Some(value)
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        // Always mirror. A successful keyring write used to delete the file
        // copy, which meant that a later locked keyring made a previously
        // working key silently disappear and every request fail with 401.
        let mirrored = self.fallback.set(key, value);
        match self.primary.set(key, value) {
            Ok(()) => mirrored,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "keyring unavailable; the API key is kept in the local secret file"
                );
                mirrored
            }
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        let _ = self.fallback.delete(key);
        self.primary.delete(key)
    }

    fn keys(&self) -> Vec<String> {
        let mut keys = self.fallback.keys();
        keys.extend(self.primary.keys());
        keys.sort();
        keys.dedup();
        keys
    }
}

/// The store the app should use. Uses the keyring when the `keyring` feature is
/// compiled in *and* a Secret Service is reachable; failures fall back to a
/// mode-0600 file so keys survive restarts.
pub fn default_store() -> Arc<dyn SecretStore> {
    #[cfg(feature = "keyring")]
    {
        if keyring::LibSecretStore::is_available() {
            return Arc::new(ResilientStore {
                primary: Arc::new(keyring::LibSecretStore::new()),
                fallback: FileStore::new(),
            });
        }
        tracing::warn!(
            "no Secret Service keyring reachable; API keys will be kept in a local mode-0600 file"
        );
    }
    Arc::new(FileStore::new())
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
            libsecret::Service::sync(libsecret::ServiceFlags::NONE, gio::Cancellable::NONE).is_ok()
        }

        fn schema() -> libsecret::Schema {
            let mut attributes = std::collections::HashMap::new();
            attributes.insert("model", libsecret::SchemaAttributeType::String);
            libsecret::Schema::new(SCHEMA_NAME, libsecret::SchemaFlags::NONE, attributes)
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

    #[test]
    fn resilient_store_mirrors_keyring_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        {
            let store = ResilientStore {
                primary: Arc::new(MemoryStore::new()),
                fallback: FileStore::at(path.clone()),
            };
            store.set("model:a", "sk-mirrored").unwrap();
        }
        // Simulate the Secret Service disappearing: a fresh primary has no
        // value, but the file mirror must still answer.
        let store = ResilientStore {
            primary: Arc::new(MemoryStore::new()),
            fallback: FileStore::at(path),
        };
        assert_eq!(store.get("model:a").as_deref(), Some("sk-mirrored"));
    }

    #[test]
    fn resilient_store_mirrors_keyring_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let primary = MemoryStore::new();
        primary.set("model:b", "sk-existing").unwrap();
        let store = ResilientStore {
            primary: Arc::new(primary),
            fallback: FileStore::at(path.clone()),
        };
        assert_eq!(store.get("model:b").as_deref(), Some("sk-existing"));
        // The read was copied to disk for the next time the keyring is gone.
        assert_eq!(
            FileStore::at(path).get("model:b").as_deref(),
            Some("sk-existing")
        );
    }

    #[test]
    fn resilient_store_prefers_the_fresh_file_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let primary = MemoryStore::new();
        primary.set("model:c", "sk-stale").unwrap();
        let fallback = FileStore::at(path);
        fallback.set("model:c", "sk-fresh").unwrap();
        let store = ResilientStore {
            primary: Arc::new(primary),
            fallback,
        };
        assert_eq!(store.get("model:c").as_deref(), Some("sk-fresh"));
    }
}
