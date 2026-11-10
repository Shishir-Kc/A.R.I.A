//! Model config: which providers/models exist and their (encrypted) API keys.
//!
//! The config lives at `$ARIA/Config/Model/model_config.json` and looks like
//! `{ "<provider>": { "<model>": { "model_name": ..., "api_key": ... } } }`.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::crypto::{KeyStore, decrypt, encrypt};
use crate::error::{AriaError, Result};
use crate::logger::Logger;
use crate::paths::{aria_path, http_get_json, log_file, read_json, write_json};

static LOG: LazyLock<Logger> =
    LazyLock::new(|| Logger::new("Config.model_config", Some(log_file("Model/model_config.log"))));

pub const DEFAULT_CONFIG_URL: &str = "https://raw.githubusercontent.com/Shishir-Kc/Elysium_additionals/main/Configs/Elysium_config/model_config.json";
const CONFIG_NAME: &str = "model_config.json";

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedModel {
    pub api_key: String,
    pub provider: String,
    pub model: String,
}

pub struct ModelConfig {
    base_dir: PathBuf,
    keys: KeyStore,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self::new(aria_path().join("Config/Model"), KeyStore::default())
    }
}

impl ModelConfig {
    pub fn new(base_dir: PathBuf, keys: KeyStore) -> Self {
        Self { base_dir, keys }
    }

    pub fn config_path(&self) -> PathBuf {
        self.base_dir.join(CONFIG_NAME)
    }

    pub fn check_model_config_path(&self) -> Result<()> {
        if !self.config_path().exists() {
            LOG.warning(format!("{CONFIG_NAME} does not exist!"));
            return Err(AriaError::ConfigFileMissing(format!(
                "{} (try `aria model download-config`)",
                self.config_path().display()
            )));
        }
        LOG.info(format!("{CONFIG_NAME} exists :)"));
        Ok(())
    }

    /// The whole config; empty when it is missing or unreadable.
    pub fn load_config(&self) -> Map<String, Value> {
        LOG.info("trying to load model_config");
        match read_json(&self.config_path()) {
            Ok(Value::Object(map)) => {
                LOG.info(format!("{CONFIG_NAME} loaded!"));
                map
            }
            Ok(_) => {
                LOG.error(format!("{CONFIG_NAME} is not a JSON object"));
                Map::new()
            }
            Err(e) => {
                LOG.error(format!("could not load {CONFIG_NAME} from {}: {e}", self.base_dir.display()));
                Map::new()
            }
        }
    }

    pub fn available_providers(&self) -> Vec<String> {
        LOG.info("getting available providers");
        self.load_config().keys().cloned().collect()
    }

    /// Encrypts `api_key` with a fresh key and stores it on the model's entry.
    pub fn insert_api_key(&self, provider_name: &str, model_name: &str, api_key: &str) -> Result<()> {
        LOG.info("Applying custom API key");
        if provider_name.is_empty() {
            return Err(AriaError::ProviderNotGiven("expected provider name".into()));
        }
        if api_key.is_empty() {
            return Err(AriaError::ApiKeyNotGiven("expected API key".into()));
        }
        if model_name.is_empty() {
            return Err(AriaError::ModelNameNotGiven("expected model name".into()));
        }
        self.check_model_config_path()?;

        let mut config = self.load_config();
        let provider = config
            .get_mut(provider_name)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| AriaError::ProviderNotFound(format!("'{provider_name}' is not in the config")))?;
        let model = provider.get_mut(model_name).and_then(Value::as_object_mut).ok_or_else(|| {
            AriaError::ModelNotFound(format!("'{model_name}' is not in the '{provider_name}' config"))
        })?;

        let key = self.keys.generate_key("config::model_config", provider_name, model_name)?;
        model.insert("api_key".into(), Value::String(encrypt(api_key, &key)?));
        write_json(&self.config_path(), &config)?;
        LOG.info("applied custom api key!");
        Ok(())
    }

    /// Downloads the model config from `url` (or the default one).
    pub fn download_config(&self, url: Option<&str>) -> Result<()> {
        let url = match url.filter(|u| !u.is_empty()) {
            Some(u) => {
                LOG.info(format!("Using custom url '{u}'"));
                u
            }
            None => {
                LOG.info("Using default config url");
                DEFAULT_CONFIG_URL
            }
        };
        let config = http_get_json(url).inspect_err(|e| LOG.error(format!("Request failed {e}")))?;
        write_json(&self.config_path(), &config)?;
        LOG.info("Config downloaded");
        self.check_model_config_path()
    }

    /// The model's decrypted API key.
    pub fn load_model(&self, required_provider: &str, required_model: &str) -> Result<LoadedModel> {
        let config = self.load_config();
        let provider = config
            .get(required_provider)
            .and_then(Value::as_object)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| AriaError::ProviderNotFound(required_provider.into()))?;
        let model = provider
            .get(required_model)
            .ok_or_else(|| AriaError::ModelNotFound(required_model.into()))?;
        let token = model.get("api_key").and_then(Value::as_str).unwrap_or_default();
        if token.is_empty() {
            return Err(AriaError::ApiKeyNotGiven(format!(
                "no API key stored for {required_provider}/{required_model} (try `aria model insert-api`)"
            )));
        }
        let key = self.keys.get_key(required_provider, required_model)?;
        Ok(LoadedModel {
            api_key: decrypt(token, &key)?,
            provider: required_provider.to_owned(),
            model: required_model.to_owned(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn fixture() -> (tempfile::TempDir, ModelConfig) {
        let dir = tempfile::tempdir().unwrap();
        let mc = ModelConfig::new(dir.path().join("Model"), KeyStore::new(dir.path().join("keys.json")));
        write_json(
            &mc.config_path(),
            &json!({
                "nvidia": {
                    "priority": 1,
                    "llama": {"model_name": "llama", "model_provider": "meta", "api_key": ""}
                },
                "groq": {"mixtral": {"model_name": "mixtral", "api_key": ""}}
            }),
        )
        .unwrap();
        (dir, mc)
    }

    #[test]
    fn providers_keep_file_order() {
        let (_dir, mc) = fixture();
        assert_eq!(mc.available_providers(), ["nvidia", "groq"]);
    }

    #[test]
    fn missing_config_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mc = ModelConfig::new(dir.path().into(), KeyStore::new(dir.path().join("k.json")));
        assert!(matches!(mc.check_model_config_path(), Err(AriaError::ConfigFileMissing(_))));
        assert!(mc.load_config().is_empty());
    }

    #[test]
    fn api_key_is_stored_encrypted_and_loads_back() {
        let (_dir, mc) = fixture();
        mc.insert_api_key("nvidia", "llama", "sk-123").unwrap();

        let stored = mc.load_config()["nvidia"]["llama"]["api_key"].as_str().unwrap().to_owned();
        assert!(!stored.is_empty() && !stored.contains("sk-123"));

        let loaded = mc.load_model("nvidia", "llama").unwrap();
        assert_eq!(loaded.api_key, "sk-123");
    }

    #[test]
    fn insert_api_key_validates() {
        let (_dir, mc) = fixture();
        assert!(matches!(mc.insert_api_key("", "llama", "k"), Err(AriaError::ProviderNotGiven(_))));
        assert!(matches!(mc.insert_api_key("nvidia", "llama", ""), Err(AriaError::ApiKeyNotGiven(_))));
        assert!(matches!(mc.insert_api_key("nvidia", "", "k"), Err(AriaError::ModelNameNotGiven(_))));
        assert!(matches!(mc.insert_api_key("nope", "llama", "k"), Err(AriaError::ProviderNotFound(_))));
        assert!(matches!(mc.insert_api_key("nvidia", "nope", "k"), Err(AriaError::ModelNotFound(_))));
        assert!(matches!(mc.load_model("nvidia", "llama"), Err(AriaError::ApiKeyNotGiven(_))));
    }
}
