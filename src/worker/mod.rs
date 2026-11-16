//! Workers: jobs described in JSON, so new work can be added (by a user or an
//! agent) by dropping a file into `$ARIA/Config/worker/`, without code.
//!
//! * [`spec`]   — the worker file format and the scheduling maths
//! * [`awl`]    — AWL, the worker language (`*.awl` files)
//! * [`daemon`] — the supervisor that runs them (`aria worker daemon`)
//! * [`ipc`]    — its control socket protocol and client
//! * [`tui`]    — the worker manager (`aria worker`)
//!
//! See `docs/workers.md` for the design and the research behind it.

pub mod awl;
pub mod daemon;
pub mod ipc;
pub mod spec;
pub mod tui;

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::paths::aria_path;
use spec::{WorkerSpec, is_worker_file};

/// An example worker file (also used by the tests).
pub const EXAMPLE: &str = include_str!("../../workers/workers_preview.json");
/// The same idea in AWL, with a condition and hooks.
pub const EXAMPLE_AWL: &str = include_str!("../../workers/example.awl");

/// Every path the worker system uses.
#[derive(Debug, Clone)]
pub struct WorkerPaths {
    /// Worker definitions: `*.json` and `*.awl` files.
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

    /// Writes the example worker files (JSON and AWL) when the worker
    /// directory has no worker files yet. Returns the files written.
    pub fn init_example(&self) -> Result<Vec<PathBuf>> {
        fs::create_dir_all(&self.config_dir)?;
        let has_workers =
            fs::read_dir(&self.config_dir)?.filter_map(|e| e.ok()).any(|e| is_worker_file(&e.path()));
        if has_workers {
            return Ok(Vec::new());
        }
        let mut written = Vec::new();
        for (name, text) in [("example.json", EXAMPLE), ("example.awl", EXAMPLE_AWL)] {
            let path = self.config_dir.join(name);
            fs::write(&path, text)?;
            written.push(path);
        }
        Ok(written)
    }
}

/// Checks worker files without running anything (`aria worker check`).
/// AWL files get the full compiler report. Returns the report and whether
/// every file was valid.
pub fn check_files(files: &[PathBuf]) -> (String, bool) {
    let mut report = String::new();
    let mut ok = true;
    for file in files {
        let shown = file.display().to_string();
        let src = match fs::read_to_string(file) {
            Ok(src) => src,
            Err(e) => {
                report.push_str(&format!("error: {shown}: {e}\n"));
                ok = false;
                continue;
            }
        };
        let result: std::result::Result<Vec<String>, String> = if file.extension().is_some_and(|x| x == "awl") {
            awl::check_source(&src, &shown).and_then(|names| {
                // The language checks types; the scheduler checks the rest.
                let compiled = awl::compile(&src, &shown).map_err(|_| String::new())?;
                for c in compiled {
                    spec::Worker::compile(&c.name, c.spec).map_err(|e| format!("error: {shown}: worker `{}`: {e}\n", c.name))?;
                }
                Ok(names)
            })
        } else {
            match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&src) {
                Err(e) => Err(format!("error: {shown}: {e}\n")),
                Ok(map) => {
                    let mut problems = String::new();
                    for (name, raw) in &map {
                        let checked = serde_json::from_value::<WorkerSpec>(raw.clone())
                            .map_err(|e| e.to_string())
                            .and_then(|spec| spec::Worker::compile(name, spec));
                        if let Err(e) = checked {
                            problems.push_str(&format!("error: {shown}: worker `{name}`: {e}\n"));
                        }
                    }
                    if problems.is_empty() { Ok(map.keys().cloned().collect()) } else { Err(problems) }
                }
            }
        };
        match result {
            Ok(names) => report.push_str(&format!("ok: {shown} ({} worker{}: {})\n", names.len(), if names.len() == 1 { "" } else { "s" }, names.join(", "))),
            Err(e) => {
                ok = false;
                report.push_str(&e);
                if !e.ends_with("\n\n") {
                    report.push('\n');
                }
            }
        }
    }
    (report, ok)
}

impl WorkerPaths {
    /// Every worker file in the worker directory, in name order.
    pub fn worker_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.config_dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| is_worker_file(p)).collect())
            .unwrap_or_default();
        files.sort();
        files
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
        let written = paths.init_example().unwrap();
        assert_eq!(written.len(), 2);
        assert!(written.iter().all(|p| p.exists()));
        // Not written twice.
        assert!(paths.init_example().unwrap().is_empty());
        let entries = load_dir(&paths.config_dir);
        assert!(entries.len() >= 6);
        for entry in &entries {
            assert!(entry.worker.is_ok(), "{}: {:?}", entry.name, entry.worker.as_ref().err());
        }
        let tidy = entries.iter().find(|e| e.name == "old_downloads").unwrap();
        assert!(tidy.worker.as_ref().unwrap().logic.as_ref().unwrap().has_condition());
        let (report, ok) = check_files(&paths.worker_files());
        assert!(ok, "{report}");
    }

    #[test]
    fn check_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.awl");
        fs::write(&bad, "worker w:\n    run = \"x\"\n    timeout = \"5s\"\n").unwrap();
        let (report, ok) = check_files(&[bad]);
        assert!(!ok);
        assert!(report.contains("error[E0308]: mismatched types"), "{report}");
        assert!(report.contains("expected `duration`, found `str`"), "{report}");
    }

    #[test]
    fn log_names_are_safe() {
        let paths = WorkerPaths::under(Path::new("/x"));
        assert_eq!(paths.worker_log("disk-report"), Path::new("/x/logs/disk-report.log"));
        assert_eq!(paths.worker_log("../etc/passwd"), Path::new("/x/logs/_etc_passwd.log"));
    }
}
