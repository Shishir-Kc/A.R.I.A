//! Path config: where A.R.I.A keeps its files, plus JSON / download helpers.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use serde_json::Value;

use crate::error::{AriaError, Result};
use crate::logger::Logger;

/// `config/path_config.json`, compiled into the binary.
const PATH_CONFIG: &str = include_str!("../config/path_config.json");
/// `config/config.json` (version metadata + download URLs), compiled into the binary.
const APP_CONFIG: &str = include_str!("../config/config.json");

static LOG: LazyLock<Logger> = LazyLock::new(|| Logger::new("Config.path_config", None));

static PATHS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(PATH_CONFIG).expect("path_config.json is valid JSON"));
static APP: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(APP_CONFIG).expect("config.json is valid JSON"));

static HTTP: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into()
});

pub fn home() -> PathBuf {
    std::env::home_dir().expect("could not determine the home directory")
}

/// The whole path config.
pub fn show_aria_paths() -> &'static Value {
    &PATHS
}

/// The names of the sections in the path config.
pub fn aria_path_sections() -> Vec<String> {
    PATHS.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

/// The config that shipped with this build.
pub fn bundled_app_config() -> &'static Value {
    &APP
}

/// A path from the `aria_paths` section, relative to the home directory.
pub fn get_aria_path(of: &str) -> Option<String> {
    PATHS["aria_paths"][of].as_str().map(str::to_owned)
}

/// A path from any section, resolved against the home directory.
pub fn home_path(section: &str, key: &str) -> Option<PathBuf> {
    PATHS[section][key].as_str().map(|p| home().join(p))
}

/// `$HOME/.config/A.R.I.A`
pub fn aria_path() -> PathBuf {
    home().join(get_aria_path("Root_path").unwrap_or_else(|| ".config/A.R.I.A".into()))
}

/// A log file under `$ARIA/Logs`.
pub fn log_file(relative: &str) -> PathBuf {
    // Tests must not write into the real A.R.I.A directory.
    if cfg!(test) {
        return std::env::temp_dir().join("aria-test-logs").join(relative);
    }
    aria_path().join("Logs").join(relative)
}

/// Does `path` exist? With no path, checks the A.R.I.A root.
pub fn check_for_aria_path(path: Option<&Path>) -> bool {
    match path {
        Some(p) => p.exists(),
        None => aria_path().exists(),
    }
}

pub fn http() -> &'static ureq::Agent {
    &HTTP
}

pub fn http_get_text(url: &str) -> Result<String> {
    Ok(HTTP.get(url).call()?.body_mut().read_to_string()?)
}

pub fn http_get_json(url: &str) -> Result<Value> {
    Ok(serde_json::from_str(&http_get_text(url)?)?)
}

/// The file name a URL points at.
pub fn url_file_name(url: &str) -> &str {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    no_query.trim_end_matches('/').rsplit('/').next().unwrap_or(no_query)
}

/// Downloads `url` into `dir`, keeping the remote file name. JSON is validated
/// and pretty-printed. Returns the path written.
pub fn download_config(dir: &Path, url: &str) -> Result<PathBuf> {
    if dir.as_os_str().is_empty() || url.is_empty() {
        return Err(AriaError::DirectoryNotGiven);
    }
    let file_name = url_file_name(url);
    let path = dir.join(file_name);
    LOG.info(format!("downloading {url} -> {}", path.display()));
    let text = http_get_text(url)?;
    fs::create_dir_all(dir)?;
    if file_name.ends_with(".json") {
        write_json(&path, &serde_json::from_str::<Value>(&text)?)?;
    } else {
        fs::write(&path, text)?;
    }
    Ok(path)
}

pub fn read_json(path: &Path) -> Result<Value> {
    LOG.info(format!("Reading JSON {}", path.display()));
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

/// Writes pretty (2-space) JSON, creating parent directories.
pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

/// Prints `label`, then reads one trimmed line from stdin.
pub fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Err(AriaError::Other("input closed".into()));
    }
    Ok(line.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_path_comes_from_config() {
        assert_eq!(get_aria_path("Root_path").as_deref(), Some(".config/A.R.I.A"));
        assert!(aria_path().ends_with(".config/A.R.I.A"));
        assert!(aria_path_sections().contains(&"aria_paths".to_string()));
    }

    #[test]
    fn file_name_from_url() {
        assert_eq!(url_file_name("https://x.dev/a/b/config.json"), "config.json");
        assert_eq!(url_file_name("https://x.dev/a/tool.py?raw=1"), "tool.py");
    }

    #[test]
    fn json_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b.json");
        write_json(&path, &serde_json::json!({"z": 1, "a": 2})).unwrap();
        let back = read_json(&path).unwrap();
        // key order is preserved
        assert_eq!(back.as_object().unwrap().keys().collect::<Vec<_>>(), ["z", "a"]);
    }
}
