//! Workers: jobs described in JSON so new work can be added dynamically (by a
//! user or an agent) without hard-coding it. See `workers/workers_preview.json`
//! for the shape. Only loading the config exists so far; scheduling and
//! running workers is still to be built.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AriaError, Result};
use crate::paths::{aria_path, read_json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerSpec {
    pub id: String,
    pub execution_time: String,
    pub repeat: bool,
}

pub struct Worker {
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl Default for Worker {
    fn default() -> Self {
        Self {
            config_dir: aria_path().join("Config/worker"),
            log_dir: aria_path().join("Logs/worker"),
        }
    }
}

impl Worker {
    /// Creates the worker config and log directories.
    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.config_dir)?;
        fs::create_dir_all(&self.log_dir)?;
        Ok(())
    }

    /// Loads worker definitions, keyed by worker name.
    pub fn load_config(&self, path: &Path) -> Result<BTreeMap<String, WorkerSpec>> {
        Ok(serde_json::from_value(read_json(path)?)?)
    }

    pub fn check_config(&self) -> Result<()> {
        Err(AriaError::NotImplemented("worker check_config"))
    }
    pub fn add_config(&self) -> Result<()> {
        Err(AriaError::NotImplemented("worker add_config"))
    }
    pub fn stats(&self) -> Result<()> {
        Err(AriaError::NotImplemented("worker stats"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_preview_config() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("workers/workers_preview.json");
        let workers = Worker::default().load_config(&path).unwrap();
        assert_eq!(
            workers["start_up"],
            WorkerSpec { id: "some_id".into(), execution_time: "on_startup".into(), repeat: true }
        );
    }
}
