//! The worker daemon's control protocol: one JSON object per line over a
//! Unix socket, a request then its response.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AriaError, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    /// Run now, whatever the schedule.
    Run { name: String },
    /// Stop the current run and pause the worker (kept across daemon restarts).
    Stop { name: String },
    /// Resume a stopped or failed worker.
    Start { name: String },
    /// Stop the current run, then start again.
    Restart { name: String },
    /// Re-read the worker directory.
    Reload,
    /// Stop every worker and the daemon.
    Shutdown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// Nothing scheduled (a manual worker, or a finished one-shot).
    #[default]
    Idle,
    Scheduled,
    Running,
    /// Waiting to retry or restart after a failure.
    Backoff,
    /// Paused by the user.
    Stopped,
    /// Restarted too often; waits for the user.
    Failed,
    /// `"enabled": false` in its config.
    Disabled,
    /// Its config could not be loaded.
    Invalid,
}

impl WorkerState {
    pub fn label(self) -> &'static str {
        match self {
            WorkerState::Idle => "idle",
            WorkerState::Scheduled => "scheduled",
            WorkerState::Running => "running",
            WorkerState::Backoff => "backoff",
            WorkerState::Stopped => "stopped",
            WorkerState::Failed => "failed",
            WorkerState::Disabled => "disabled",
            WorkerState::Invalid => "invalid",
        }
    }
}

/// Everything known about one worker. Times are Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerStatus {
    pub name: String,
    pub description: String,
    pub schedule: String,
    pub command: String,
    pub state: WorkerState,
    pub pid: Option<u32>,
    pub error: Option<String>,
    /// Why the last scheduled run was skipped (an AWL `when:` condition).
    pub note: Option<String>,
    pub next_run: Option<i64>,
    pub last_start: Option<i64>,
    pub last_end: Option<i64>,
    /// Exit code; a process killed by signal N reports 128 + N.
    pub last_exit: Option<i32>,
    pub last_duration_ms: Option<u64>,
    pub runs: u64,
    pub failures: u64,
    pub restarts: u64,
    pub skipped: u64,
    pub log_file: String,
    pub source: String,
    pub spec: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonInfo {
    pub pid: u32,
    pub started_at: i64,
    pub version: String,
    pub socket: String,
    pub config_dir: String,
    pub log_dir: String,
    pub max_parallel: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Response {
    pub ok: bool,
    pub message: Option<String>,
    pub error: Option<String>,
    pub workers: Vec<WorkerStatus>,
    pub daemon: Option<DaemonInfo>,
}

impl Response {
    pub fn ok(message: impl Into<String>) -> Self {
        Self { ok: true, message: Some(message.into()), ..Default::default() }
    }

    pub fn err(error: impl Into<String>) -> Self {
        Self { ok: false, error: Some(error.into()), ..Default::default() }
    }
}

/// Sends one request to the daemon and waits (at most a few seconds) for the
/// answer. A refused request comes back as an error.
pub fn request(socket: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        AriaError::Other(format!(
            "the worker daemon is not running ({e}); start it with `aria worker daemon --detach`"
        ))
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;

    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer)?;
    let response: Response = serde_json::from_str(&answer)?;
    if !response.ok {
        return Err(AriaError::Other(response.error.unwrap_or_else(|| "request failed".into())));
    }
    Ok(response)
}

/// `12s`, `4m 05s`, `3h 20m`, `2d 4h`.
pub fn human_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..86_400 => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// `in 4m 05s` / `3h 20m ago` relative to `now`.
pub fn relative(ts: i64, now: i64) -> String {
    if ts >= now {
        format!("in {}", human_duration((ts - now) as u64))
    } else {
        format!("{} ago", human_duration((now - ts) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_tagged_json() {
        let json = serde_json::to_string(&Request::Run { name: "a".into() }).unwrap();
        assert_eq!(json, r#"{"cmd":"run","name":"a"}"#);
        assert_eq!(serde_json::from_str::<Request>(r#"{"cmd":"status"}"#).unwrap(), Request::Status);
    }

    #[test]
    fn human_times() {
        assert_eq!(human_duration(5), "5s");
        assert_eq!(human_duration(245), "4m 05s");
        assert_eq!(human_duration(12_000), "3h 20m");
        assert_eq!(relative(110, 100), "in 10s");
        assert_eq!(relative(100, 160), "1m 00s ago");
    }
}
