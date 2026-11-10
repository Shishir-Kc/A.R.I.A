//! Config for the local voice model.
//!
//! The voice config lists the available models with their RAM requirement;
//! the biggest model the machine can run is selected.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde_json::Value;

use crate::error::{AriaError, Result};
use crate::linux::Linux;
use crate::logger::Logger;
use crate::paths::{aria_path, home, http_get_json, read_json, url_file_name, write_json};

static LOG: LazyLock<Logger> = LazyLock::new(|| Logger::new("Config.voice_config", None));

pub const CONFIG_DOWNLOAD_URL: &str = "https://raw.githubusercontent.com/Shishir-Kc/Elysium_additionals/refs/heads/main/Configs/Voice/voice_config.json";

pub struct VoiceConfig {
    pub config_download_url: String,
    pub voice_config_path: PathBuf,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            config_download_url: CONFIG_DOWNLOAD_URL.to_owned(),
            voice_config_path: aria_path().join("Config/Model").join(url_file_name(CONFIG_DOWNLOAD_URL)),
        }
    }
}

impl VoiceConfig {
    fn load(&self) -> Result<Value> {
        read_json(&self.voice_config_path).map_err(|e| {
            LOG.error(e.to_string());
            AriaError::ConfigFileMissing(format!(
                "{} (try `aria voice download-config`)",
                self.voice_config_path.display()
            ))
        })
    }

    /// Where the voice model is (to be) installed.
    pub fn model_path(&self) -> Result<Option<PathBuf>> {
        LOG.info("Reading voice model path");
        let config = self.load()?;
        Ok(config["path"].as_str().filter(|p| !p.is_empty()).map(|p| home().join(p)))
    }

    /// Downloads the voice config into `$ARIA/Config/Model`.
    pub fn download(&self) -> Result<()> {
        LOG.info(format!("Sending get request to {}", self.config_download_url));
        write_json(&self.voice_config_path, &http_get_json(&self.config_download_url)?)?;
        LOG.info("Downloaded voice config");
        Ok(())
    }

    /// The model to use on this machine, chosen by its total RAM.
    pub fn select_model(&self) -> Result<Option<String>> {
        let total_ram = Linux::default().show_ram_info()?.total as u64;
        LOG.info(format!("Got {total_ram}GB"));
        Ok(select_model_for(&self.load()?, total_ram))
    }

    /// Downloading the model itself is not available: KittenTTS, which did
    /// the download, was removed from the project and has no Rust port.
    pub fn download_model(&self) -> Result<()> {
        Err(AriaError::NotImplemented("voice model download"))
    }
}

/// The last model in the config whose `required_ram` (e.g. "4GB") fits.
pub fn select_model_for(config: &Value, total_ram_gb: u64) -> Option<String> {
    let models = config["models"].as_object()?;
    models
        .iter()
        .filter(|(_, requirement)| {
            requirement["required_ram"]
                .as_str()
                .and_then(|r| r.trim().trim_end_matches("GB").trim().parse::<u64>().ok())
                .is_some_and(|required| required <= total_ram_gb)
        })
        .map(|(name, _)| name.clone())
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_the_last_model_that_fits() {
        let config = json!({"models": {
            "nano": {"required_ram": "1GB"},
            "micro": {"required_ram": "4GB"},
            "mini": {"required_ram": "16GB"}
        }});
        assert_eq!(select_model_for(&config, 8).as_deref(), Some("micro"));
        assert_eq!(select_model_for(&config, 64).as_deref(), Some("mini"));
        assert_eq!(select_model_for(&config, 0), None);
        assert_eq!(select_model_for(&json!({}), 8), None);
    }
}
