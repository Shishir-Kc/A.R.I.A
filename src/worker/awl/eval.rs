//! The evaluator. It runs only code the checker accepted, so a type error
//! here is a bug. It never sleeps or starts a process: side effects are
//! collected as [`Action`]s for the daemon to carry out, and every
//! evaluation runs on a fixed budget of steps.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use chrono::{Datelike, Timelike};

use super::ast::*;
use super::diag::Span;
use super::host::Host;

/// Steps one evaluation may take before it is stopped.
pub const FUEL: u64 = 200_000;
/// Deepest function-call nesting.
const MAX_DEPTH: usize = 64;
/// Longest list, and longest string in bytes, a script can build. Each step
/// is cheap only while values stay small.
pub const MAX_LEN: usize = 100_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    /// Milliseconds.
    Duration(i64),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Schedule(ScheduleValue),
    Restart(String),
    Overlap(String),
    Status(StatusValue),
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScheduleValue {
    Daemon,
    Startup,
    Manual,
    /// Milliseconds.
    Every(i64),
    Cron(String),
}

/// What a script sees as `status`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusValue {
    pub name: String,
    pub runs: i64,
    pub failures: i64,
    pub exit: i64,
    pub success: bool,
    pub duration_ms: i64,
    pub retry: i64,
}

/// A side effect requested by a hook, carried out by the daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Log(String),
    Notify { title: String, body: String },
    /// Run another worker now.
    Trigger(String),
    /// Stop scheduling this worker until it is started again.
    Pause,
}

pub fn format_duration(ms: i64) -> String {
    if ms == 0 {
        return "0s".into();
    }
    let sign = if ms < 0 { "-" } else { "" };
    let mut left = ms.unsigned_abs();
    let mut out = String::from(sign);
    for (unit, size) in [("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1_000), ("ms", 1)] {
        if left >= size {
            out.push_str(&format!("{}{unit}", left / size));
            left %= size;
        }
    }
    out
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) if x.fract() == 0.0 && x.is_finite() => write!(f, "{x:.1}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Str(s) => write!(f, "{s}"),
            Value::Duration(ms) => write!(f, "{}", format_duration(*ms)),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Map(m) => {
                let parts: Vec<String> = m.iter().map(|(k, v)| format!("{k:?}: {v}")).collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            Value::Schedule(s) => write!(f, "{s:?}"),
            Value::Restart(v) => write!(f, "Restart.{v}"),
            Value::Overlap(v) => write!(f, "Overlap.{v}"),
            Value::Status(s) => write!(f, "Status({})", s.name),
            Value::Unit => write!(f, "()"),
        }
    }
}

/// A runtime error (division by zero, index out of range...) and where.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeError {
    pub message: String,
    pub span: Span,
}

type R<T> = Result<T, RuntimeError>;

/// How a block finished.
enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

pub struct Interp<'a> {
    pub fns: &'a HashMap<String, FnDecl>,
    pub globals: &'a HashMap<String, Value>,
    pub host: Option<&'a dyn Host>,
    pub actions: Vec<Action>,
    fuel: u64,
    depth: usize,
    scopes: Vec<HashMap<String, Value>>,
}

fn err<T>(message: impl Into<String>, span: Span) -> R<T> {
    Err(RuntimeError { message: message.into(), span })
}

fn sized(v: Value, span: Span) -> R<Value> {
    let len = match &v {
        Value::Str(s) => s.len(),
        Value::List(l) => l.len(),
        Value::Map(m) => m.len(),
        _ => 0,
    };
    if len > MAX_LEN {
        return err(format!("value too large ({len} items or bytes; the limit is {MAX_LEN})"), span);
    }
    Ok(v)
}

impl<'a> Interp<'a> {
    pub fn new(fns: &'a HashMap<String, FnDecl>, globals: &'a HashMap<String, Value>, host: Option<&'a dyn Host>) -> Self {
        Self { fns, globals, host, actions: Vec::new(), fuel: FUEL, depth: 0, scopes: Vec::new() }
    }

    fn tick(&mut self, span: Span) -> R<()> {
        if self.fuel == 0 {
            return err(format!("evaluation stopped after {FUEL} steps"), span);
        }
        self.fuel -= 1;
        Ok(())
    }

    fn host(&self, span: Span) -> R<&'a dyn Host> {
        match self.host {
            Some(h) => Ok(h),
            None => err("live state is not available here", span),
        }
    }

    /// Runs a hook body with `status` in scope. Returns what it returned.
    pub fn run_hook(&mut self, body: &Block, status: StatusValue) -> R<Value> {
        self.scopes = vec![HashMap::from([("status".to_owned(), Value::Status(status))])];
        match self.block(body)? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Value::Unit),
        }
    }

    pub fn eval(&mut self, e: &Expr) -> R<Value> {
        self.expr(e)
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).or_else(|| self.globals.get(name))
    }

    fn block(&mut self, block: &Block) -> R<Flow> {
        self.scopes.push(HashMap::new());
        let result = self.stmts(block);
        self.scopes.pop();
        result
    }

    fn stmts(&mut self, block: &Block) -> R<Flow> {
        for stmt in block {
            match self.stmt(stmt)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn stmt(&mut self, stmt: &Stmt) -> R<Flow> {
        match stmt {
            Stmt::Let(l) => {
                let v = self.expr(&l.value)?;
                self.scopes.last_mut().unwrap().insert(l.name.name.clone(), v);
            }
            Stmt::Assign { target, value } => {
                let v = self.expr(value)?;
                if let Some(slot) = self.scopes.iter_mut().rev().find_map(|s| s.get_mut(&target.name)) {
                    *slot = v;
                }
            }
            Stmt::Expr(e) => {
                self.expr(e)?;
            }
            Stmt::If { branches, otherwise, .. } => {
                for (cond, body) in branches {
                    if self.expr(cond)? == Value::Bool(true) {
                        return self.block(body);
                    }
                }
                if let Some(body) = otherwise {
                    return self.block(body);
                }
            }
            Stmt::For { var, iter, body, span } => {
                let Value::List(items) = self.expr(iter)? else { return err("not a list", iter.span) };
                for item in items {
                    self.tick(*span)?;
                    self.scopes.push(HashMap::from([(var.name.clone(), item)]));
                    let flow = self.block(body);
                    self.scopes.pop();
                    match flow? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                }
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.expr(e)?,
                    None => Value::Unit,
                };
                return Ok(Flow::Return(v));
            }
            Stmt::Pass => {}
            Stmt::Break(_) => return Ok(Flow::Break),
            Stmt::Continue(_) => return Ok(Flow::Continue),
        }
        Ok(Flow::Normal)
    }

    fn expr(&mut self, e: &Expr) -> R<Value> {
        self.tick(e.span)?;
        Ok(match &e.kind {
            ExprKind::Int(n) => Value::Int(*n),
            ExprKind::Float(x) => Value::Float(*x),
            ExprKind::Bool(b) => Value::Bool(*b),
            ExprKind::Str(s) => Value::Str(s.clone()),
            ExprKind::Duration(ms) => Value::Duration(*ms),
            ExprKind::FStr(parts) => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        FPartE::Lit(s) => out.push_str(s),
                        FPartE::Expr(inner) => out.push_str(&self.expr(inner)?.to_string()),
                    }
                }
                sized(Value::Str(out), e.span)?
            }
            ExprKind::Name(name) => match self.lookup(name) {
                Some(v) => v.clone(),
                None => return err(format!("`{name}` is not defined"), e.span),
            },
            ExprKind::List(items) => Value::List(items.iter().map(|i| self.expr(i)).collect::<R<_>>()?),
            ExprKind::Map(pairs) => {
                let mut map = BTreeMap::new();
                for (k, v) in pairs {
                    let Value::Str(key) = self.expr(k)? else { return err("map keys must be str", k.span) };
                    let value = self.expr(v)?;
                    map.insert(key, value);
                }
                Value::Map(map)
            }
            ExprKind::Unary(op, inner) => match (op, self.expr(inner)?) {
                (UnOp::Not, Value::Bool(b)) => Value::Bool(!b),
                (UnOp::Neg, Value::Int(n)) => match n.checked_neg() {
                    Some(v) => Value::Int(v),
                    None => return err("integer overflow", e.span),
                },
                (UnOp::Neg, Value::Float(x)) => Value::Float(-x),
                _ => return err("bad operand", e.span),
            },
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, e.span)?,
            ExprKind::Call(callee, args) => {
                let values = args.iter().map(|a| self.expr(a)).collect::<R<Vec<_>>>()?;
                self.call(&callee.name, values, e.span)?
            }
            ExprKind::Field(base, field) => {
                if let ExprKind::Name(ns) = &base.kind
                    && self.lookup(ns).is_none()
                {
                    let v = field.name.clone();
                    return Ok(match ns.as_str() {
                        "Restart" => Value::Restart(v),
                        "Overlap" => Value::Overlap(v),
                        "Schedule" => Value::Schedule(match v.as_str() {
                            "daemon" => ScheduleValue::Daemon,
                            "startup" => ScheduleValue::Startup,
                            _ => ScheduleValue::Manual,
                        }),
                        _ => return err(format!("`{ns}` is not defined"), base.span),
                    });
                }
                let Value::Status(s) = self.expr(base)? else { return err("no such field", field.span) };
                match field.name.as_str() {
                    "name" => Value::Str(s.name),
                    "runs" => Value::Int(s.runs),
                    "failures" => Value::Int(s.failures),
                    "exit" => Value::Int(s.exit),
                    "success" => Value::Bool(s.success),
                    "duration" => Value::Duration(s.duration_ms),
                    "retry" => Value::Int(s.retry),
                    _ => return err("no such field", field.span),
                }
            }
            ExprKind::Index(base, index) => match (self.expr(base)?, self.expr(index)?) {
                (Value::List(items), Value::Int(i)) => {
                    let len = items.len() as i64;
                    // Negative indexes count from the end, like Python.
                    let at = if i < 0 { len + i } else { i };
                    match usize::try_from(at).ok().and_then(|at| items.into_iter().nth(at)) {
                        Some(v) => v,
                        None => return err(format!("index {i} is out of range for a list of length {len}"), index.span),
                    }
                }
                (Value::Map(map), Value::Str(key)) => match map.get(&key) {
                    Some(v) => v.clone(),
                    None => return err(format!("key {key:?} is not in the map (use `get(m, key, default)`)"), index.span),
                },
                _ => return err("cannot index", e.span),
            },
        })
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> R<Value> {
        use Value::*;
        // Short-circuit.
        if matches!(op, BinOp::And | BinOp::Or) {
            let l = self.expr(lhs)? == Bool(true);
            if (op == BinOp::And && !l) || (op == BinOp::Or && l) {
                return Ok(Bool(l));
            }
            return Ok(Bool(self.expr(rhs)? == Bool(true)));
        }
        let l = self.expr(lhs)?;
        let r = self.expr(rhs)?;
        let overflow = || RuntimeError { message: "integer overflow".into(), span };
        let zero = || RuntimeError { message: "division by zero".into(), span };
        Ok(match (op, l, r) {
            (BinOp::Add, Int(a), Int(b)) => Int(a.checked_add(b).ok_or_else(overflow)?),
            (BinOp::Sub, Int(a), Int(b)) => Int(a.checked_sub(b).ok_or_else(overflow)?),
            (BinOp::Mul, Int(a), Int(b)) => Int(a.checked_mul(b).ok_or_else(overflow)?),
            (BinOp::Div, Int(a), Int(b)) => Int(a.checked_div(b).ok_or_else(|| if b == 0 { zero() } else { overflow() })?),
            (BinOp::Rem, Int(a), Int(b)) => Int(a.checked_rem(b).ok_or_else(|| if b == 0 { zero() } else { overflow() })?),
            (BinOp::Add, Float(a), Float(b)) => Float(a + b),
            (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
            (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
            (BinOp::Div, Float(a), Float(b)) => Float(a / b),
            (BinOp::Add, Duration(a), Duration(b)) => Duration(a.checked_add(b).ok_or_else(overflow)?),
            (BinOp::Sub, Duration(a), Duration(b)) => Duration(a.checked_sub(b).ok_or_else(overflow)?),
            (BinOp::Mul, Duration(a), Int(b)) | (BinOp::Mul, Int(b), Duration(a)) => {
                Duration(a.checked_mul(b).ok_or_else(overflow)?)
            }
            (BinOp::Div, Duration(a), Int(b)) => Duration(a.checked_div(b).ok_or_else(zero)?),
            (BinOp::Div, Duration(a), Duration(b)) => Int(a.checked_div(b).ok_or_else(zero)?),
            (BinOp::Add, Str(a), Str(b)) => sized(Str(a + &b), span)?,
            (BinOp::Add, List(mut a), List(b)) => {
                a.extend(b);
                sized(List(a), span)?
            }
            (BinOp::Eq, a, b) => Bool(a == b),
            (BinOp::Ne, a, b) => Bool(a != b),
            (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, a, b) => {
                let ord = match (a, b) {
                    (Int(a), Int(b)) => a.partial_cmp(&b),
                    (Float(a), Float(b)) => a.partial_cmp(&b),
                    (Duration(a), Duration(b)) => a.partial_cmp(&b),
                    (Str(a), Str(b)) => a.partial_cmp(&b),
                    _ => None,
                };
                let Some(ord) = ord else { return Ok(Bool(false)) };
                Bool(match op {
                    BinOp::Lt => ord.is_lt(),
                    BinOp::Le => ord.is_le(),
                    BinOp::Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                })
            }
            (BinOp::In | BinOp::NotIn, needle, hay) => {
                let found = match (&needle, &hay) {
                    (Str(n), Str(h)) => h.contains(n.as_str()),
                    (Str(n), Map(m)) => m.contains_key(n),
                    (n, List(items)) => items.contains(n),
                    _ => false,
                };
                Bool(found == (op == BinOp::In))
            }
            _ => return err("bad operands", span),
        })
    }

    fn call(&mut self, name: &str, args: Vec<Value>, span: Span) -> R<Value> {
        if let Some(f) = self.fns.get(name) {
            if self.depth >= MAX_DEPTH {
                return err("functions nested too deeply", span);
            }
            let frame: HashMap<String, Value> =
                f.params.iter().map(|(p, _)| p.name.clone()).zip(args).collect();
            // A function sees its parameters and the globals, nothing else.
            let saved = std::mem::replace(&mut self.scopes, vec![frame]);
            self.depth += 1;
            let result = self.block(&f.body);
            self.depth -= 1;
            self.scopes = saved;
            return Ok(match result? {
                Flow::Return(v) => v,
                _ => Value::Unit,
            });
        }
        self.builtin(name, args, span)
    }

    fn builtin(&mut self, name: &str, args: Vec<Value>, span: Span) -> R<Value> {
        use Value::*;
        let mut a = args.into_iter();
        let mut next = || a.next().unwrap_or(Unit);
        let s = |v: Value| if let Str(s) = v { s } else { String::new() };
        Ok(match name {
            "str" => Str(next().to_string()),
            "int" => match next() {
                Float(x) if x.is_finite() && x.abs() < 9.2e18 => Int(x as i64),
                Float(_) => return err("float is out of range for int", span),
                Str(t) => match t.trim().parse() {
                    Ok(n) => Int(n),
                    Err(_) => return err(format!("{t:?} is not an int"), span),
                },
                _ => return err("bad argument", span),
            },
            "float" => match next() {
                Int(n) => Float(n as f64),
                Str(t) => match t.trim().parse() {
                    Ok(x) => Float(x),
                    Err(_) => return err(format!("{t:?} is not a float"), span),
                },
                _ => return err("bad argument", span),
            },
            "len" => Int(match next() {
                Str(t) => t.chars().count() as i64,
                List(l) => l.len() as i64,
                Map(m) => m.len() as i64,
                _ => 0,
            }),
            "range" => {
                let first = next();
                let second = next();
                let (Int(start), Int(end)) = (if second == Unit { (Int(0), first) } else { (first, second) }) else {
                    return err("bad argument", span);
                };
                if end.saturating_sub(start) > MAX_LEN as i64 {
                    return err(format!("range is too large ({MAX_LEN} items at most)"), span);
                }
                List((start..end).map(Int).collect())
            }
            "min" | "max" => {
                let (x, y) = (next(), next());
                let less = match (&x, &y) {
                    (Int(a), Int(b)) => a < b,
                    (Float(a), Float(b)) => a < b,
                    (Duration(a), Duration(b)) => a < b,
                    _ => return err("bad argument", span),
                };
                if less == (name == "min") { x } else { y }
            }
            "abs" => match next() {
                Int(n) => Int(n.checked_abs().ok_or_else(|| RuntimeError { message: "integer overflow".into(), span })?),
                Float(x) => Float(x.abs()),
                _ => return err("bad argument", span),
            },
            "upper" => sized(Str(s(next()).to_uppercase()), span)?,
            "lower" => Str(s(next()).to_lowercase()),
            "strip" => Str(s(next()).trim().to_owned()),
            "starts_with" => {
                let (x, p) = (s(next()), s(next()));
                Bool(x.starts_with(&p))
            }
            "ends_with" => {
                let (x, p) = (s(next()), s(next()));
                Bool(x.ends_with(&p))
            }
            "contains" => {
                let (x, p) = (s(next()), s(next()));
                Bool(x.contains(&p))
            }
            "replace" => {
                let (x, from, to) = (s(next()), s(next()), s(next()));
                if from.is_empty() {
                    return err("`replace` needs a non-empty pattern", span);
                }
                sized(Str(x.replace(&from, &to)), span)?
            }
            "split" => {
                let (x, sep) = (s(next()), s(next()));
                if sep.is_empty() {
                    List(x.split_whitespace().map(|p| Str(p.to_owned())).collect())
                } else {
                    List(x.split(sep.as_str()).map(|p| Str(p.to_owned())).collect())
                }
            }
            "join" => {
                let (List(items), sep) = (next(), next()) else { return err("bad argument", span) };
                let parts: Vec<String> = items.into_iter().map(s).collect();
                sized(Str(parts.join(&s(sep))), span)?
            }
            "keys" => match next() {
                Map(m) => List(m.into_keys().map(Str).collect()),
                _ => return err("bad argument", span),
            },
            "get" => {
                let (m, key, default) = (next(), s(next()), next());
                match m {
                    Map(mut m) => m.remove(&key).unwrap_or(default),
                    _ => default,
                }
            }
            "secs" => match next() {
                Duration(ms) => Int(ms / 1000),
                _ => return err("bad argument", span),
            },
            "millis" => match next() {
                Duration(ms) => Int(ms),
                _ => return err("bad argument", span),
            },
            "every" => match next() {
                Duration(ms) => Schedule(ScheduleValue::Every(ms)),
                _ => return err("bad argument", span),
            },
            "cron" => Schedule(ScheduleValue::Cron(s(next()))),
            // Live state.
            "env" => Str(self.host(span)?.env(&s(next())).unwrap_or_default()),
            "has_env" => Bool(self.host(span)?.env(&s(next())).is_some()),
            "file_exists" => Bool(self.host(span)?.path_exists(&s(next()))),
            "read_file" => Str(self.host(span)?.read_file(&s(next())).unwrap_or_default()),
            "now" => Int(self.host(span)?.now().timestamp()),
            "hour" => Int(self.host(span)?.now().hour() as i64),
            "minute" => Int(self.host(span)?.now().minute() as i64),
            "weekday" => Int(self.host(span)?.now().weekday().num_days_from_monday() as i64),
            "day" => Int(self.host(span)?.now().day() as i64),
            "month" => Int(self.host(span)?.now().month() as i64),
            "today" => Str(self.host(span)?.now().format("%Y-%m-%d").to_string()),
            "battery" => Int(self.host(span)?.battery()),
            "on_ac" => Bool(self.host(span)?.on_ac()),
            "load" => Float(self.host(span)?.load()),
            "cpus" => Int(self.host(span)?.cpus()),
            "mem_used" => Int(self.host(span)?.mem_used()),
            "disk_free" => Int(self.host(span)?.disk_free(&s(next()))),
            "uptime" => Duration(self.host(span)?.uptime().saturating_mul(1000)),
            // Actions.
            "log" => {
                self.actions.push(Action::Log(s(next())));
                Unit
            }
            "notify" => {
                let (title, body) = (s(next()), s(next()));
                self.actions.push(Action::Notify { title, body });
                Unit
            }
            "trigger" => {
                self.actions.push(Action::Trigger(s(next())));
                Unit
            }
            "pause" => {
                self.actions.push(Action::Pause);
                Unit
            }
            _ => return err(format!("unknown function `{name}`"), span),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_format_compactly() {
        assert_eq!(format_duration(5_400_000), "1h30m");
        assert_eq!(format_duration(1_250), "1s250ms");
        assert_eq!(format_duration(0), "0s");
        assert_eq!(Value::Float(2.0).to_string(), "2.0");
    }

    #[test]
    fn values_cannot_grow_without_bound() {
        use crate::worker::awl::compile;
        let src = "worker w:\n    run = \"x\"\n    when:\n        let mut s = \"ab\"\n        for i in range(40):\n            s = s + s\n        return len(s) > 0\n";
        let workers = compile(src, "t.awl").unwrap();
        let host = crate::worker::awl::host::SystemHost;
        let error = workers[0].logic.as_ref().unwrap().should_run(&host, StatusValue::default()).unwrap_err();
        assert!(error.contains("value too large"), "{error}");
    }
}
