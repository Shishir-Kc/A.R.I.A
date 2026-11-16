//! AWL, the A.R.I.A Worker Language: Python-shaped syntax with Rust-style
//! static types, for writing workers as `*.awl` files.
//!
//! ```text
//! worker backup:
//!     run = ["restic", "backup", "~/docs"]
//!     schedule = cron("0 3 * * *")
//!     timeout = 2h
//!     when: on_ac() and disk_free("/") > 10
//!     on_failure:
//!         notify("backup failed", f"exit {status.exit}")
//! ```
//!
//! A file goes through [`lexer`] → [`parser`] → [`check`] (the static type
//! checker, which reports every error with a rustc-style caret) and is then
//! lowered here into the same [`WorkerSpec`] a JSON worker produces, so
//! scheduling and supervision stay in one place. What JSON can't express —
//! a `when:` condition and the `on_success:` / `on_failure:` hooks — is kept
//! as [`Logic`] and run by [`eval`], which never blocks: side effects come
//! back as [`Action`]s for the daemon to carry out.
//!
//! See `docs/awl.md` for the language reference.

// Diagnostics are big, but they are only built on the (cold) error path.
#![allow(clippy::result_large_err)]

pub mod ast;
pub mod check;
pub mod diag;
pub mod eval;
pub mod host;
pub mod lexer;
pub mod parser;
pub mod types;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use serde_json::{Map, Value as Json, json};

use ast::{Block, Expr, FnDecl, Item, Member};
use diag::Diagnostic;
pub use eval::{Action, StatusValue};
use eval::{Interp, RuntimeError, ScheduleValue, Value};
use host::Host;

use super::spec::{Schedule, WorkerSpec};

/// What every worker of one file shares: its functions and constants.
#[derive(Debug)]
pub struct Module {
    pub file: String,
    pub src: String,
    fns: HashMap<String, FnDecl>,
    globals: HashMap<String, Value>,
}

impl Module {
    fn runtime_error(&self, e: RuntimeError) -> String {
        let d = Diagnostic::error("E0080", "runtime error", e.span);
        let (line, col) = d.line_col(&self.src);
        format!("{}:{line}:{col}: runtime error: {}", self.file, e.message)
    }
}

/// The parts of a worker that run every time: its condition and hooks.
#[derive(Debug)]
pub struct Logic {
    module: Arc<Module>,
    when: Option<Block>,
    on_success: Option<Block>,
    on_failure: Option<Block>,
}

impl Logic {
    pub fn has_condition(&self) -> bool {
        self.when.is_some()
    }

    /// Evaluates `when:`. No condition means yes.
    pub fn should_run(&self, host: &dyn Host, status: StatusValue) -> Result<bool, String> {
        let Some(body) = &self.when else { return Ok(true) };
        let m = &self.module;
        let mut interp = Interp::new(&m.fns, &m.globals, Some(host));
        match interp.run_hook(body, status) {
            Ok(v) => Ok(v == Value::Bool(true)),
            Err(e) => Err(m.runtime_error(e)),
        }
    }

    /// Runs `on_success:` or `on_failure:`. Returns the actions it asked for
    /// and, if it stopped early, the runtime error (actions before the error
    /// are still returned).
    pub fn react(&self, success: bool, host: &dyn Host, status: StatusValue) -> (Vec<Action>, Option<String>) {
        let hook = if success { &self.on_success } else { &self.on_failure };
        let Some(body) = hook else { return (Vec::new(), None) };
        let m = &self.module;
        let mut interp = Interp::new(&m.fns, &m.globals, Some(host));
        let result = interp.run_hook(body, status);
        (interp.actions, result.err().map(|e| m.runtime_error(e)))
    }
}

/// One worker of a compiled file.
#[derive(Debug)]
pub struct CompiledWorker {
    pub name: String,
    pub spec: WorkerSpec,
    /// What `raw` holds for a JSON worker: the spec, plus a fingerprint of
    /// the file, so editing a hook (or a function it calls) counts as a change.
    pub raw: Json,
    /// `None` when the worker has no condition and no hooks.
    pub logic: Option<Arc<Logic>>,
}

/// Compiles one `.awl` file. On failure, every error found, in order.
pub fn compile(src: &str, file: &str) -> Result<Vec<CompiledWorker>, Vec<Diagnostic>> {
    let tokens = lexer::lex(src).map_err(|d| vec![d])?;
    let program = parser::parse(tokens).map_err(|d| vec![d])?;
    let diags = check::check(&program);
    if !diags.is_empty() {
        return Err(diags);
    }

    // Constants first, in order (the checker made sure they're pure).
    let mut fns = HashMap::new();
    let mut globals = HashMap::new();
    let mut errors = Vec::new();
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                fns.insert(f.name.name.clone(), f.clone());
            }
            Item::Let(l) => {
                let value = Interp::new(&fns, &globals, None).eval(&l.value);
                match value {
                    Ok(v) => {
                        globals.insert(l.name.name.clone(), v);
                    }
                    Err(e) => errors.push(Diagnostic::error("E0080", e.message, e.span).label("evaluating this constant failed")),
                }
            }
            Item::Worker(_) => {}
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    // What every worker depends on: the constants and functions. A worker's
    // fingerprint is this plus its own text, so editing one worker (or a
    // comment between workers) doesn't restart the others.
    let shared_text: String = program
        .items
        .iter()
        .zip(&program.extents)
        .filter(|(item, _)| !matches!(item, Item::Worker(_)))
        .map(|(_, (a, b))| &src[*a..*b])
        .collect();
    let fingerprint = |own: &str| {
        let mut hasher = DefaultHasher::new();
        (&shared_text, own).hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };
    let module = Arc::new(Module { file: file.to_owned(), src: src.to_owned(), fns, globals });

    let mut workers = Vec::new();
    for (item, (start, end)) in program.items.iter().zip(&program.extents) {
        let Item::Worker(w) = item else { continue };
        let fingerprint = fingerprint(&src[*start..*end]);
        let mut spec = Map::new();
        let (mut when, mut on_success, mut on_failure) = (None, None, None);
        for member in &w.members {
            match member {
                Member::Field { name, value } => match lower_field(&module, &name.name, value) {
                    Ok((key, json)) => {
                        spec.insert(key.to_owned(), json);
                    }
                    Err(d) => errors.push(d),
                },
                Member::Hook { name, body } => match name.name.as_str() {
                    "when" => when = Some(body.clone()),
                    "on_success" => on_success = Some(body.clone()),
                    _ => on_failure = Some(body.clone()),
                },
            }
        }
        let spec_json = Json::Object(spec);
        let spec: WorkerSpec = match serde_json::from_value(spec_json.clone()) {
            Ok(spec) => spec,
            Err(e) => {
                errors.push(Diagnostic::error("E0080", format!("invalid worker: {e}"), w.name.span));
                continue;
            }
        };
        let hooks: Vec<&str> = [("when", &when), ("on_success", &on_success), ("on_failure", &on_failure)]
            .into_iter()
            .filter(|(_, b)| b.is_some())
            .map(|(n, _)| n)
            .collect();
        let mut raw = spec_json;
        raw["awl"] = json!({ "file": file, "fingerprint": fingerprint, "hooks": hooks });
        let logic = (!hooks.is_empty())
            .then(|| Arc::new(Logic { module: module.clone(), when, on_success, on_failure }));
        workers.push(CompiledWorker { name: w.name.name.clone(), spec, raw, logic });
    }
    if errors.is_empty() { Ok(workers) } else { Err(errors) }
}

/// Evaluates one field and turns it into its JSON spelling.
fn lower_field(module: &Module, name: &str, expr: &Expr) -> Result<(&'static str, Json), Diagnostic> {
    let value = Interp::new(&module.fns, &module.globals, None)
        .eval(expr)
        .map_err(|e| Diagnostic::error("E0080", e.message, e.span).label("evaluating this field failed"))?;
    let bad = |message: String, label: &str| Diagnostic::error("E0080", message, expr.span).label(label.to_owned());
    let positive_ms = |ms: i64| -> Result<Json, Diagnostic> {
        if ms <= 0 {
            return Err(bad(format!("`{name}` must be longer than zero"), "zero or negative duration"));
        }
        Ok(json!(eval::format_duration(ms)))
    };
    let count = |n: i64| -> Result<Json, Diagnostic> {
        u32::try_from(n).map(|n| json!(n)).map_err(|_| bad(format!("`{name}` must be between 0 and {}", u32::MAX), "out of range"))
    };
    Ok(match (name, value) {
        ("run", Value::Str(s)) => {
            if s.trim().is_empty() {
                return Err(bad("the command is empty".into(), "nothing to run"));
            }
            ("command", json!(s))
        }
        ("run", Value::List(items)) => {
            let argv: Vec<String> = items.into_iter().map(|v| v.to_string()).collect();
            if argv.first().is_none_or(|p| p.is_empty()) {
                return Err(bad("the command is empty".into(), "the first item must name a program"));
            }
            ("command", json!(argv))
        }
        ("schedule", Value::Schedule(s)) => {
            let text = match s {
                ScheduleValue::Daemon => "daemon".to_owned(),
                ScheduleValue::Startup => "on_startup".to_owned(),
                ScheduleValue::Manual => "manual".to_owned(),
                ScheduleValue::Every(ms) => {
                    if ms < 1000 {
                        return Err(bad("the shortest interval is 1s".into(), "too often"));
                    }
                    format!("every {}", eval::format_duration(ms))
                }
                ScheduleValue::Cron(pattern) => {
                    if let Err(e) = Schedule::parse(&pattern, false) {
                        return Err(bad(e, "invalid cron pattern")
                            .help("cron has 5 fields (minute hour day month weekday), 6 with seconds first, or `@hourly`, `@daily`..."));
                    }
                    pattern
                }
            };
            ("schedule", json!(text))
        }
        ("description", Value::Str(s)) => ("description", json!(s)),
        ("cwd", Value::Str(s)) => ("cwd", json!(s)),
        ("timeout", Value::Duration(ms)) => ("timeout", positive_ms(ms)?),
        ("jitter", Value::Duration(ms)) => ("jitter", positive_ms(ms)?),
        ("restart_window", Value::Duration(ms)) => ("restart_window", positive_ms(ms)?),
        ("retries", Value::Int(n)) => ("retries", count(n)?),
        ("max_restarts", Value::Int(n)) => ("max_restarts", count(n)?),
        ("restart", Value::Restart(v)) => ("restart", json!(v.replace('_', "-"))),
        ("overlap", Value::Overlap(v)) => ("overlap", json!(v)),
        ("catch_up", Value::Bool(b)) => ("catch_up", json!(b)),
        ("enabled", Value::Bool(b)) => ("enabled", json!(b)),
        ("env", Value::Map(m)) => {
            let env: Map<String, Json> = m.into_iter().map(|(k, v)| (k, json!(v.to_string()))).collect();
            ("env", Json::Object(env))
        }
        (other, v) => return Err(bad(format!("`{other}` can't be set to {v}"), "unexpected value")),
    })
}

/// Checks one file and renders every problem the way `rustc` would.
/// `Ok` carries the names of the workers it defines.
pub fn check_source(src: &str, file: &str) -> Result<Vec<String>, String> {
    match compile(src, file) {
        Ok(workers) => Ok(workers.into_iter().map(|w| w.name).collect()),
        Err(diags) => Err(diag::render_all(&diags, file, src)),
    }
}

/// A one-line summary of a failed compile, for worker lists.
pub fn short_error(diags: &[Diagnostic], file: &str, src: &str) -> String {
    let first = diags.first().map(|d| d.short(file, src)).unwrap_or_default();
    match diags.len() {
        0 | 1 => first,
        n => format!("{first} (and {} more; run `aria worker check`)", n - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::awl::host::Host;
    use chrono::{DateTime, Local, TimeZone};

    struct Fake {
        hour: u32,
        battery: i64,
        ac: bool,
    }

    impl Host for Fake {
        fn now(&self) -> DateTime<Local> {
            Local.with_ymd_and_hms(2026, 11, 16, self.hour, 0, 0).unwrap()
        }
        fn env(&self, name: &str) -> Option<String> {
            (name == "USER").then(|| "aria".into())
        }
        fn path_exists(&self, path: &str) -> bool {
            path == "/flag"
        }
        fn read_file(&self, _: &str) -> Option<String> {
            None
        }
        fn battery(&self) -> i64 {
            self.battery
        }
        fn on_ac(&self) -> bool {
            self.ac
        }
        fn load(&self) -> f64 {
            0.5
        }
        fn cpus(&self) -> i64 {
            4
        }
        fn mem_used(&self) -> i64 {
            40
        }
        fn disk_free(&self, _: &str) -> i64 {
            55
        }
        fn uptime(&self) -> i64 {
            3600
        }
    }

    const SRC: &str = r#"
# Shared by every worker below.
let min_battery = 30
let quiet_hours = [23, 0, 1, 2, 3, 4, 5, 6]

fn plugged_or_charged() -> bool:
    return on_ac() or battery() >= min_battery

fn verdict(code: int) -> str:
    if code == 0:
        return "ok"
    return f"exit {code}"

worker disk_report:
    description = "Root filesystem usage, hourly"
    run = ["df", "-h", "/"]
    schedule = cron("0 * * * *")
    timeout = 30s
    retries = 2
    catch_up = true

worker sync:
    run = "rsync -a ~/docs /mnt/backup"
    schedule = every(15m)
    jitter = 1m
    restart_window = 2 * 1m
    env = {"LANG": "C"}
    when:
        if hour() in quiet_hours:
            return false
        return plugged_or_charged()
    on_failure:
        notify("sync failed", verdict(status.exit))
        if status.failures >= 3:
            log(f"giving up after {status.failures} failures")
            pause()
    on_success: trigger("disk_report")
"#;

    fn status(exit: i64, failures: i64) -> StatusValue {
        StatusValue { name: "sync".into(), exit, failures, success: exit == 0, ..Default::default() }
    }

    #[test]
    fn editing_one_worker_leaves_the_others_alone() {
        let raw = |src: &str| -> Vec<Json> { compile(src, "t.awl").unwrap().into_iter().map(|w| w.raw).collect() };
        let before = raw(SRC);
        let comment = raw(&SRC.replace("worker sync:", "# a note\nworker sync:"));
        assert_eq!(before, comment);
        let edited = raw(&SRC.replace("giving up", "stopping"));
        assert_eq!(before[0], edited[0]);
        assert_ne!(before[1], edited[1]);
        // A shared function counts for every worker.
        let shared = raw(&SRC.replace("return \"ok\"", "return \"fine\""));
        assert_ne!(before[0], shared[0]);
    }

    /// Whatever the input, compiling returns; it never panics.
    #[test]
    fn garbage_never_panics() {
        let mut inputs: Vec<String> = Vec::new();
        for text in [SRC, crate::worker::EXAMPLE_AWL] {
            let mut cut = 0;
            while cut <= text.len() {
                if text.is_char_boundary(cut) {
                    inputs.push(text[..cut].to_owned());
                }
                cut += 1;
            }
        }
        inputs.extend(
            [
                "let x = f\"{#}\"", "f\"{", "let x = f\"{)}\"", "worker", "worker w:", "worker w:\n", "fn f(", "let = 1",
                "worker w:\n    run = \"x\"\n    when:\n", "\u{0}", "let x = ((((((", "))))", "let x = 99999999999999999999",
                "let x = 5zz", "\t\n  \n\t", "let x = [1,,2]", "let x = {1: 2}", "worker w:\n  run = \"x\"\n    y = 1\n",
            ]
            .map(String::from),
        );
        inputs.push(format!("let x = {}1{}", "(".repeat(5000), ")".repeat(5000)));
        inputs.push(format!("let x = {}1", "-".repeat(5000)));
        inputs.push(format!("let x = {}true", "not ".repeat(5000)));
        let mut nested = String::from("fn f():\n");
        for depth in 1..300 {
            nested.push_str(&format!("{}if true:\n", "  ".repeat(depth)));
        }
        inputs.push(nested);
        for input in inputs {
            let _ = compile(&input, "fuzz.awl");
        }
    }

    #[test]
    fn lowers_to_the_same_spec_as_json() {
        let workers = compile(SRC, "t.awl").unwrap();
        assert_eq!(workers.len(), 2);
        let disk = &workers[0];
        assert!(disk.logic.is_none());
        let json: WorkerSpec = serde_json::from_value(serde_json::json!({
            "description": "Root filesystem usage, hourly",
            "command": ["df", "-h", "/"],
            "schedule": "0 * * * *",
            "timeout": "30s",
            "retries": 2,
            "catch_up": true
        }))
        .unwrap();
        assert_eq!(disk.spec, json);
        let w = crate::worker::spec::Worker::compile(&disk.name, disk.spec.clone()).unwrap();
        assert_eq!(w.timeout, Some(std::time::Duration::from_secs(30)));

        let sync = crate::worker::spec::Worker::compile("sync", workers[1].spec.clone()).unwrap();
        assert!(matches!(sync.schedule, Schedule::Every(d) if d.as_secs() == 900));
        assert_eq!(sync.restart_window.as_secs(), 120);
        assert_eq!(sync.spec.env["LANG"], "C");
        assert_eq!(workers[1].raw["awl"]["hooks"], serde_json::json!(["when", "on_success", "on_failure"]));
    }

    #[test]
    fn conditions_read_live_state() {
        let workers = compile(SRC, "t.awl").unwrap();
        let logic = workers[1].logic.as_ref().unwrap();
        let run = |hour, battery, ac| logic.should_run(&Fake { hour, battery, ac }, status(0, 0)).unwrap();
        assert!(run(12, 80, false));
        assert!(run(12, 10, true));
        assert!(!run(12, 10, false));
        assert!(!run(23, 100, true), "quiet hours");
    }

    #[test]
    fn hooks_return_actions() {
        let workers = compile(SRC, "t.awl").unwrap();
        let logic = workers[1].logic.as_ref().unwrap();
        let host = Fake { hour: 12, battery: 80, ac: true };
        assert_eq!(logic.react(true, &host, status(0, 0)), (vec![Action::Trigger("disk_report".into())], None));
        let (actions, error) = logic.react(false, &host, status(2, 3));
        assert_eq!(error, None);
        assert_eq!(
            actions,
            [
                Action::Notify { title: "sync failed".into(), body: "exit 2".into() },
                Action::Log("giving up after 3 failures".into()),
                Action::Pause
            ]
        );
    }

    #[test]
    fn runtime_errors_point_at_the_source() {
        let src = "worker w:\n    run = \"x\"\n    when:\n        let xs = [1]\n        return xs[3] > 0\n";
        let workers = compile(src, "w.awl").unwrap();
        let logic = workers[0].logic.as_ref().unwrap();
        let error = logic.should_run(&Fake { hour: 1, battery: 1, ac: true }, status(0, 0)).unwrap_err();
        assert!(error.starts_with("w.awl:5:19: runtime error: index 3 is out of range"), "{error}");
    }

    #[test]
    fn lowering_errors() {
        let code = |src: &str| compile(src, "t.awl").unwrap_err()[0].message.clone();
        assert!(code("worker w:\n    run = \"x\"\n    schedule = every(500ms)\n").contains("shortest interval"));
        assert!(code("worker w:\n    run = \"x\"\n    schedule = cron(\"61 * * * *\")\n").contains("bad schedule"));
        assert!(code("worker w:\n    run = \"x\"\n    timeout = 0s\n").contains("longer than zero"));
        assert!(code("worker w:\n    run = \"x\"\n    retries = -1\n").contains("between 0"));
        assert!(code("worker w:\n    run = []\n").contains("empty"));
        assert!(code("let xs = [1]\nlet y = xs[5]\nworker w:\n    run = \"x\"\n").contains("out of range"));
    }

    #[test]
    fn many_errors_at_once() {
        let src = "worker w:\n    run = 5\n    timeout = \"1m\"\n    retries = true\n";
        let diags = compile(src, "t.awl").unwrap_err();
        assert_eq!(diags.len(), 3);
        let report = diag::render_all(&diags, "t.awl", src);
        assert!(report.contains("due to 3 previous errors"), "{report}");
        assert!(short_error(&diags, "t.awl", src).contains("and 2 more"));
    }

    #[test]
    fn runaway_loops_are_stopped() {
        let src = "worker w:\n    run = \"x\"\n    when:\n        let mut n = 0\n        for a in range(100000):\n            for b in range(100000):\n                n = n + 1\n        return n > 0\n";
        let workers = compile(src, "t.awl").unwrap();
        let error = workers[0].logic.as_ref().unwrap().should_run(&Fake { hour: 1, battery: 1, ac: true }, status(0, 0)).unwrap_err();
        assert!(error.contains("evaluation stopped"), "{error}");
    }
}
