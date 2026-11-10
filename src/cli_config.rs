//! The CLI's own config (`$ARIA/Config/cli/config.json`): which model the CLI
//! talks to. Logs go to `$ARIA/Logs/cli/config.log`.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::crypto::{KeyStore, encrypt};
use crate::error::{AriaError, Result};
use crate::logger::Logger;
use crate::paths::{aria_path, http_get_json, log_file, prompt, read_json, write_json};

static LOG: LazyLock<Logger> =
    LazyLock::new(|| Logger::new("A.R.I.A.Cli.Config.cli_config", Some(log_file("cli/config.log"))));

pub const DEFAULT_CONFIG_URL: &str = "https://raw.githubusercontent.com/Shishir-Kc/Elysium_additionals/main/Configs/Elysium_cli/cli_config.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_type: Option<String>,
}

pub struct CliConfig {
    pub base_config_path: PathBuf,
    keys: KeyStore,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self::new(aria_path().join("Config/cli/config.json"), KeyStore::default())
    }
}

impl CliConfig {
    pub fn new(base_config_path: PathBuf, keys: KeyStore) -> Self {
        Self { base_config_path, keys }
    }

    pub fn check_config(&self) -> Result<()> {
        if !self.base_config_path.exists() {
            return Err(AriaError::ConfigNotFound(
                "CLI config not found, create it with `aria cli-config make`".into(),
            ));
        }
        Ok(())
    }

    /// Downloads the default config. An existing config is kept unless
    /// `over_ride` is set. Returns whether a config was written.
    pub fn make_config(&self, over_ride: bool) -> Result<bool> {
        if self.base_config_path.exists() && !over_ride {
            return Ok(false);
        }
        LOG.info("Creating default config");
        let config = http_get_json(DEFAULT_CONFIG_URL)
            .inspect_err(|e| LOG.error(format!("Request failed {e}")))?;
        write_json(&self.base_config_path, &config)?;
        Ok(true)
    }

    /// Asks for every field of the model config; the API key is encrypted
    /// before it is stored.
    pub fn input_config(&self) -> Result<()> {
        LOG.info("Adding values");
        let config = self.load()?;
        let fields: Vec<String> = config["model"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let mut model = serde_json::Map::new();
        for field in fields {
            let mut input = prompt(&format!("Enter {field} => "))?;
            if field == "api_key" {
                let key = self.keys.generate_key("cli_config", "", "")?;
                input = encrypt(&input, &key)?;
            }
            model.insert(field, Value::String(input));
        }
        let model = serde_json::from_value(Value::Object(model)).map_err(|e| {
            LOG.warning(format!("Invalid or missing args: {e}"));
            AriaError::InvalidArgs(e.to_string())
        })?;
        self.update_config(&model)
    }

    /// Replaces the `model` section of the config.
    pub fn update_config(&self, model: &ModelSchema) -> Result<()> {
        let mut config = self.load()?;
        let Some(map) = config.as_object_mut() else {
            return Err(AriaError::Other("CLI config is not a JSON object".into()));
        };
        map.insert("model".into(), serde_json::to_value(model)?);
        write_json(&self.base_config_path, &config)
    }

    pub fn load(&self) -> Result<Value> {
        self.check_config()?;
        read_json(&self.base_config_path).inspect_err(|e| {
            LOG.warning(format!(
                "failed to load the CLI config ({e}); reset it with `aria cli-config override`"
            ))
        })
    }

    /// The model section. A cloud model without an API key prompts for one.
    pub fn load_model_data(&self) -> Result<ModelSchema> {
        let config = self.load()?;
        let mut model: ModelSchema =
            serde_json::from_value(config.get("model").cloned().unwrap_or(Value::Null))
                .map_err(|e| AriaError::InvalidArgs(e.to_string()))?;
        if model.model_type.as_deref() == Some("Cloud") && model.api_key.as_deref() == Some("") {
            LOG.warning(format!("API key not provided for the model: {:?}", model.model_name));
            model.api_key = Some(prompt("API_KEY: ")?);
            self.update_config(&model)?;
        }
        Ok(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, CliConfig) {
        let dir = tempfile::tempdir().unwrap();
        let config =
            CliConfig::new(dir.path().join("cli/config.json"), KeyStore::new(dir.path().join("k.json")));
        (dir, config)
    }

    #[test]
    fn missing_config_is_an_error() {
        let (_dir, config) = fixture();
        assert!(matches!(config.check_config(), Err(AriaError::ConfigNotFound(_))));
        assert!(matches!(config.load(), Err(AriaError::ConfigNotFound(_))));
    }

    #[test]
    fn make_keeps_existing_config() {
        let (_dir, config) = fixture();
        write_json(&config.base_config_path, &json!({"model": {}})).unwrap();
        assert!(!config.make_config(false).unwrap());
    }

    #[test]
    fn update_replaces_model_and_drops_empty_fields() {
        let (_dir, config) = fixture();
        write_json(&config.base_config_path, &json!({"model": {"provider": "old"}, "other": 1})).unwrap();
        let model = ModelSchema {
            provider: Some("nvidia".into()),
            model_name: Some("llama".into()),
            api_key: None,
            model_type: Some("Local".into()),
        };
        config.update_config(&model).unwrap();
        let saved = config.load().unwrap();
        assert_eq!(saved["other"], 1);
        assert_eq!(
            saved["model"],
            json!({"provider": "nvidia", "model_name": "llama", "model_type": "Local"})
        );
        assert_eq!(config.load_model_data().unwrap(), model);
    }

    #[test]
    fn unknown_model_fields_are_rejected() {
        assert!(serde_json::from_value::<ModelSchema>(json!({"nope": "x"})).is_err());
    }
}
