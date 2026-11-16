//! Workers: jobs described in JSON, so new work can be added (by a user or an
//! agent) by dropping a file into `$ARIA/Config/worker/`, without code.
//!
//! * [`spec`]   — the worker file format and the scheduling maths
//! * [`daemon`] — the supervisor that runs them (`aria worker daemon`)
//! * [`ipc`]    — its control socket protocol and client
//! * [`tui`]    — the worker manager (`aria worker`)
//!
//! See `docs/workers.md` for the design and the research behind it.

pub mod daemon;
pub mod ipc;
pub mod spec;
pub mod tui;

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::paths::aria_path;

/// An example worker file (also used by the tests).
pub const EXAMPLE: &str = include_str!("../../workers/workers_preview.json");

/// Every path the worker system uses.
#[derive(Debug, Clone)]
pub struct WorkerPaths {
    /// Worker definitions, one or more `*.json` files.
    pub config_dir: PathBuf,
    /// One `<worker>.log` per worker.
    pub log_dir: PathBuf,
    /// Run history, kept across daemon restarts.
    pub state_file: PathBuf,
    /// Private directory for the socket and lock (`$XDG_RUNTIME_DIR/aria`).
    pub runtime_dir: PathBuf,
    pub socket: PathBuf,
    pub lock_file: PathBuf,
}

impl Default for WorkerPaths {
    fn default() -> Self {
        let root = aria_path();
        let runtime_dir = runtime_dir();
        Self {
            config_dir: root.join("Config/worker"),
            log_dir: root.join("Logs/worker"),
            state_file: root.join("State/workers.json"),
            socket: runtime_dir.join("workers.sock"),
            lock_file: runtime_dir.join("workers.lock"),
            runtime_dir,
        }
    }
}

impl WorkerPaths {
    /// Everything under one directory (for tests).
    pub fn under(root: &Path) -> Self {
        let runtime_dir = root.join("run");
        Self {
            config_dir: root.join("config"),
            log_dir: root.join("logs"),
            state_file: root.join("state.json"),
            socket: runtime_dir.join("workers.sock"),
            lock_file: runtime_dir.join("workers.lock"),
            runtime_dir,
        }
    }

    /// The log file of a worker; unusual characters in its name become `_`.
    pub fn worker_log(&self, name: &str) -> PathBuf {
        let safe: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' })
            .collect();
        self.log_dir.join(format!("{}.log", safe.trim_start_matches('.')))
    }

    /// Writes the example worker file when the worker directory has no
    /// `.json` files yet. Returns the file written.
    pub fn init_example(&self) -> Result<Option<PathBuf>> {
        fs::create_dir_all(&self.config_dir)?;
        let has_workers = fs::read_dir(&self.config_dir)?
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|x| x == "json"));
        if has_workers {
            return Ok(None);
        }
        let path = self.config_dir.join("example.json");
        fs::write(&path, EXAMPLE)?;
        Ok(Some(path))
    }
}

/// `$XDG_RUNTIME_DIR/aria`, or `/tmp/aria-<uid>` without one.
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("aria"),
        // SAFETY: getuid cannot fail.
        None => std::env::temp_dir().join(format!("aria-{}", unsafe { libc::getuid() })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::spec::load_dir;

    #[test]
    fn the_example_file_is_valid() {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::under(dir.path());
        let written = paths.init_example().unwrap().unwrap();
        assert!(written.exists());
        // Not written twice.
        assert!(paths.init_example().unwrap().is_none());
        let entries = load_dir(&paths.config_dir);
        assert!(entries.len() >= 4);
        for entry in entries {
            assert!(entry.worker.is_ok(), "{}: {:?}", entry.name, entry.worker.err());
        }
    }

    #[test]
    fn log_names_are_safe() {
        let paths = WorkerPaths::under(Path::new("/x"));
        assert_eq!(paths.worker_log("disk-report"), Path::new("/x/logs/disk-report.log"));
        assert_eq!(paths.worker_log("../etc/passwd"), Path::new("/x/logs/_etc_passwd.log"));
    }
}
