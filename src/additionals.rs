//! Additionals: A.R.I.A's plug-and-play skills and tools.
//!
//! Additionals live in a separate repository
//! (<https://github.com/Shishir-Kc/Elysium_additionals>). Its `config.json`
//! lists every additional with a version, download URL, install path and
//! dependencies. This module downloads that list, installs additionals from
//! it, and checks installed ones for updates.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::error::{AriaError, Result};
use crate::logger::Logger;
use crate::paths::{
    bundled_app_config, download_config, home, home_path, http_get_json, read_json, write_json,
};
use crate::updater::parse_version;

static LOG: LazyLock<Logger> = LazyLock::new(|| Logger::new("Config.download_additionals", None));

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Downloaded(PathBuf),
    AlreadyDownloaded,
}

pub struct Additionals {
    /// Install paths in the additionals config are relative to this.
    pub home: PathBuf,
    /// `$ARIA/Additionals`
    pub root: PathBuf,
    /// The local copy of the additionals list.
    pub config_path: PathBuf,
    /// Names of the installed additionals.
    pub settings_path: PathBuf,
    /// Where the published additionals list is downloaded from.
    pub cloud_url: String,
}

impl Additionals {
    pub fn new() -> Result<Self> {
        let cloud_url = bundled_app_config()["aria_additionals_config"]["download_url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .ok_or_else(|| AriaError::ConfigFileMissing("aria_additionals_config.download_url".into()))?
            .to_owned();
        let root = home_path("aria_additionals_paths", "Root_path")
            .ok_or_else(|| AriaError::ConfigFileMissing("aria_additionals_paths.Root_path".into()))?;
        let config_path = home_path("additionals_config", "Config_path")
            .ok_or_else(|| AriaError::ConfigFileMissing("additionals_config.Config_path".into()))?;
        Ok(Self { home: home(), settings_path: root.join("settings.json"), root, config_path, cloud_url })
    }

    /// The published additionals list.
    pub fn cloud_config(&self) -> Result<Map<String, Value>> {
        LOG.info("Getting additionals cloud config");
        as_map(http_get_json(&self.cloud_url)?)
    }

    /// The local additionals list, downloaded first when it is missing.
    pub fn additionals(&self) -> Result<Map<String, Value>> {
        if !self.config_path.exists() {
            LOG.warning("Additionals config file missing, downloading it");
            write_json(&self.config_path, &self.cloud_config()?)?;
        }
        as_map(read_json(&self.config_path)?)
    }

    /// Installed additionals that have a newer version published.
    pub fn check_update(&self) -> Result<Map<String, Value>> {
        LOG.info("Checking for updates");
        let cloud = self.cloud_config()?;
        Ok(find_updates(&self.additionals()?, &cloud, &self.read_downloaded()))
    }

    pub fn read_downloaded(&self) -> Vec<String> {
        read_json(&self.settings_path)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    fn write_downloaded(&self, additional: &str) -> Result<()> {
        let mut installed = self.read_downloaded();
        if !installed.iter().any(|a| a == additional) {
            installed.push(additional.to_owned());
        }
        write_json(&self.settings_path, &installed)?;
        LOG.info(format!("Additional '{additional}' saved to settings"));
        Ok(())
    }

    /// Refreshes one additional's entry in the local list from the cloud.
    fn update_config(&self, additional: &str) -> Result<Map<String, Value>> {
        let cloud = self.cloud_config()?;
        let info = cloud
            .get(additional)
            .cloned()
            .ok_or_else(|| AriaError::AdditionalsNotFound(additional.to_owned()))?;
        let mut local = self.additionals()?;
        local.insert(additional.to_owned(), info);
        write_json(&self.config_path, &local)?;
        Ok(local)
    }

    /// Installs `additional` and its dependencies. With `update`, it must
    /// already be installed and is downloaded again.
    pub fn download(&self, additional: &str, update: bool) -> Result<DownloadStatus> {
        if additional.is_empty() {
            return Err(AriaError::InvalidArgs("no additional provided".into()));
        }
        let installed = self.read_downloaded().iter().any(|a| a == additional);
        if update && !installed {
            return Err(AriaError::AdditionalsNotInstalled(additional.to_owned()));
        }
        if !update && installed {
            return Ok(DownloadStatus::AlreadyDownloaded);
        }

        let local = self.update_config(additional)?;
        let info = &local[additional];
        let dir = self.home.join(info["path"].as_str().unwrap_or_default());
        let url = info["download_url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .ok_or_else(|| AriaError::AdditionalsNotFound(format!("{additional} has no download_url")))?;

        LOG.info(format!("Downloading {additional}"));
        let path = download_config(&dir, url)?;
        if let Some(dependencies) = info["dependency"].as_object() {
            for (name, dependency) in dependencies {
                LOG.info(format!("Downloading dependency: {name}"));
                let url = dependency["download_url"].as_str().ok_or_else(|| {
                    AriaError::AdditionalsNotFound(format!("dependency {name} has no download_url"))
                })?;
                download_config(&dir, url)?;
            }
        }
        self.write_downloaded(additional)?;
        Ok(DownloadStatus::Downloaded(path))
    }

    /// Updates every installed additional that has a newer version. Returns
    /// the names that were updated.
    pub fn update(&self) -> Result<Vec<String>> {
        let updates = self.check_update()?;
        for (i, additional) in updates.keys().enumerate() {
            LOG.info(format!("Updating: {} | {additional}", i + 1));
            self.download(additional, true)?;
        }
        Ok(updates.keys().cloned().collect())
    }
}

fn as_map(value: Value) -> Result<Map<String, Value>> {
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(AriaError::Other("additionals config is not a JSON object".into())),
    }
}

/// Is version `a` older than `b`? Dotted versions compare numerically, plain
/// numbers as numbers, anything else as text.
fn version_less(a: &Value, b: &Value) -> bool {
    if let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) {
        return a < b;
    }
    match (a.as_str(), b.as_str()) {
        (Some(a), Some(b)) => match (parse_version(a), parse_version(b)) {
            (Ok(a), Ok(b)) => a < b,
            _ => a < b,
        },
        _ => false,
    }
}

fn find_updates(
    local: &Map<String, Value>,
    cloud: &Map<String, Value>,
    installed: &[String],
) -> Map<String, Value> {
    let mut updates = Map::new();
    for (name, info) in local {
        let Some(cloud_info) = cloud.get(name) else { continue };
        if installed.contains(name) && version_less(&info["version"], &cloud_info["version"]) {
            LOG.info(format!("update available for {name}"));
            updates.insert(name.clone(), cloud_info.clone());
        }
    }
    if updates.is_empty() {
        LOG.info("Additionals are up to date");
    }
    updates
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        as_map(v).unwrap()
    }

    #[test]
    fn version_ordering() {
        assert!(version_less(&json!("1.9.0"), &json!("1.10.0")));
        assert!(!version_less(&json!("2.0"), &json!("2.0")));
        assert!(version_less(&json!(1), &json!(1.5)));
        assert!(version_less(&json!("alpha"), &json!("beta")));
        assert!(!version_less(&json!(null), &json!("1.0")));
    }

    #[test]
    fn only_installed_and_outdated_are_updates() {
        let local = map(json!({
            "Sentinel": {"version": "1.0"},
            "Other": {"version": "1.0"},
            "Current": {"version": "2.0"},
            "Gone": {"version": "1.0"}
        }));
        let cloud = map(json!({
            "Sentinel": {"version": "1.1", "path": "p"},
            "Other": {"version": "3.0"},
            "Current": {"version": "2.0"}
        }));
        let installed = ["Sentinel".to_string(), "Current".to_string(), "Gone".to_string()];
        let updates = find_updates(&local, &cloud, &installed);
        assert_eq!(updates.keys().collect::<Vec<_>>(), ["Sentinel"]);
        assert_eq!(updates["Sentinel"]["path"], "p");
    }

    #[test]
    fn installed_list_has_no_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let add = Additionals {
            home: dir.path().into(),
            root: dir.path().into(),
            config_path: dir.path().join("config.json"),
            settings_path: dir.path().join("settings.json"),
            cloud_url: String::new(),
        };
        assert!(add.read_downloaded().is_empty());
        add.write_downloaded("Sentinel").unwrap();
        add.write_downloaded("Sentinel").unwrap();
        assert_eq!(add.read_downloaded(), ["Sentinel"]);
        assert_eq!(add.download("Sentinel", false).unwrap(), DownloadStatus::AlreadyDownloaded);
        assert!(matches!(add.download("Nope", true), Err(AriaError::AdditionalsNotInstalled(_))));
        assert!(matches!(add.download("", false), Err(AriaError::InvalidArgs(_))));
    }
}
