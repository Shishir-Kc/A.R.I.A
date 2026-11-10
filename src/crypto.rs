//! API-key encryption (Fernet, compatible with Python's `cryptography.fernet`).
//!
//! * [`KeyStore::generate_key`] creates a key and records it in `keys.json`
//! * [`encrypt`] / [`decrypt`] encrypt and decrypt a string with a key
//! * [`KeyStore::get_key`] looks a key up by provider + model

use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;

use fernet::Fernet;
use serde::{Deserialize, Serialize};

use crate::error::{AriaError, Result};
use crate::logger::Logger;
use crate::paths::{aria_path, log_file};

static LOG: LazyLock<Logger> = LazyLock::new(|| {
    Logger::new("cryptography", Some(log_file("Security/encryption/cryptography.log")))
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntry {
    pub module: String,
    pub key: String,
    pub saved_at: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub model_name: String,
}

/// The `keys.json` file holding one Fernet key per provider/model pair.
pub struct KeyStore {
    path: PathBuf,
}

impl Default for KeyStore {
    fn default() -> Self {
        Self::new(aria_path().join("Config/Security/encryption/keys.json"))
    }
}

impl KeyStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<Vec<KeyEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&fs::read_to_string(&self.path)?)?)
    }

    fn save(&self, entries: &[KeyEntry]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_string_pretty(entries)?)?;
        // The file holds secret keys: owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Generates a key for `provider_name`/`model_name` and stores it. If that
    /// pair already has a key, the old one is replaced.
    pub fn generate_key(&self, module: &str, provider_name: &str, model_name: &str) -> Result<String> {
        LOG.info(format!("creating key for {module}"));
        let key = Fernet::generate_key();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string();
        let mut entries = self.load()?;
        match entries
            .iter_mut()
            .find(|e| e.model_name == model_name && e.provider_name == provider_name)
        {
            Some(entry) => {
                LOG.warning("module already exists updating old key");
                entry.key = key.clone();
                entry.saved_at = now;
            }
            None => entries.push(KeyEntry {
                module: module.to_owned(),
                key: key.clone(),
                saved_at: now,
                provider_name: provider_name.to_owned(),
                model_name: model_name.to_owned(),
            }),
        }
        self.save(&entries).inspect_err(|e| LOG.error(format!("failed to save key {e}")))?;
        LOG.info("Key saved successfully");
        Ok(key)
    }

    pub fn get_key(&self, provider_name: &str, model_name: &str) -> Result<String> {
        if !self.path.exists() {
            return Err(AriaError::KeysNotFound);
        }
        self.load()?
            .into_iter()
            .find(|e| e.model_name == model_name && e.provider_name == provider_name)
            .map(|e| e.key)
            .filter(|k| !k.is_empty())
            .ok_or(AriaError::KeysNotFound)
    }
}

fn fernet(key: &str) -> Result<Fernet> {
    Fernet::new(key).ok_or_else(|| AriaError::Crypto("invalid Fernet key".into()))
}

pub fn encrypt(item: &str, key: &str) -> Result<String> {
    LOG.info("encrypting key");
    Ok(fernet(key)?.encrypt(item.as_bytes()))
}

pub fn decrypt(token: &str, key: &str) -> Result<String> {
    LOG.info("decrypting key");
    let bytes = fernet(key)?
        .decrypt(token)
        .map_err(|_| AriaError::Crypto("could not decrypt: wrong key or corrupted token".into()))?;
    String::from_utf8(bytes).map_err(|e| AriaError::Crypto(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = Fernet::generate_key();
        let token = encrypt("sk-secret", &key).unwrap();
        assert_ne!(token, "sk-secret");
        assert_eq!(decrypt(&token, &key).unwrap(), "sk-secret");
        assert!(decrypt(&token, &Fernet::generate_key()).is_err());
    }

    /// Test vector from the Fernet spec: tokens written by the Python version
    /// stay readable.
    #[test]
    fn reads_python_fernet_tokens() {
        let key = "cw_0x689RpI-jtRR7oE8h_eQsKImvJapLeSbXpwF4e4=";
        let token = "gAAAAAAdwJ6wAAECAwQFBgcICQoLDA0ODy021cpGVWKZ_eEwCGM4BLLF_5CV9dOPmrhuVUPgJobwOz7JcbmrR64jVmpU4IwqDA==";
        assert_eq!(decrypt(token, key).unwrap(), "hello");
    }

    #[test]
    fn key_store_adds_then_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::new(dir.path().join("enc/keys.json"));
        assert!(matches!(store.get_key("nvidia", "m"), Err(AriaError::KeysNotFound)));

        let first = store.generate_key("test", "nvidia", "m").unwrap();
        store.generate_key("test", "nvidia", "other").unwrap();
        assert_eq!(store.get_key("nvidia", "m").unwrap(), first);

        let second = store.generate_key("test", "nvidia", "m").unwrap();
        assert_ne!(first, second);
        assert_eq!(store.get_key("nvidia", "m").unwrap(), second);
        assert_eq!(store.load().unwrap().len(), 2);
    }
}
