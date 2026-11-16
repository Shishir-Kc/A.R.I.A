//! The worker daemon: runs every worker on its schedule and supervises it.
//!
//! * One lightweight tokio task per worker, each asleep until its next
//!   deadline: an idle daemon does no work. Wall-clock sleeps are capped at a
//!   minute because tokio timers stand still while the machine is suspended.
//! * Each run is its own process group. Stopping it (by hand, by timeout, by
//!   `overlap: replace` or at shutdown) is always the same: SIGTERM to the
//!   group, a grace period, SIGKILL to the group, then reap.
//! * Output goes straight from the child into its log file (no copying through
//!   the daemon). Logs over 5 MB are copied to `.1` and truncated; children
//!   write in append mode, so they carry on at the new end.
//! * Failures are retried with exponential backoff plus jitter, and a worker
//!   that restarts too often is marked failed rather than restarted forever.
//! * Run history is kept in a state file, so schedules and counts survive a
//!   daemon restart.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::task::JoinHandle;

use super::WorkerPaths;
use super::awl::host::SystemHost;
use super::awl::{Action, StatusValue};
use super::ipc::{DaemonInfo, Request, Response, WorkerState, WorkerStatus};
use super::spec::{
    BACKOFF_BASE, BACKOFF_MAX, MIN_UPTIME, Overlap, RestartLimiter, RestartPolicy, Schedule, Worker,
    backoff_delay, load_dir, next_after, next_fire,
};
use crate::error::{AriaError, Result};
use crate::logger::Logger;
use crate::notifier::{Notification, push_notification};
use crate::paths::{home, log_file, read_json, write_json};

static LOG: LazyLock<Logger> =
    LazyLock::new(|| Logger::new("Worker.daemon", Some(log_file("worker-daemon.log"))));

/// How long a stopped run gets to exit after SIGTERM before SIGKILL.
const GRACE: Duration = Duration::from_secs(5);
/// Longest single sleep, so a suspend/resume is noticed within a minute.
const MAX_SLEEP: Duration = Duration::from_secs(60);
/// A worker log is rotated once it grows past this.
const LOG_LIMIT: u64 = 5 * 1024 * 1024;
/// How often the log size of a running worker is checked.
const LOG_CHECK: Duration = Duration::from_secs(30);
/// When the `when:` condition of a `daemon` worker says no, how long until
/// it is asked again.
const CONDITION_RECHECK: Duration = Duration::from_secs(30);
/// Longest an AWL condition or hook may take. Its probes read files, and a
/// path on a hung mount must not stall the daemon.
const EVAL_TIMEOUT: Duration = Duration::from_secs(5);
/// At most this many `trigger()` runs of one worker per [`TRIGGER_WINDOW`],
/// so workers that trigger each other can't loop at full speed.
const TRIGGER_LIMIT: u32 = 10;
const TRIGGER_WINDOW: Duration = Duration::from_secs(60);

/// What survives a daemon restart, per worker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    last_start: Option<i64>,
    last_end: Option<i64>,
    last_exit: Option<i32>,
    last_duration_ms: Option<u64>,
    runs: u64,
    failures: u64,
    paused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ctl {
    RunNow,
    Stop,
    Start,
    Restart,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum StopReason {
    Timeout,
    Stopped,
    Restarted,
    Replaced,
    Shutdown,
}

struct Shared {
    paths: WorkerPaths,
    statuses: Mutex<BTreeMap<String, WorkerStatus>>,
    persisted: Mutex<BTreeMap<String, Persisted>>,
    /// Bounds how many scheduled runs execute at once (daemon workers are
    /// long-running and don't take a slot).
    slots: Arc<Semaphore>,
    max_parallel: usize,
    started_at: i64,
    shutdown: Notify,
    /// Worker names an AWL hook asked to run (`trigger("name")`).
    triggers: mpsc::UnboundedSender<String>,
}

impl Shared {
    fn update(&self, name: &str, f: impl FnOnce(&mut WorkerStatus)) {
        if let Some(status) = self.statuses.lock().unwrap().get_mut(name) {
            f(status);
        }
    }

    fn persisted(&self, name: &str) -> Persisted {
        self.persisted.lock().unwrap().get(name).cloned().unwrap_or_default()
    }

    fn persist(&self, name: &str, f: impl FnOnce(&mut Persisted)) {
        let mut all = self.persisted.lock().unwrap();
        f(all.entry(name.to_owned()).or_default());
        if let Err(e) = write_json(&self.paths.state_file, &*all) {
            LOG.error(format!("could not save {}: {e}", self.paths.state_file.display()));
        }
    }
}

struct Handle {
    tx: mpsc::UnboundedSender<Ctl>,
    task: JoinHandle<()>,
    raw: Value,
}

pub struct Supervisor {
    shared: Arc<Shared>,
    handles: tokio::sync::Mutex<HashMap<String, Handle>>,
    trigger_rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

impl Supervisor {
    fn new(paths: WorkerPaths) -> Self {
        let persisted: BTreeMap<String, Persisted> = read_json(&paths.state_file)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let max_parallel = std::thread::available_parallelism().map_or(4, |n| n.get() * 2);
        let (triggers, trigger_rx) = mpsc::unbounded_channel();
        Self {
            shared: Arc::new(Shared {
                paths,
                statuses: Mutex::new(BTreeMap::new()),
                persisted: Mutex::new(persisted),
                slots: Arc::new(Semaphore::new(max_parallel)),
                max_parallel,
                started_at: Local::now().timestamp(),
                shutdown: Notify::new(),
                triggers,
            }),
            handles: tokio::sync::Mutex::new(HashMap::new()),
            trigger_rx: Mutex::new(Some(trigger_rx)),
        }
    }

    /// Syncs the running workers with the worker directory: new ones start,
    /// removed ones stop, changed ones restart, untouched ones carry on.
    async fn reload(&self) -> String {
        let entries = load_dir(&self.shared.paths.config_dir);
        let mut handles = self.handles.lock().await;

        let unchanged = |name: &str, handle: &Handle| {
            entries.iter().any(|e| e.name == name && e.worker.is_ok() && e.raw == handle.raw)
        };
        let outdated: Vec<String> =
            handles.iter().filter(|(n, h)| !unchanged(n, h)).map(|(n, _)| n.clone()).collect();
        for name in &outdated {
            if let Some(handle) = handles.remove(name) {
                let _ = handle.tx.send(Ctl::Shutdown);
                let _ = handle.task.await;
            }
        }

        let mut statuses = BTreeMap::new();
        let (mut started, mut invalid) = (0, 0);
        for entry in entries {
            let mut status = WorkerStatus {
                name: entry.name.clone(),
                source: entry.source.display().to_string(),
                spec: entry.raw.clone(),
                ..Default::default()
            };
            match &entry.worker {
                Err(error) => {
                    invalid += 1;
                    status.state = WorkerState::Invalid;
                    status.error = Some(error.clone());
                }
                Ok(worker) => {
                    status.description = worker.spec.description.clone();
                    status.schedule = worker.spec.schedule.clone();
                    status.command = worker.command.display();
                    status.log_file = self.shared.paths.worker_log(&worker.name).display().to_string();
                    let p = self.shared.persisted(&worker.name);
                    (status.last_start, status.last_end, status.last_exit) = (p.last_start, p.last_end, p.last_exit);
                    (status.last_duration_ms, status.runs, status.failures) = (p.last_duration_ms, p.runs, p.failures);
                    if let Some(old) = self.shared.statuses.lock().unwrap().get(&worker.name) {
                        (status.restarts, status.skipped) = (old.restarts, old.skipped);
                        if handles.contains_key(&worker.name) {
                            status = old.clone();
                        }
                    }
                    if !worker.spec.enabled {
                        status.state = WorkerState::Disabled;
                    } else if !handles.contains_key(&worker.name) {
                        let (tx, rx) = mpsc::unbounded_channel();
                        let task = tokio::spawn(worker_task(worker.clone(), self.shared.clone(), rx));
                        handles.insert(worker.name.clone(), Handle { tx, task, raw: entry.raw.clone() });
                        started += 1;
                    }
                }
            }
            statuses.insert(entry.name, status);
        }
        let removed = outdated.iter().filter(|n| !handles.contains_key(*n)).count();
        let message = format!(
            "{} workers: {started} started, {removed} stopped, {invalid} invalid",
            statuses.len()
        );
        *self.shared.statuses.lock().unwrap() = statuses;
        LOG.info(&message);
        message
    }

    async fn control(&self, name: &str, ctl: Ctl) -> Response {
        let handles = self.handles.lock().await;
        match handles.get(name) {
            Some(handle) if handle.tx.send(ctl).is_ok() => Response::ok(format!("{ctl:?} sent to {name}")),
            Some(_) => Response::err(format!("{name} is not running")),
            None => match self.shared.statuses.lock().unwrap().get(name) {
                Some(s) if s.state == WorkerState::Disabled => {
                    Response::err(format!("{name} is disabled in its config (\"enabled\": false)"))
                }
                Some(s) => Response::err(format!("{name} is invalid: {}", s.error.clone().unwrap_or_default())),
                None => Response::err(format!("no worker named '{name}'")),
            },
        }
    }

    async fn handle(&self, request: Request) -> Response {
        match request {
            Request::Ping => Response::ok("pong"),
            Request::Status => {
                let shared = &self.shared;
                Response {
                    ok: true,
                    workers: shared.statuses.lock().unwrap().values().cloned().collect(),
                    daemon: Some(DaemonInfo {
                        pid: std::process::id(),
                        started_at: shared.started_at,
                        version: env!("CARGO_PKG_VERSION").into(),
                        socket: shared.paths.socket.display().to_string(),
                        config_dir: shared.paths.config_dir.display().to_string(),
                        log_dir: shared.paths.log_dir.display().to_string(),
                        max_parallel: shared.max_parallel,
                    }),
                    ..Default::default()
                }
            }
            Request::Run { name } => self.control(&name, Ctl::RunNow).await,
            Request::Stop { name } => self.control(&name, Ctl::Stop).await,
            Request::Start { name } => self.control(&name, Ctl::Start).await,
            Request::Restart { name } => self.control(&name, Ctl::Restart).await,
            Request::Reload => Response::ok(self.reload().await),
            Request::Shutdown => {
                self.shared.shutdown.notify_one();
                Response::ok("shutting down")
            }
        }
    }

    async fn shutdown_all(&self) {
        let mut handles = self.handles.lock().await;
        for handle in handles.values() {
            let _ = handle.tx.send(Ctl::Shutdown);
        }
        for (_, handle) in handles.drain() {
            let _ = handle.task.await;
        }
    }
}

/// Per-worker state that only lives as long as the daemon.
#[derive(Default)]
struct Runtime {
    paused: bool,
    failed: bool,
    run_now: bool,
    /// The pending run was asked for by a person (or a trigger), so it skips
    /// the `when:` condition.
    manual: bool,
    startup_done: bool,
    daemon_done: bool,
    retry_at: Option<DateTime<Local>>,
    /// Set when a `when:` condition said no: the slot it was asked for is
    /// used up, and the next one starts no earlier than this.
    hold_until: Option<DateTime<Local>>,
    /// Consecutive failures (backoff exponent).
    attempt: u32,
    /// Retries used for the current scheduled run.
    retries_used: u32,
}

impl Runtime {
    fn next_run(&self, w: &Worker, now: DateTime<Local>, last_start: Option<DateTime<Local>>) -> Option<DateTime<Local>> {
        if self.run_now {
            return Some(now);
        }
        if self.paused || self.failed {
            return None;
        }
        if self.retry_at.is_some() {
            return self.retry_at;
        }
        if let Some(hold) = self.hold_until.filter(|h| *h > now) {
            return Some(hold);
        }
        match &w.schedule {
            Schedule::Startup => (!self.startup_done).then_some(now),
            Schedule::Daemon => (!self.daemon_done).then_some(now),
            Schedule::Manual => None,
            timed => {
                let jitter = w.jitter.map_or(Duration::ZERO, |j| j.mul_f64(fastrand::f64()));
                next_fire(timed, now, last_start, w.spec.catch_up)
                    .map(|t| t + chrono::Duration::from_std(jitter).unwrap_or_default())
            }
        }
    }

    fn idle_state(&self, scheduled: bool) -> WorkerState {
        if self.paused {
            WorkerState::Stopped
        } else if self.failed {
            WorkerState::Failed
        } else if self.retry_at.is_some() {
            WorkerState::Backoff
        } else if scheduled {
            WorkerState::Scheduled
        } else {
            WorkerState::Idle
        }
    }

    /// Applies a command that arrived while no run was going. Returns false
    /// when the worker must exit.
    fn apply(&mut self, ctl: Ctl, shared: &Shared, name: &str) -> bool {
        match ctl {
            Ctl::RunNow => (self.run_now, self.manual) = (true, true),
            Ctl::Stop => {
                self.paused = true;
                self.retry_at = None;
                shared.persist(name, |p| p.paused = true);
            }
            Ctl::Start | Ctl::Restart => {
                self.resume(shared, name);
                self.run_now = ctl == Ctl::Restart;
                self.manual = ctl == Ctl::Restart;
            }
            Ctl::Shutdown => return false,
        }
        true
    }

    fn resume(&mut self, shared: &Shared, name: &str) {
        (self.paused, self.failed, self.daemon_done) = (false, false, false);
        (self.attempt, self.retries_used, self.retry_at, self.hold_until) = (0, 0, None, None);
        shared.persist(name, |p| p.paused = false);
    }
}

fn from_ts(ts: Option<i64>) -> Option<DateTime<Local>> {
    ts.and_then(|t| Local.timestamp_opt(t, 0).single())
}

/// Sleeps until `t` (in capped steps) unless a command arrives first.
async fn wait_until(t: DateTime<Local>, rx: &mut mpsc::UnboundedReceiver<Ctl>) -> Option<Ctl> {
    loop {
        let left = (t - Local::now()).to_std().unwrap_or(Duration::ZERO);
        if left.is_zero() {
            return None;
        }
        tokio::select! {
            _ = tokio::time::sleep(left.min(MAX_SLEEP)) => {}
            ctl = rx.recv() => return Some(ctl.unwrap_or(Ctl::Shutdown)),
        }
    }
}

async fn worker_task(w: Worker, shared: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<Ctl>) {
    let name = w.name.clone();
    let mut rt = Runtime { paused: shared.persisted(&name).paused, ..Default::default() };
    let mut limiter = RestartLimiter::new(w.max_restarts, w.restart_window);

    loop {
        let next = rt.next_run(&w, Local::now(), from_ts(shared.persisted(&name).last_start));
        shared.update(&name, |s| {
            s.state = rt.idle_state(next.is_some());
            s.next_run = next.map(|t| t.timestamp());
            s.pid = None;
        });

        let woken_by = match next {
            Some(t) => wait_until(t, &mut rx).await,
            None => Some(rx.recv().await.unwrap_or(Ctl::Shutdown)),
        };
        if let Some(ctl) = woken_by {
            if !rt.apply(ctl, &shared, &name) {
                return;
            }
            continue;
        }

        // A run asked for by hand, or the retry of a failed scheduled run,
        // skips the `when:` condition. A `daemon` worker asks it again before
        // every restart, and a run started by `overlap: replace` asks too.
        let manual = std::mem::take(&mut rt.manual);
        let retry = rt.retry_at.is_some() && !matches!(w.schedule, Schedule::Daemon);
        rt.run_now = false;
        rt.retry_at = None;
        if !manual && !retry && !condition_allows(&w, &shared, &mut rt).await {
            continue;
        }
        let run = run_once(&w, &shared, &mut rx, &mut rt).await;
        if run.reason == Some(StopReason::Shutdown) {
            return;
        }

        let failed = match run.reason {
            None => run.exit != Some(0),
            Some(StopReason::Timeout) => true,
            // Stopped, restarted or replaced on purpose: no retry logic.
            Some(_) => continue,
        };
        if react(&w, &shared, &mut rt, &run, failed).await {
            continue;
        }
        let now = Local::now();
        match w.schedule {
            Schedule::Daemon => {
                let restart = match w.restart {
                    RestartPolicy::Always => true,
                    RestartPolicy::OnFailure => failed,
                    RestartPolicy::Never => false,
                };
                if !restart {
                    rt.daemon_done = true;
                    continue;
                }
                if run.uptime >= MIN_UPTIME {
                    rt.attempt = 0;
                }
                if !limiter.allow(Instant::now()) {
                    rt.failed = true;
                    let msg = format!(
                        "gave up: more than {} restarts within {}s; start it again by hand",
                        w.max_restarts,
                        w.restart_window.as_secs()
                    );
                    append_log(&shared.paths.worker_log(&name), &format!("── {msg} ──"));
                    LOG.warning(format!("{name}: {msg}"));
                    shared.update(&name, |s| s.error = Some(msg));
                    continue;
                }
                let delay = backoff_delay(rt.attempt, BACKOFF_BASE, BACKOFF_MAX, fastrand::f64());
                rt.attempt += 1;
                rt.retry_at = Some(now + chrono::Duration::from_std(delay).unwrap_or_default());
                shared.update(&name, |s| s.restarts += 1);
            }
            _ => {
                rt.startup_done = true;
                if failed && rt.retries_used < w.spec.retries {
                    let delay = backoff_delay(rt.retries_used, BACKOFF_BASE, BACKOFF_MAX, fastrand::f64());
                    rt.retries_used += 1;
                    rt.retry_at = Some(now + chrono::Duration::from_std(delay).unwrap_or_default());
                } else {
                    rt.retries_used = 0;
                }
            }
        }
    }
}

/// What a script sees as `status`; `run` is the run that just ended, if any.
fn status_value(w: &Worker, shared: &Shared, rt: &Runtime, run: Option<&RunResult>) -> StatusValue {
    let p = shared.persisted(&w.name);
    let (exit, duration_ms) = match run {
        Some(run) => (run.exit, run.uptime.as_millis() as i64),
        None => (p.last_exit, p.last_duration_ms.unwrap_or(0) as i64),
    };
    StatusValue {
        name: w.name.clone(),
        runs: p.runs as i64,
        failures: p.failures as i64,
        exit: exit.map_or(-1, i64::from),
        success: exit == Some(0),
        duration_ms,
        retry: rt.retries_used as i64,
    }
}

/// Runs AWL evaluation on the blocking pool, so a probe stuck on a hung
/// mount can't stall the daemon's single thread. `None` after
/// [`EVAL_TIMEOUT`] (the stuck thread is left to finish on its own).
async fn evaluate<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::time::timeout(EVAL_TIMEOUT, tokio::task::spawn_blocking(f)).await.ok()?.ok()
}

/// Asks the worker's `when:` condition whether this scheduled run goes ahead.
/// When it says no (or fails), the slot is used up so the worker sleeps
/// until its next one instead of asking again straight away.
async fn condition_allows(w: &Worker, shared: &Shared, rt: &mut Runtime) -> bool {
    let Some(logic) = w.logic.clone().filter(|l| l.has_condition()) else { return true };
    let status = status_value(w, shared, rt, None);
    let verdict = evaluate(move || logic.should_run(&SystemHost, status))
        .await
        .unwrap_or_else(|| Err(format!("`when:` took longer than {}s; counted as no", EVAL_TIMEOUT.as_secs())));
    let reason = match verdict {
        Ok(true) => return true,
        Ok(false) => "`when:` said no".to_owned(),
        Err(e) => {
            append_log(&shared.paths.worker_log(&w.name), &format!("── {e} ──"));
            LOG.warning(format!("{}: {e}", w.name));
            e
        }
    };
    let now = Local::now();
    match &w.schedule {
        Schedule::Startup => rt.startup_done = true,
        Schedule::Daemon => {
            rt.hold_until = Some(now + chrono::Duration::from_std(CONDITION_RECHECK).unwrap_or_default());
        }
        timed => rt.hold_until = next_after(timed, now),
    }
    let note = format!("skipped at {}: {reason}", now.format("%H:%M:%S"));
    shared.update(&w.name, |s| {
        s.skipped += 1;
        s.note = Some(note);
    });
    false
}

/// Runs the `on_success:` / `on_failure:` hook and carries out what it asks
/// for. Returns true when the hook paused the worker.
async fn react(w: &Worker, shared: &Shared, rt: &mut Runtime, run: &RunResult, failed: bool) -> bool {
    let Some(logic) = w.logic.clone() else { return false };
    let log_path = shared.paths.worker_log(&w.name);
    let status = status_value(w, shared, rt, Some(run));
    let (actions, error) = evaluate(move || logic.react(!failed, &SystemHost, status)).await.unwrap_or_else(|| {
        (Vec::new(), Some(format!("the hook took longer than {}s and was abandoned", EVAL_TIMEOUT.as_secs())))
    });
    if let Some(e) = error {
        append_log(&log_path, &format!("── {e} ──"));
        LOG.warning(format!("{}: {e}", w.name));
    }
    let hook = if failed { "on_failure" } else { "on_success" };
    let mut paused = false;
    for action in actions {
        match action {
            Action::Log(message) => append_log(&log_path, &format!("» {message}")),
            Action::Notify { title, body } => {
                // notify-send is a short blocking call; keep it off the daemon's one thread.
                tokio::task::spawn_blocking(move || push_notification(&Notification { title, body }, 5000));
            }
            Action::Trigger(name) => {
                append_log(&log_path, &format!("── {hook}: trigger {name} ──"));
                let _ = shared.triggers.send(name);
            }
            Action::Pause => paused = true,
        }
    }
    if paused {
        append_log(&log_path, &format!("── paused by {hook}; start it again by hand ──"));
        rt.apply(Ctl::Stop, shared, &w.name);
    }
    paused
}

struct RunResult {
    exit: Option<i32>,
    uptime: Duration,
    reason: Option<StopReason>,
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().or_else(|| status.signal().map(|s| 128 + s)).unwrap_or(-1)
}

async fn maybe_sleep(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// Runs the worker's command once, until it exits or is stopped.
async fn run_once(
    w: &Worker,
    shared: &Shared,
    rx: &mut mpsc::UnboundedReceiver<Ctl>,
    rt: &mut Runtime,
) -> RunResult {
    let name = &w.name;
    let failed_to_start = |reason| RunResult { exit: None, uptime: Duration::ZERO, reason };

    // Scheduled runs share a bounded number of slots.
    let _permit = if matches!(w.schedule, Schedule::Daemon) {
        None
    } else {
        loop {
            tokio::select! {
                permit = shared.slots.clone().acquire_owned() => break permit.ok(),
                ctl = rx.recv() => match ctl.unwrap_or(Ctl::Shutdown) {
                    Ctl::Shutdown => return failed_to_start(Some(StopReason::Shutdown)),
                    Ctl::Stop => {
                        rt.apply(Ctl::Stop, shared, name);
                        return failed_to_start(Some(StopReason::Stopped));
                    }
                    _ => {}
                },
            }
        }
    };

    let log_path = shared.paths.worker_log(name);
    rotate_if_big(&log_path);
    let started_at = Local::now();
    let run_no = shared.persisted(name).runs + 1;
    shared.persist(name, |p| {
        p.runs += 1;
        p.last_start = Some(started_at.timestamp());
    });
    append_log(&log_path, &format!("── run #{run_no} · {} ──", started_at.format("%Y-%m-%d %H:%M:%S")));

    let child = open_log(&log_path).and_then(|log| spawn(w, log));
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            append_log(&log_path, &format!("── could not start: {e} ──"));
            LOG.error(format!("{name}: could not start: {e}"));
            finish(shared, name, started_at, None, Duration::ZERO, None);
            return RunResult { exit: Some(127), uptime: Duration::ZERO, reason: None };
        }
    };
    let pid = child.id();
    shared.update(name, |s| {
        s.state = WorkerState::Running;
        s.pid = pid;
        s.next_run = None;
        s.last_start = Some(started_at.timestamp());
        s.runs = run_no;
        s.error = None;
        s.note = None;
    });

    let started = Instant::now();
    let deadline = w.timeout;
    let mut tick = next_after(&w.schedule, started_at);
    let mut log_check = tokio::time::interval(LOG_CHECK);
    log_check.tick().await;
    let mut reason = None;

    let status = loop {
        let until_deadline = deadline.map(|d| d.saturating_sub(started.elapsed()));
        let until_tick = tick.map(|t| (t - Local::now()).to_std().unwrap_or(Duration::ZERO).min(MAX_SLEEP));
        tokio::select! {
            status = child.wait() => break status.ok(),
            _ = maybe_sleep(until_deadline) => {
                reason = Some(StopReason::Timeout);
                append_log(&log_path, &format!("── timed out after {}s ──", deadline.unwrap_or_default().as_secs()));
                break stop_child(&mut child).await;
            }
            _ = maybe_sleep(until_tick) => {
                let now = Local::now();
                if tick.is_some_and(|t| t <= now) {
                    match w.spec.overlap {
                        Overlap::Skip => {
                            shared.update(name, |s| s.skipped += 1);
                            tick = next_after(&w.schedule, now);
                        }
                        Overlap::Replace => {
                            reason = Some(StopReason::Replaced);
                            rt.run_now = true;
                            break stop_child(&mut child).await;
                        }
                    }
                }
            }
            _ = log_check.tick() => rotate_if_big(&log_path),
            ctl = rx.recv() => match ctl.unwrap_or(Ctl::Shutdown) {
                Ctl::Stop => {
                    rt.apply(Ctl::Stop, shared, name);
                    reason = Some(StopReason::Stopped);
                    break stop_child(&mut child).await;
                }
                Ctl::Restart => {
                    rt.resume(shared, name);
                    (rt.run_now, rt.manual) = (true, true);
                    reason = Some(StopReason::Restarted);
                    break stop_child(&mut child).await;
                }
                Ctl::Shutdown => {
                    reason = Some(StopReason::Shutdown);
                    break stop_child(&mut child).await;
                }
                // Already running.
                Ctl::RunNow | Ctl::Start => {}
            },
        }
    };

    let uptime = started.elapsed();
    let exit = status.map(exit_code);
    let why = reason.map(|r| format!(" ({r:?})").to_lowercase()).unwrap_or_default();
    let code = exit.map_or("?".to_string(), |c| c.to_string());
    append_log(&log_path, &format!("── exit {code} after {:.1}s{why} ──", uptime.as_secs_f64()));
    let counts_as_failure = match reason {
        None => exit != Some(0),
        Some(r) => r == StopReason::Timeout,
    };
    finish(shared, name, started_at, exit, uptime, Some(counts_as_failure));
    RunResult { exit, uptime, reason }
}

/// Records the end of a run. `failed: None` means it never started (always a failure).
fn finish(
    shared: &Shared,
    name: &str,
    started_at: DateTime<Local>,
    exit: Option<i32>,
    uptime: Duration,
    failed: Option<bool>,
) {
    let end = Local::now().timestamp();
    let failed = failed.unwrap_or(true);
    let ms = uptime.as_millis() as u64;
    shared.persist(name, |p| {
        p.last_end = Some(end);
        p.last_exit = exit;
        p.last_duration_ms = Some(ms);
        p.failures += failed as u64;
    });
    let failures = shared.persisted(name).failures;
    shared.update(name, |s| {
        s.last_start = Some(started_at.timestamp());
        s.last_end = Some(end);
        s.last_exit = exit;
        s.last_duration_ms = Some(ms);
        s.failures = failures;
        s.pid = None;
    });
}

fn spawn(w: &Worker, log: File) -> std::io::Result<Child> {
    let (program, args) = w.command.program();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .envs(&w.spec.env)
        .env("ARIA_WORKER", &w.name)
        // Its own process group, so stopping it also stops what it started.
        .process_group(0)
        .kill_on_drop(true);
    if let Some(cwd) = &w.spec.cwd {
        command.current_dir(expand_home(cwd));
    }
    command.spawn()
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

fn signal_group(pgid: u32, sig: i32) {
    // SAFETY: plain syscall. The group leader is our unreaped child, so its pid
    // (and with it the group id) cannot have been reused.
    unsafe {
        libc::kill(-(pgid as i32), sig);
    }
}

/// SIGTERM to the run's process group, SIGKILL after [`GRACE`], then reap.
async fn stop_child(child: &mut Child) -> Option<ExitStatus> {
    // `id()` is None once the child has been reaped: never signal after that.
    if let Some(pid) = child.id() {
        signal_group(pid, libc::SIGTERM);
    }
    match tokio::time::timeout(GRACE, child.wait()).await {
        Ok(status) => status.ok(),
        Err(_) => {
            if let Some(pid) = child.id() {
                signal_group(pid, libc::SIGKILL);
            }
            child.wait().await.ok()
        }
    }
}

fn open_log(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn append_log(path: &Path, line: &str) {
    if let Ok(mut file) = open_log(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Copy-and-truncate rotation: safe while a child is writing, because the
/// child's file is in append mode and simply continues at the new end.
fn rotate_if_big(path: &Path) {
    if fs::metadata(path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        if fs::copy(path, &old).is_ok()
            && let Ok(file) = OpenOptions::new().write(true).open(path)
        {
            let _ = file.set_len(0);
        }
    }
}

async fn serve_connection(stream: UnixStream, supervisor: Arc<Supervisor>) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => supervisor.handle(request).await,
            Err(e) => Response::err(format!("bad request: {e}")),
        };
        let Ok(mut out) = serde_json::to_vec(&response) else { break };
        out.push(b'\n');
        if writer.write_all(&out).await.is_err() {
            break;
        }
    }
}

/// Holds the single-instance lock for as long as it lives.
struct InstanceLock(#[allow(dead_code)] File);

fn lock_instance(path: &Path) -> Result<InstanceLock> {
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    // SAFETY: flock on a descriptor we own; released when the file is closed.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(AriaError::Other("the worker daemon is already running".into()));
    }
    Ok(InstanceLock(file))
}

/// Runs the daemon in the foreground until it gets SIGTERM / SIGINT or a
/// `shutdown` request. SIGHUP reloads the worker directory. On the way out
/// every running worker is stopped.
pub fn run_daemon(paths: WorkerPaths) -> Result<()> {
    fs::create_dir_all(&paths.config_dir)?;
    fs::create_dir_all(&paths.log_dir)?;
    fs::create_dir_all(&paths.runtime_dir)?;
    fs::set_permissions(&paths.runtime_dir, fs::Permissions::from_mode(0o700))?;
    let _lock = lock_instance(&paths.lock_file)?;
    // The lock is ours, so any socket file left behind is stale.
    let _ = fs::remove_file(&paths.socket);

    // One thread is plenty: the daemon only waits and spawns processes.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let listener = UnixListener::bind(&paths.socket).map_err(|e| {
            AriaError::Other(format!("could not open the socket {}: {e}", paths.socket.display()))
        })?;
        fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
        let supervisor = Arc::new(Supervisor::new(paths.clone()));
        let mut triggers = supervisor.trigger_rx.lock().unwrap().take().expect("taken once");
        let mut trigger_limits: HashMap<String, RestartLimiter> = HashMap::new();
        let loaded = supervisor.reload().await;
        LOG.info(format!("worker daemon {} listening on {}", std::process::id(), paths.socket.display()));
        println!("worker daemon listening on {}\n{loaded}", paths.socket.display());

        let mut term = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut hangup = signal(SignalKind::hangup())?;
        loop {
            tokio::select! {
                conn = listener.accept() => match conn {
                    Ok((stream, _)) => { tokio::spawn(serve_connection(stream, supervisor.clone())); }
                    Err(e) => LOG.error(format!("accept failed: {e}")),
                },
                _ = hangup.recv() => { supervisor.reload().await; }
                Some(name) = triggers.recv() => {
                    let limiter = trigger_limits
                        .entry(name.clone())
                        .or_insert_with(|| RestartLimiter::new(TRIGGER_LIMIT, TRIGGER_WINDOW));
                    if !limiter.allow(Instant::now()) {
                        let msg = format!(
                            "── trigger ignored: more than {TRIGGER_LIMIT} triggers within {}s (a trigger loop?) ──",
                            TRIGGER_WINDOW.as_secs()
                        );
                        append_log(&paths.worker_log(&name), &msg);
                        LOG.warning(format!("trigger({name}) ignored: too many"));
                        continue;
                    }
                    let response = supervisor.control(&name, Ctl::RunNow).await;
                    if !response.ok {
                        LOG.warning(format!("trigger({name}): {}", response.error.unwrap_or_default()));
                    }
                }
                _ = term.recv() => break,
                _ = interrupt.recv() => break,
                _ = supervisor.shared.shutdown.notified() => break,
            }
        }
        LOG.info("stopping every worker");
        supervisor.shutdown_all().await;
        let _ = fs::remove_file(&paths.socket);
        LOG.info("worker daemon stopped");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::ipc::request;

    fn write_workers(paths: &WorkerPaths, json: &str) {
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(paths.config_dir.join("test.json"), json).unwrap();
    }

    fn status_of(paths: &WorkerPaths, name: &str) -> WorkerStatus {
        let response = request(&paths.socket, &Request::Status).unwrap();
        response.workers.into_iter().find(|w| w.name == name).unwrap()
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let start = Instant::now();
        while !ok() {
            assert!(start.elapsed() < Duration::from_secs(15), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// AWL workers in the real daemon: conditions, hooks, triggers, pause.
    #[test]
    fn daemon_runs_awl_conditions_and_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::under(dir.path());
        let flag = dir.path().join("flag");
        fs::create_dir_all(&paths.config_dir).unwrap();
        let src = format!(
            r#"
worker gated:
    run = "echo gated ran"
    schedule = every(1s)
    when: file_exists("{flag}")

worker chained:
    run = "echo first"
    on_success:
        log(f"hook ran after run #{{status.runs}}")
        trigger("second")

worker second:
    run = "echo second ran"

worker pauser:
    run = "exit 4"
    schedule = Schedule.daemon
    on_failure:
        if status.exit == 4:
            pause()

worker ping:
    run = "true"
    on_success: trigger("pong")

worker pong:
    run = "true"
    on_success: trigger("ping")

worker gated_service:
    run = "exit 1"
    schedule = Schedule.daemon
    when: status.runs < 1
"#,
            flag = flag.display()
        );
        fs::write(paths.config_dir.join("test.awl"), src).unwrap();
        let daemon_paths = paths.clone();
        let daemon = std::thread::spawn(move || run_daemon(daemon_paths));
        wait_for("socket", || request(&paths.socket, &Request::Ping).is_ok());

        // A false condition skips each slot once, without spinning.
        wait_for("gated to skip", || status_of(&paths, "gated").skipped >= 2);
        let gated = status_of(&paths, "gated");
        assert_eq!(gated.runs, 0);
        assert!(gated.note.unwrap().contains("`when:` said no"));
        std::thread::sleep(Duration::from_millis(1500));
        let skipped = status_of(&paths, "gated").skipped;
        assert!(skipped <= 6, "skipped {skipped} times in a few seconds: spinning");

        // Running by hand bypasses the condition.
        request(&paths.socket, &Request::Run { name: "gated".into() }).unwrap();
        wait_for("gated manual run", || status_of(&paths, "gated").last_exit == Some(0));
        assert_eq!(status_of(&paths, "gated").runs, 1);

        // Once the condition holds, scheduled runs go ahead.
        fs::write(&flag, "").unwrap();
        wait_for("gated scheduled run", || status_of(&paths, "gated").runs >= 2);

        // on_success logs and triggers another worker.
        request(&paths.socket, &Request::Run { name: "chained".into() }).unwrap();
        wait_for("second to be triggered", || status_of(&paths, "second").last_exit == Some(0));
        let log = fs::read_to_string(paths.worker_log("chained")).unwrap();
        assert!(log.contains("» hook ran after run #1"), "{log}");
        assert!(log.contains("trigger second"), "{log}");

        // on_failure pauses a crashing daemon worker instead of restarting it.
        wait_for("pauser to pause", || status_of(&paths, "pauser").state == WorkerState::Stopped);
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(status_of(&paths, "pauser").runs, 1);
        assert_eq!(status_of(&paths, "pauser").restarts, 0);

        // A daemon worker asks `when:` again before each restart.
        wait_for("gated_service to be held", || status_of(&paths, "gated_service").skipped >= 1);
        assert_eq!(status_of(&paths, "gated_service").runs, 1);

        // Workers triggering each other are slowed down, not looped forever.
        request(&paths.socket, &Request::Run { name: "ping".into() }).unwrap();
        wait_for("the trigger loop to be cut", || {
            fs::read_to_string(paths.worker_log("ping")).unwrap_or_default().contains("trigger ignored")
                || fs::read_to_string(paths.worker_log("pong")).unwrap_or_default().contains("trigger ignored")
        });
        let runs = status_of(&paths, "ping").runs + status_of(&paths, "pong").runs;
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(status_of(&paths, "ping").runs + status_of(&paths, "pong").runs, runs);
        assert!(runs <= 2 * u64::from(TRIGGER_LIMIT) + 2, "{runs}");

        // Editing a hook counts as a change: the worker is reloaded.
        let edited = fs::read_to_string(paths.config_dir.join("test.awl")).unwrap().replace("hook ran", "edited hook ran");
        fs::write(paths.config_dir.join("test.awl"), edited).unwrap();
        request(&paths.socket, &Request::Reload).unwrap();
        request(&paths.socket, &Request::Run { name: "chained".into() }).unwrap();
        wait_for("edited hook", || {
            fs::read_to_string(paths.worker_log("chained")).unwrap_or_default().contains("edited hook ran")
        });

        // A file that doesn't compile is one invalid entry, with the position.
        fs::write(paths.config_dir.join("zz-broken.awl"), "worker w:\n    run = 5\n").unwrap();
        request(&paths.socket, &Request::Reload).unwrap();
        let broken = status_of(&paths, "zz-broken.awl");
        assert_eq!(broken.state, WorkerState::Invalid);
        assert!(broken.error.unwrap().contains("zz-broken.awl:2:11: error[E0308]"));

        request(&paths.socket, &Request::Shutdown).unwrap();
        daemon.join().unwrap().unwrap();
    }

    /// The whole daemon, end to end, in a temp directory.
    #[test]
    fn daemon_runs_supervises_and_stops_workers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::under(dir.path());
        write_workers(
            &paths,
            r#"{
                "once": {"command": ["sh", "-c", "echo hello from once"], "schedule": "on_startup"},
                "crasher": {"command": "exit 3", "schedule": "daemon", "max_restarts": 2, "restart_window": "1m"},
                "sleeper": {"command": "sleep 60", "schedule": "daemon"},
                "slow": {"command": "sleep 60", "schedule": "manual", "timeout": "300ms"},
                "busy": {"command": "sleep 60", "schedule": "every 1s", "overlap": "skip"},
                "fresh-each-time": {"command": "sleep 3", "schedule": "every 1s", "overlap": "replace"},
                "broken": {"schedule": "daemon"}
            }"#,
        );
        let daemon_paths = paths.clone();
        let daemon = std::thread::spawn(move || run_daemon(daemon_paths));
        wait_for("socket", || request(&paths.socket, &Request::Ping).is_ok());

        // A second daemon is refused.
        assert!(run_daemon(paths.clone()).is_err());

        // One-shot ran, exited 0, and its output is in its log.
        wait_for("once to finish", || status_of(&paths, "once").last_exit == Some(0));
        let log = fs::read_to_string(paths.worker_log("once")).unwrap();
        assert!(log.contains("hello from once"), "{log}");

        // A crash-looping worker is restarted with backoff, then given up on.
        wait_for("crasher to fail", || status_of(&paths, "crasher").state == WorkerState::Failed);
        assert_eq!(status_of(&paths, "crasher").restarts, 2);

        // Invalid config is reported, not fatal.
        let broken = status_of(&paths, "broken");
        assert_eq!(broken.state, WorkerState::Invalid);
        assert!(broken.error.unwrap().contains("command"));

        // Timeout kills the run.
        request(&paths.socket, &Request::Run { name: "slow".into() }).unwrap();
        wait_for("slow to time out", || status_of(&paths, "slow").last_exit == Some(128 + libc::SIGTERM));
        assert_eq!(status_of(&paths, "slow").failures, 1);

        // Stop pauses a long-running worker and kills its process.
        wait_for("sleeper to run", || status_of(&paths, "sleeper").pid.is_some());
        let pid = status_of(&paths, "sleeper").pid.unwrap() as i32;
        request(&paths.socket, &Request::Stop { name: "sleeper".into() }).unwrap();
        wait_for("sleeper to stop", || status_of(&paths, "sleeper").state == WorkerState::Stopped);
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "process still alive");
        request(&paths.socket, &Request::Start { name: "sleeper".into() }).unwrap();
        wait_for("sleeper to run again", || status_of(&paths, "sleeper").state == WorkerState::Running);

        // Restart replaces the process.
        wait_for("sleeper pid", || status_of(&paths, "sleeper").pid.is_some());
        let before = status_of(&paths, "sleeper").pid.unwrap();
        request(&paths.socket, &Request::Restart { name: "sleeper".into() }).unwrap();
        wait_for("a new sleeper process", || status_of(&paths, "sleeper").pid.is_some_and(|p| p != before));
        assert_ne!(unsafe { libc::kill(before as i32, 0) }, 0, "old process still alive");

        // Overlap: a long job every 1s is skipped while it runs; a 3s one is replaced.
        wait_for("busy to skip a run", || status_of(&paths, "busy").skipped >= 1);
        assert_eq!(status_of(&paths, "busy").runs, 1);
        wait_for("fresh-each-time to be replaced", || status_of(&paths, "fresh-each-time").runs >= 2);
        assert_eq!(status_of(&paths, "fresh-each-time").last_exit, Some(128 + libc::SIGTERM));

        // Unknown worker.
        assert!(request(&paths.socket, &Request::Run { name: "nope".into() }).is_err());

        // Reload picks up a new worker and drops removed ones.
        write_workers(&paths, r#"{"fresh": {"command": "true", "schedule": "manual"}}"#);
        request(&paths.socket, &Request::Reload).unwrap();
        let names: Vec<String> =
            request(&paths.socket, &Request::Status).unwrap().workers.into_iter().map(|w| w.name).collect();
        assert_eq!(names, ["fresh"]);

        request(&paths.socket, &Request::Shutdown).unwrap();
        daemon.join().unwrap().unwrap();
        assert!(!paths.socket.exists());
    }
}
