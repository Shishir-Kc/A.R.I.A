//! Named loggers, each with an optional dedicated log file.
//!
//! Line format: `| 2026-11-10 12:00:00,123 | INFO | Linux.system | message |`
//! Lines always go to the logger's file (when it has one) and are mirrored to
//! stderr only when verbose mode is on (`aria -v ...`).

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Mirror every log line to stderr.
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        })
    }
}

pub struct Logger {
    name: &'static str,
    file: Option<PathBuf>,
    level: Level,
}

impl Logger {
    /// `name` e.g. "Linux.system"; `file` e.g. `~/.config/A.R.I.A/Logs/Linux/system.log`.
    pub fn new(name: &'static str, file: Option<PathBuf>) -> Self {
        Self { name, file, level: Level::Debug }
    }

    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    pub fn format(&self, level: Level, msg: &str) -> String {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S,%3f");
        format!("| {ts} | {level} | {} | {msg} |", self.name)
    }

    pub fn log(&self, level: Level, msg: impl AsRef<str>) {
        if level < self.level {
            return;
        }
        let line = self.format(level, msg.as_ref());
        if VERBOSE.load(Ordering::Relaxed) {
            eprintln!("{line}");
        }
        // Logging must never take the program down, so file errors are ignored.
        if let Some(path) = &self.file {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    pub fn debug(&self, msg: impl AsRef<str>) {
        self.log(Level::Debug, msg)
    }
    pub fn info(&self, msg: impl AsRef<str>) {
        self.log(Level::Info, msg)
    }
    pub fn warning(&self, msg: impl AsRef<str>) {
        self.log(Level::Warning, msg)
    }
    pub fn error(&self, msg: impl AsRef<str>) {
        self.log(Level::Error, msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_formatted_lines_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/test.log");
        let log = Logger::new("Test.logger", Some(path.clone())).with_level(Level::Info);
        log.debug("hidden");
        log.info("hello");
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("hidden"));
        assert!(text.trim_end().ends_with("| INFO | Test.logger | hello |"));
    }
}
