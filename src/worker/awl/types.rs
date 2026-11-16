//! The type system and the signatures of the built-in functions.

use std::fmt;

/// A static type. `Unknown` stands for "already reported" (or the element
/// type of `[]`) and is compatible with everything, so one mistake gives
/// one error instead of a cascade.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Duration,
    List(Box<Ty>),
    /// Keys are always `str`.
    Map(Box<Ty>),
    Schedule,
    Restart,
    Overlap,
    Status,
    Unit,
    Unknown,
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Ty::Int => write!(f, "int"),
            Ty::Float => write!(f, "float"),
            Ty::Bool => write!(f, "bool"),
            Ty::Str => write!(f, "str"),
            Ty::Duration => write!(f, "duration"),
            Ty::List(t) => write!(f, "list[{t}]"),
            Ty::Map(t) => write!(f, "map[str, {t}]"),
            Ty::Schedule => write!(f, "Schedule"),
            Ty::Restart => write!(f, "Restart"),
            Ty::Overlap => write!(f, "Overlap"),
            Ty::Status => write!(f, "Status"),
            Ty::Unit => write!(f, "()"),
            Ty::Unknown => write!(f, "_"),
        }
    }
}

impl Ty {
    /// `found` can be used where `self` is expected. No implicit
    /// conversions: an `int` is not a `float`.
    pub fn accepts(&self, found: &Ty) -> bool {
        match (self, found) {
            (Ty::Unknown, _) | (_, Ty::Unknown) => true,
            (Ty::List(a), Ty::List(b)) | (Ty::Map(a), Ty::Map(b)) => a.accepts(b),
            (a, b) => a == b,
        }
    }

    /// The more precise of two compatible types (`list[int]` over `list[_]`).
    pub fn join(&self, other: &Ty) -> Ty {
        match (self, other) {
            (Ty::Unknown, t) | (t, Ty::Unknown) => t.clone(),
            (Ty::List(a), Ty::List(b)) => Ty::List(Box::new(a.join(b))),
            (Ty::Map(a), Ty::Map(b)) => Ty::Map(Box::new(a.join(b))),
            (a, _) => a.clone(),
        }
    }

    pub fn is_known(&self) -> bool {
        match self {
            Ty::Unknown => false,
            Ty::List(t) | Ty::Map(t) => t.is_known(),
            _ => true,
        }
    }

    /// Can be shown in an f-string or passed to `str()`.
    pub fn is_displayable(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Duration | Ty::Unknown)
    }
}

/// What calling something may do. The checker allows `Probe` only where a
/// value is computed per run (`when:` and hooks), and `Action` only in the
/// `on_success:` / `on_failure:` hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    /// Depends only on its arguments.
    Pure,
    /// Reads live state: the clock, the environment, files, battery, load.
    Probe,
    /// Does something: logs, notifies, triggers or pauses a worker.
    Action,
}

/// The fields of the `status` value available in `when:` and hooks.
pub const STATUS_FIELDS: &[(&str, Ty, &str)] = &[
    ("name", Ty::Str, "the worker's name"),
    ("runs", Ty::Int, "how many runs have started, ever"),
    ("failures", Ty::Int, "how many runs failed, ever"),
    ("exit", Ty::Int, "exit code of this run (in `when:`, of the last run; -1 if it never ran)"),
    ("success", Ty::Bool, "this run (in `when:`, the last run) exited with 0"),
    ("duration", Ty::Duration, "how long this run (in `when:`, the last run) took"),
    ("retry", Ty::Int, "retries used so far for this scheduled run"),
];

pub fn status_field(name: &str) -> Option<Ty> {
    STATUS_FIELDS.iter().find(|(n, _, _)| *n == name).map(|(_, t, _)| t.clone())
}

/// Variants of the enum-like namespaces, written `Restart.on_failure`.
pub fn enum_variant(ns: &str, variant: &str) -> Option<Ty> {
    let (ty, variants): (Ty, &[&str]) = match ns {
        "Restart" => (Ty::Restart, &["always", "on_failure", "never"]),
        "Overlap" => (Ty::Overlap, &["skip", "replace"]),
        "Schedule" => (Ty::Schedule, &["daemon", "startup", "manual"]),
        _ => return None,
    };
    variants.contains(&variant).then_some(ty)
}

pub fn enum_variants(ns: &str) -> &'static [&'static str] {
    match ns {
        "Restart" => &["always", "on_failure", "never"],
        "Overlap" => &["skip", "replace"],
        "Schedule" => &["daemon", "startup", "manual"],
        _ => &[],
    }
}

/// The result of looking up a built-in for some argument types.
pub enum Sig {
    /// Not a built-in.
    Unknown,
    Ok(Ty, Effect),
    /// A built-in, but these arguments don't fit; the message says why.
    BadArgs(String),
}

/// Every built-in, with a one-line signature (for errors and the docs).
pub const BUILTINS: &[(&str, &str)] = &[
    ("str", "str(x: int | float | bool | str | duration) -> str"),
    ("int", "int(x: float | str) -> int"),
    ("float", "float(x: int | str) -> float"),
    ("len", "len(x: str | list[T] | map[str, T]) -> int"),
    ("range", "range(end: int) -> list[int]  /  range(start: int, end: int) -> list[int]"),
    ("min", "min(a: T, b: T) -> T   (T is int, float or duration)"),
    ("max", "max(a: T, b: T) -> T   (T is int, float or duration)"),
    ("abs", "abs(x: int | float) -> int | float"),
    ("upper", "upper(s: str) -> str"),
    ("lower", "lower(s: str) -> str"),
    ("strip", "strip(s: str) -> str"),
    ("starts_with", "starts_with(s: str, prefix: str) -> bool"),
    ("ends_with", "ends_with(s: str, suffix: str) -> bool"),
    ("contains", "contains(s: str, part: str) -> bool"),
    ("replace", "replace(s: str, from: str, to: str) -> str"),
    ("split", "split(s: str, sep: str) -> list[str]"),
    ("join", "join(parts: list[str], sep: str) -> str"),
    ("keys", "keys(m: map[str, T]) -> list[str]"),
    ("get", "get(m: map[str, T], key: str, default: T) -> T"),
    ("secs", "secs(d: duration) -> int"),
    ("millis", "millis(d: duration) -> int"),
    ("every", "every(interval: duration) -> Schedule"),
    ("cron", "cron(pattern: str) -> Schedule"),
    ("env", "env(name: str) -> str   (\"\" when unset)"),
    ("has_env", "has_env(name: str) -> bool"),
    ("file_exists", "file_exists(path: str) -> bool"),
    ("read_file", "read_file(path: str) -> str   (first 64 KiB; \"\" if unreadable)"),
    ("now", "now() -> int   (Unix seconds)"),
    ("hour", "hour() -> int"),
    ("minute", "minute() -> int"),
    ("weekday", "weekday() -> int   (0 = Monday)"),
    ("day", "day() -> int   (day of the month)"),
    ("month", "month() -> int"),
    ("today", "today() -> str   (YYYY-MM-DD)"),
    ("battery", "battery() -> int   (percent; -1 without a battery)"),
    ("on_ac", "on_ac() -> bool"),
    ("load", "load() -> float   (1-minute load average)"),
    ("cpus", "cpus() -> int"),
    ("mem_used", "mem_used() -> int   (percent)"),
    ("disk_free", "disk_free(path: str) -> int   (percent)"),
    ("uptime", "uptime() -> duration"),
    ("log", "log(message: str)"),
    ("notify", "notify(title: str, body: str)"),
    ("trigger", "trigger(worker: str)"),
    ("pause", "pause()"),
];

pub fn signature_of(name: &str) -> Option<&'static str> {
    BUILTINS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

pub fn builtin(name: &str, args: &[Ty]) -> Sig {
    use Effect::*;
    use Ty::*;
    let Some(sig) = signature_of(name) else { return Sig::Unknown };
    let bad = || Sig::BadArgs(format!("expected `{sig}`"));
    let fixed = |params: &[Ty], ret: Ty, effect: Effect| {
        if params.len() == args.len() && params.iter().zip(args).all(|(p, a)| p.accepts(a)) {
            Sig::Ok(ret, effect)
        } else {
            Sig::BadArgs(format!("expected `{sig}`"))
        }
    };
    let numeric = |t: &Ty| matches!(t, Int | Float | Duration | Unknown);
    match name {
        "str" => match args {
            [t] if t.is_displayable() => Sig::Ok(Str, Pure),
            _ => bad(),
        },
        "int" => match args {
            [Float | Str | Unknown] => Sig::Ok(Int, Pure),
            _ => bad(),
        },
        "float" => match args {
            [Int | Str | Unknown] => Sig::Ok(Float, Pure),
            _ => bad(),
        },
        "len" => match args {
            [Str | List(_) | Map(_) | Unknown] => Sig::Ok(Int, Pure),
            _ => bad(),
        },
        "range" => fixed(&vec![Int; args.len().clamp(1, 2)], List(Box::new(Int)), Pure),
        "min" | "max" => match args {
            [a, b] if numeric(a) && a.accepts(b) => Sig::Ok(a.join(b), Pure),
            _ => bad(),
        },
        "abs" => match args {
            [t @ (Int | Float | Unknown)] => Sig::Ok(t.clone(), Pure),
            _ => bad(),
        },
        "upper" | "lower" | "strip" => fixed(&[Str], Str, Pure),
        "starts_with" | "ends_with" | "contains" => fixed(&[Str, Str], Bool, Pure),
        "replace" => fixed(&[Str, Str, Str], Str, Pure),
        "split" => fixed(&[Str, Str], List(Box::new(Str)), Pure),
        "join" => fixed(&[List(Box::new(Str)), Str], Str, Pure),
        "keys" => match args {
            [Map(_) | Unknown] => Sig::Ok(List(Box::new(Str)), Pure),
            _ => bad(),
        },
        "get" => match args {
            [Map(v), key, default] if Str.accepts(key) && v.accepts(default) => Sig::Ok(v.join(default), Pure),
            [Unknown, _, default] => Sig::Ok(default.clone(), Pure),
            _ => bad(),
        },
        "secs" | "millis" => fixed(&[Duration], Int, Pure),
        "every" => fixed(&[Duration], Schedule, Pure),
        "cron" => fixed(&[Str], Schedule, Pure),
        "env" | "read_file" => fixed(&[Str], Str, Probe),
        "has_env" | "file_exists" => fixed(&[Str], Bool, Probe),
        "now" | "hour" | "minute" | "weekday" | "day" | "month" | "battery" | "cpus" | "mem_used" => {
            fixed(&[], Int, Probe)
        }
        "today" => fixed(&[], Str, Probe),
        "on_ac" => fixed(&[], Bool, Probe),
        "load" => fixed(&[], Float, Probe),
        "uptime" => fixed(&[], Duration, Probe),
        "disk_free" => fixed(&[Str], Int, Probe),
        "log" | "trigger" => fixed(&[Str], Unit, Action),
        "notify" => fixed(&[Str, Str], Unit, Action),
        "pause" => fixed(&[], Unit, Action),
        _ => Sig::Unknown,
    }
}
