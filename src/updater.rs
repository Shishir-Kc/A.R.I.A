//! Checking for and installing new A.R.I.A versions.
//!
//! The installed copy lives in `~/.A.R.I.A`; its `config/config.json` names the
//! URL of the published config, which carries the latest version.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use serde::Serialize;
use serde_json::Value;

use crate::error::{AriaError, Result};
use crate::logger::Logger;
use crate::paths::{bundled_app_config, home, http_get_json, read_json};

static LOG: LazyLock<Logger> = LazyLock::new(|| Logger::new("Config.updater", None));

/// "1.12.3" -> [1, 12, 3], so versions compare numerically.
pub fn parse_version(v: &str) -> Result<Vec<u64>> {
    v.trim()
        .split('.')
        .map(|part| part.parse::<u64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| AriaError::Other(format!("invalid version '{v}'")))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    /// Set when the version name changed, i.e. a major update.
    pub version_name: Option<String>,
    pub stable: bool,
    pub url: String,
    pub repo: String,
    pub latest_changes: String,
}

fn text(meta: &Value, key: &str) -> String {
    meta[key].as_str().unwrap_or_default().to_owned()
}

/// The config stores `stable` as either a bool or the string "True"/"False".
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Compares two full configs; `Some` when `cloud` is newer than `local`.
pub fn compare_configs(local: &Value, cloud: &Value) -> Result<Option<UpdateInfo>> {
    let (local, cloud) = (&local["aria"], &cloud["aria"]);
    let version = |meta: &Value, which: &str| {
        meta["version"]
            .as_str()
            .ok_or_else(|| AriaError::Other(format!("{which} config has no aria.version")))
            .and_then(parse_version)
    };
    if version(local, "local")? >= version(cloud, "cloud")? {
        LOG.info("No update available");
        return Ok(None);
    }
    LOG.info("Update is available");
    let major = local["version_name"] != cloud["version_name"];
    if major {
        LOG.info("Major update is available!");
    }
    Ok(Some(UpdateInfo {
        version: text(cloud, "version"),
        version_name: major.then(|| text(cloud, "version_name")),
        stable: truthy(&cloud["stable"]),
        url: text(cloud, "url"),
        repo: text(cloud, "repo"),
        latest_changes: text(cloud, "last_development_changes"),
    }))
}

pub struct Updater {
    pub root: PathBuf,
    pub local_config: PathBuf,
}

impl Default for Updater {
    fn default() -> Self {
        Self::new(home().join(".A.R.I.A"))
    }
}

impl Updater {
    pub fn new(root: PathBuf) -> Self {
        Self { local_config: root.join("config/config.json"), root }
    }

    /// The installed config, or the one built into this binary when A.R.I.A
    /// is not installed under `root`.
    pub fn read_local_config(&self) -> Result<Value> {
        if self.local_config.exists() {
            read_json(&self.local_config)
        } else {
            Ok(bundled_app_config().clone())
        }
    }

    fn cloud_config(&self, local: &Value) -> Result<Value> {
        LOG.info("Getting cloud config");
        let url = local["aria"]["url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .ok_or_else(|| AriaError::Other("local config has no aria.url".into()))?;
        http_get_json(url)
    }

    pub fn check_update(&self) -> Result<Option<UpdateInfo>> {
        let local = self.read_local_config()?;
        let cloud = self.cloud_config(&local)?;
        compare_configs(&local, &cloud)
    }

    /// Downloads and installs the newest version if there is one.
    ///
    /// The new version is cloned next to the install first and only swapped in
    /// once the clone succeeded, so a failed download leaves the old version
    /// untouched.
    pub fn update_aria(&self) -> Result<Option<UpdateInfo>> {
        LOG.info("Looking for updates");
        let Some(update) = self.check_update()? else {
            LOG.info("No updates found");
            return Ok(None);
        };
        if !self.root.exists() {
            return Err(AriaError::Other(format!(
                "A.R.I.A is not installed at {}",
                self.root.display()
            )));
        }

        let staging = sibling(&self.root, "update");
        let backup = sibling(&self.root, "old");
        remove_if_exists(&staging)?;
        remove_if_exists(&backup)?;

        LOG.info("Downloading update");
        run(Command::new("git").args(["clone", "--depth", "1", &update.repo]).arg(&staging))?;

        LOG.warning("Replacing old version");
        fs::rename(&self.root, &backup)?;
        if let Err(e) = fs::rename(&staging, &self.root) {
            fs::rename(&backup, &self.root)?;
            return Err(e.into());
        }
        remove_if_exists(&backup)?;
        LOG.info("Update downloaded");

        LOG.info("Syncing up");
        if self.root.join("Cargo.toml").exists() {
            run(Command::new("cargo").args(["build", "--release"]).current_dir(&self.root))?;
        } else if self.root.join("pyproject.toml").exists() {
            run(Command::new("uv").arg("sync").current_dir(&self.root))?;
        }
        LOG.info("Sync process completed");
        Ok(Some(update))
    }
}

fn sibling(root: &Path, suffix: &str) -> PathBuf {
    let mut name = root.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{suffix}"));
    root.with_file_name(name)
}

fn remove_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| AriaError::Other(format!("could not run {program}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(AriaError::Other(format!("{program} failed ({status})")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(version: &str, name: &str, stable: Value) -> Value {
        json!({"aria": {
            "version": version, "version_name": name, "stable": stable,
            "url": "https://example.test/config.json", "repo": "https://example.test/repo",
            "last_development_changes": "today"
        }})
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("0.10.0").unwrap() > parse_version("0.9.9").unwrap());
        assert_eq!(parse_version("1.2.3").unwrap(), [1, 2, 3]);
        assert!(parse_version("1.x").is_err());
    }

    #[test]
    fn no_update_when_same_or_older() {
        let local = config("0.2.0", "a", json!(false));
        assert_eq!(compare_configs(&local, &local).unwrap(), None);
        assert_eq!(compare_configs(&local, &config("0.1.9", "a", json!(false))).unwrap(), None);
    }

    #[test]
    fn minor_and_major_updates() {
        let local = config("0.1.0", "omega-cooper", json!("False"));
        let minor = compare_configs(&local, &config("0.1.1", "omega-cooper", json!("False")))
            .unwrap()
            .unwrap();
        assert_eq!(minor.version, "0.1.1");
        assert_eq!(minor.version_name, None);
        assert!(!minor.stable);
        assert_eq!(minor.latest_changes, "today");

        let major = compare_configs(&local, &config("1.0.0", "next", json!("True"))).unwrap().unwrap();
        assert_eq!(major.version_name.as_deref(), Some("next"));
        assert!(major.stable);
    }

    #[test]
    fn falls_back_to_bundled_config() {
        let dir = tempfile::tempdir().unwrap();
        let updater = Updater::new(dir.path().join("missing"));
        assert!(updater.read_local_config().unwrap()["aria"]["version"].is_string());
        assert!(sibling(&updater.root, "old").ends_with("missing.old"));
    }
}
