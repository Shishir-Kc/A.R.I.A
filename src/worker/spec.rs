//! Worker definitions: the JSON a user (or an agent) writes, and the pure
//! scheduling maths the daemon runs on. Nothing here sleeps or touches a
//! process, so all of it is tested with fixed clocks.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use croner::Cron;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::awl;
use crate::paths::read_json;

/// First retry / restart delay; it doubles on every consecutive failure.
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// The longest a retry or restart is ever delayed.
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// A long-running worker that stayed up this long is healthy again, and its
/// backoff starts over.
pub const MIN_UPTIME: Duration = Duration::from_secs(10);
/// Restart intensity: at most this many restarts...
pub const DEFAULT_MAX_RESTARTS: u32 = 5;
/// ...within this window, or the worker is marked failed.
pub const DEFAULT_RESTART_WINDOW: Duration = Duration::from_secs(60);

/// What to run: a string goes through `sh -c`, a list is executed directly
/// (no shell, so no quoting surprises and one process less).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandSpec {
    Shell(String),
    Argv(Vec<String>),
}

impl CommandSpec {
    /// The program and its arguments.
    pub fn program(&self) -> (&str, Vec<&str>) {
        match self {
            CommandSpec::Shell(script) => ("sh", vec!["-c", script.as_str()]),
            CommandSpec::Argv(argv) => (
                argv.first().map(String::as_str).unwrap_or_default(),
                argv.iter().skip(1).map(String::as_str).collect(),
            ),
        }
    }

    pub fn display(&self) -> String {
        match self {
            CommandSpec::Shell(script) => script.clone(),
            CommandSpec::Argv(argv) => argv.join(" "),
        }
    }
}

/// When a long-running (`daemon`) worker is started again after it exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

/// What happens when a scheduled run comes due while the previous one is
/// still going.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Overlap {
    /// Let the running one finish and drop this run (counted as skipped).
    #[default]
    Skip,
    /// Stop the running one and start fresh.
    Replace,
}

fn yes() -> bool {
    true
}

/// One worker as written in a `Config/worker/*.json` file, where each file is
/// an object of `"name": { ...spec... }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerSpec {
    /// Free-form id (kept for older worker files).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub command: Option<CommandSpec>,
    /// `on_startup`, `daemon`, `manual`, `every 5m`, or a cron pattern
    /// (`*/5 * * * *`, `@hourly`, 6 fields for seconds).
    #[serde(alias = "execution_time", default = "manual")]
    pub schedule: String,
    /// With `on_startup`: keep it running, i.e. the same as `daemon`.
    #[serde(default)]
    pub repeat: bool,
    /// For `daemon` workers. Default `always`.
    pub restart: Option<RestartPolicy>,
    /// For every other schedule: how often a failed run is retried.
    #[serde(default)]
    pub retries: u32,
    /// Kill the run after this long (`30s`, `5m`, `1h30m`).
    pub timeout: Option<String>,
    /// Random delay of up to this long before each scheduled run, so workers
    /// that share a schedule don't all start in the same second.
    pub jitter: Option<String>,
    #[serde(default)]
    pub overlap: Overlap,
    /// Run once at daemon start when a scheduled run was missed while the
    /// daemon was down (like systemd's `Persistent=true`).
    #[serde(default)]
    pub catch_up: bool,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub max_restarts: Option<u32>,
    pub restart_window: Option<String>,
}

fn manual() -> String {
    "manual".into()
}

#[derive(Debug, Clone)]
pub enum Schedule {
    /// Once, when the daemon starts.
    Startup,
    /// Kept running; restarted per its [`RestartPolicy`].
    Daemon,
    /// Only when asked (`aria worker run`, or `r` in the TUI).
    Manual,
    Every(Duration),
    Cron(Box<Cron>),
}

impl Schedule {
    pub fn parse(text: &str, repeat: bool) -> Result<Self, String> {
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();
        match lower.as_str() {
            "on_startup" | "startup" | "@reboot" => {
                return Ok(if repeat { Schedule::Daemon } else { Schedule::Startup });
            }
            "daemon" | "always" | "service" => return Ok(Schedule::Daemon),
            "manual" | "" => return Ok(Schedule::Manual),
            _ => {}
        }
        if let Some(every) = lower.strip_prefix("every ").or_else(|| lower.strip_prefix("@every ")) {
            let every = parse_duration(every)?;
            // Run times are kept in whole seconds.
            if every < Duration::from_secs(1) {
                return Err(format!("'{trimmed}' is too often: the shortest interval is 1s"));
            }
            return Ok(Schedule::Every(every));
        }
        let pattern = trimmed.strip_prefix("cron:").unwrap_or(trimmed).trim();
        pattern
            .parse::<Cron>()
            .map(|cron| Schedule::Cron(Box::new(cron)))
            .map_err(|e| format!("bad schedule '{trimmed}': {e}"))
    }

    /// Runs on the clock (`every` or cron).
    pub fn is_timed(&self) -> bool {
        matches!(self, Schedule::Every(_) | Schedule::Cron(_))
    }
}

/// `500ms`, `30s`, `5m`, `1h30m`, `2d`; a bare number is seconds.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("bad duration '{text}' (try 30s, 5m, 1h30m)");
    let mut total = Duration::ZERO;
    let mut number = String::new();
    let mut chars = text.trim().chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        let mut unit = c.to_string();
        while let Some(&next) = chars.peek().filter(|n| n.is_ascii_alphabetic()) {
            unit.push(next);
            chars.next();
        }
        let n: u64 = number.parse().map_err(|_| bad())?;
        number.clear();
        total += match unit.as_str() {
            "ms" => Duration::from_millis(n),
            "s" | "sec" | "secs" => Duration::from_secs(n),
            "m" | "min" | "mins" => Duration::from_secs(n * 60),
            "h" | "hr" | "hrs" => Duration::from_secs(n * 3600),
            "d" | "day" | "days" => Duration::from_secs(n * 86_400),
            _ => return Err(bad()),
        };
    }
    if !number.is_empty() {
        total += Duration::from_secs(number.parse().map_err(|_| bad())?);
    }
    if total.is_zero() {
        return Err(bad());
    }
    Ok(total)
}

/// When a timed worker is due next. An overdue run (an interval that passed
/// while the daemon was down, or a missed cron slot with `catch_up`) is due
/// right away; it is run once, not once per missed slot.
pub fn next_fire(
    schedule: &Schedule,
    now: DateTime<Local>,
    last_start: Option<DateTime<Local>>,
    catch_up: bool,
) -> Option<DateTime<Local>> {
    match schedule {
        Schedule::Every(every) => {
            let every = chrono::Duration::from_std(*every).ok()?;
            Some(match last_start {
                Some(last) if last + every > now => last + every,
                _ => now,
            })
        }
        Schedule::Cron(cron) => {
            if catch_up
                && let Some(last) = last_start
                && cron.find_next_occurrence(&last, false).is_ok_and(|missed| missed <= now)
            {
                return Some(now);
            }
            cron.find_next_occurrence(&now, false).ok()
        }
        _ => None,
    }
}

/// The first slot strictly after `after` (used while a run is still going).
pub fn next_after(schedule: &Schedule, after: DateTime<Local>) -> Option<DateTime<Local>> {
    match schedule {
        Schedule::Every(every) => Some(after + chrono::Duration::from_std(*every).ok()?),
        Schedule::Cron(cron) => cron.find_next_occurrence(&after, false).ok(),
        _ => None,
    }
}

/// Exponential backoff with "equal jitter": the cap doubles per attempt (up
/// to `max`) and the delay is random in `[cap/2, cap]`. Half is fixed so a
/// crash loop always slows down; half is random so workers that failed
/// together don't retry together. `rand01` is a number in `[0, 1]`.
pub fn backoff_delay(attempt: u32, base: Duration, max: Duration, rand01: f64) -> Duration {
    let factor = 1u32.checked_shl(attempt.min(30)).unwrap_or(u32::MAX);
    let cap = base.saturating_mul(factor).min(max);
    cap / 2 + cap.mul_f64(rand01.clamp(0.0, 1.0)) / 2
}

/// Erlang/OTP-style restart intensity: allows at most `max` restarts in any
/// `window`. Past that the worker is broken, and restarting it again only
/// burns CPU, so the daemon marks it failed instead.
#[derive(Debug)]
pub struct RestartLimiter {
    max: u32,
    window: Duration,
    history: VecDeque<Instant>,
}

impl RestartLimiter {
    pub fn new(max: u32, window: Duration) -> Self {
        Self { max, window, history: VecDeque::new() }
    }

    /// Records a restart at `now` if one is allowed.
    pub fn allow(&mut self, now: Instant) -> bool {
        while self.history.front().is_some_and(|t| now.duration_since(*t) > self.window) {
            self.history.pop_front();
        }
        if self.history.len() >= self.max as usize {
            return false;
        }
        self.history.push_back(now);
        true
    }
}

/// A spec checked and turned into what the daemon runs.
#[derive(Debug, Clone)]
pub struct Worker {
    pub name: String,
    pub spec: WorkerSpec,
    pub command: CommandSpec,
    pub schedule: Schedule,
    pub timeout: Option<Duration>,
    pub jitter: Option<Duration>,
    pub restart: RestartPolicy,
    pub max_restarts: u32,
    pub restart_window: Duration,
    /// The `when:` condition and hooks of a worker written in AWL.
    pub logic: Option<Arc<awl::Logic>>,
}

impl Worker {
    pub fn compile(name: &str, spec: WorkerSpec) -> Result<Self, String> {
        let command = match &spec.command {
            None => return Err("no command".into()),
            Some(CommandSpec::Shell(s)) if s.trim().is_empty() => return Err("empty command".into()),
            Some(CommandSpec::Argv(argv)) if argv.first().is_none_or(|p| p.is_empty()) => {
                return Err("empty command".into());
            }
            Some(command) => command.clone(),
        };
        let duration = |field: &Option<String>| field.as_deref().map(parse_duration).transpose();
        Ok(Self {
            name: name.to_owned(),
            command,
            schedule: Schedule::parse(&spec.schedule, spec.repeat)?,
            timeout: duration(&spec.timeout)?,
            jitter: duration(&spec.jitter)?,
            restart: spec.restart.unwrap_or(RestartPolicy::Always),
            max_restarts: spec.max_restarts.unwrap_or(DEFAULT_MAX_RESTARTS),
            restart_window: duration(&spec.restart_window)?.unwrap_or(DEFAULT_RESTART_WINDOW),
            logic: None,
            spec,
        })
    }
}

/// One entry found in the worker directory, valid or not.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The worker's name (for a broken file, the file name).
    pub name: String,
    pub source: PathBuf,
    pub raw: Value,
    pub worker: Result<Worker, String>,
}

/// Is this a worker file (`*.json`, or `*.awl` written in AWL)?
pub fn is_worker_file(path: &Path) -> bool {
    path.extension().is_some_and(|x| x == "json" || x == "awl")
}

/// A worker read from a file: its name, its raw definition, and the worker
/// (or why it is invalid).
type Loaded = (String, Value, Result<Worker, String>);

/// Reads one `.awl` file: its workers, or the reason it doesn't compile.
fn load_awl(file: &Path, file_name: &str) -> Result<Vec<Loaded>, String> {
    let src = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let compiled = awl::compile(&src, file_name).map_err(|diags| awl::short_error(&diags, file_name, &src))?;
    Ok(compiled
        .into_iter()
        .map(|c| {
            let worker = Worker::compile(&c.name, c.spec).map(|w| Worker { logic: c.logic, ..w });
            (c.name, c.raw, worker)
        })
        .collect())
}

/// Reads every worker file in `dir` (`*.json` and `*.awl`), in name order.
/// A broken file or worker becomes an entry with an error; it never stops
/// the others loading. Names must be unique across all files.
pub fn load_dir(dir: &Path) -> Vec<Entry> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| is_worker_file(p)).collect())
        .unwrap_or_default();
    files.sort();

    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for file in files {
        let file_name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let broken = |error: String| Entry {
            name: file_name.clone(),
            source: file.clone(),
            raw: Value::Null,
            worker: Err(error),
        };
        let found = if file.extension().is_some_and(|x| x == "awl") {
            load_awl(&file, &file_name)
        } else {
            match read_json(&file) {
                Ok(Value::Object(map)) => Ok(map
                    .into_iter()
                    .map(|(name, raw)| {
                        let worker = serde_json::from_value::<WorkerSpec>(raw.clone())
                            .map_err(|e| e.to_string())
                            .and_then(|spec| Worker::compile(&name, spec));
                        (name, raw, worker)
                    })
                    .collect()),
                Ok(_) => Err("not a JSON object of workers".into()),
                Err(e) => Err(e.to_string()),
            }
        };
        match found {
            Ok(workers) => {
                for (name, raw, worker) in workers {
                    let (name, worker) = if seen.insert(name.clone()) {
                        (name, worker)
                    } else {
                        (format!("{name}@{file_name}"), Err(format!("duplicate worker name '{name}'")))
                    };
                    entries.push(Entry { name, source: file.clone(), raw, worker });
                }
            }
            Err(error) => entries.push(broken(error)),
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 16, h, m, s).unwrap()
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("5 m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        for bad in ["", "0s", "5x", "m"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn schedules_parse() {
        assert!(matches!(Schedule::parse("on_startup", false), Ok(Schedule::Startup)));
        assert!(matches!(Schedule::parse("on_startup", true), Ok(Schedule::Daemon)));
        assert!(matches!(Schedule::parse("daemon", false), Ok(Schedule::Daemon)));
        assert!(matches!(Schedule::parse("every 5m", false), Ok(Schedule::Every(d)) if d.as_secs() == 300));
        assert!(matches!(Schedule::parse("*/5 * * * *", false), Ok(Schedule::Cron(_))));
        assert!(matches!(Schedule::parse("@hourly", false), Ok(Schedule::Cron(_))));
        assert!(Schedule::parse("whenever", false).is_err());
        assert!(Schedule::parse("every 250ms", false).is_err());
    }

    #[test]
    fn interval_runs_now_when_new_or_overdue() {
        let every = Schedule::Every(Duration::from_secs(300));
        let now = at(12, 0, 0);
        assert_eq!(next_fire(&every, now, None, false), Some(now));
        assert_eq!(next_fire(&every, now, Some(at(11, 58, 0)), false), Some(at(12, 3, 0)));
        assert_eq!(next_fire(&every, now, Some(at(10, 0, 0)), false), Some(now));
    }

    #[test]
    fn cron_next_and_catch_up() {
        let cron = Schedule::parse("0 * * * *", false).unwrap();
        let now = at(12, 20, 0);
        assert_eq!(next_fire(&cron, now, None, false), Some(at(13, 0, 0)));
        // Last ran at 10:00, so 11:00 and 12:00 were missed.
        assert_eq!(next_fire(&cron, now, Some(at(10, 0, 0)), false), Some(at(13, 0, 0)));
        assert_eq!(next_fire(&cron, now, Some(at(10, 0, 0)), true), Some(now));
        // Ran at 12:00: nothing missed.
        assert_eq!(next_fire(&cron, now, Some(at(12, 0, 0)), true), Some(at(13, 0, 0)));
        assert_eq!(next_after(&cron, at(13, 0, 0)), Some(at(14, 0, 0)));
    }

    #[test]
    fn backoff_grows_caps_and_jitters() {
        let d = |attempt, r| backoff_delay(attempt, BACKOFF_BASE, BACKOFF_MAX, r);
        assert_eq!(d(0, 0.0), Duration::from_millis(500));
        assert_eq!(d(0, 1.0), Duration::from_secs(1));
        assert_eq!(d(3, 1.0), Duration::from_secs(8));
        assert_eq!(d(3, 0.0), Duration::from_secs(4));
        assert_eq!(d(40, 1.0), BACKOFF_MAX);
        assert_eq!(d(40, 0.0), BACKOFF_MAX / 2);
    }

    #[test]
    fn restart_intensity() {
        let mut limiter = RestartLimiter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(limiter.allow(t0));
        assert!(limiter.allow(t0 + Duration::from_secs(1)));
        assert!(limiter.allow(t0 + Duration::from_secs(2)));
        assert!(!limiter.allow(t0 + Duration::from_secs(3)));
        // The first restart leaves the window.
        assert!(limiter.allow(t0 + Duration::from_secs(61)));
    }

    #[test]
    fn compile_checks_the_spec() {
        let spec = |v| serde_json::from_value::<WorkerSpec>(v).unwrap();
        let w = Worker::compile("a", spec(json!({"command": ["echo", "hi"], "schedule": "every 1m", "timeout": "10s"})))
            .unwrap();
        assert_eq!(w.command.program(), ("echo", vec!["hi"]));
        assert_eq!(w.timeout, Some(Duration::from_secs(10)));
        assert_eq!(w.restart, RestartPolicy::Always);
        assert!(Worker::compile("b", spec(json!({"schedule": "daemon"}))).is_err());
        assert!(Worker::compile("c", spec(json!({"command": "x", "schedule": "nope"}))).is_err());
        assert!(serde_json::from_value::<WorkerSpec>(json!({"command": "x", "typo": 1})).is_err());
        // Old field name still works.
        assert_eq!(spec(json!({"command": "x", "execution_time": "on_startup"})).schedule, "on_startup");
    }

    #[test]
    fn broken_files_and_duplicates_do_not_stop_loading() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.json"), r#"{"ok": {"command": "true"}, "bad": {"schedule": "daemon"}}"#).unwrap();
        fs::write(dir.path().join("b.json"), r#"{"ok": {"command": "true"}}"#).unwrap();
        fs::write(dir.path().join("c.json"), "{ not json").unwrap();
        fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        let entries = load_dir(dir.path());
        let summary: Vec<(String, bool)> = entries.iter().map(|e| (e.name.clone(), e.worker.is_ok())).collect();
        assert_eq!(
            summary,
            [
                ("ok".to_string(), true),
                ("bad".to_string(), false),
                ("ok@b.json".to_string(), false),
                ("c.json".to_string(), false)
            ]
        );
    }
}
