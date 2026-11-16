//! The static type checker. It runs over the whole file before anything is
//! evaluated and reports every error it finds: types never mix implicitly,
//! names must exist, immutable variables stay immutable, functions return
//! on every path, and side effects only happen where they are allowed.

use std::collections::{HashMap, HashSet};

use super::ast::*;
use super::diag::{Diagnostic, Span, suggest};
use super::types::{
    BUILTINS, Effect, STATUS_FIELDS, Sig, Ty, builtin, enum_variant, enum_variants, signature_of, status_field,
};

/// The fields a worker can set, and their types. `run` is special: it takes
/// a `str` (run through `sh -c`) or a `list[str]` (run directly).
pub const FIELDS: &[(&str, &str)] = &[
    ("run", "str | list[str]"),
    ("description", "str"),
    ("schedule", "Schedule"),
    ("timeout", "duration"),
    ("jitter", "duration"),
    ("retries", "int"),
    ("restart", "Restart"),
    ("max_restarts", "int"),
    ("restart_window", "duration"),
    ("overlap", "Overlap"),
    ("catch_up", "bool"),
    ("cwd", "str"),
    ("env", "map[str, str]"),
    ("enabled", "bool"),
];

pub const HOOKS: &[&str] = &["when", "on_success", "on_failure"];

fn field_ty(name: &str) -> Option<Ty> {
    Some(match name {
        "description" | "cwd" => Ty::Str,
        "schedule" => Ty::Schedule,
        "timeout" | "jitter" | "restart_window" => Ty::Duration,
        "retries" | "max_restarts" => Ty::Int,
        "restart" => Ty::Restart,
        "overlap" => Ty::Overlap,
        "catch_up" | "enabled" => Ty::Bool,
        "env" => Ty::Map(Box::new(Ty::Str)),
        _ => return None,
    })
}

#[derive(Clone)]
struct Var {
    ty: Ty,
    mutable: bool,
}

#[derive(Clone)]
struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
    effect: Effect,
}

/// Where code is being checked, which decides what it may do.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    /// A top-level `let` or a worker field: evaluated once, at load.
    Const,
    /// A function body.
    Fn,
    /// A `when:` block: evaluated before each scheduled run.
    When,
    /// `on_success:` / `on_failure:`.
    Hook,
}

struct Checker {
    diags: Vec<Diagnostic>,
    scopes: Vec<HashMap<String, Var>>,
    fns: HashMap<String, FnSig>,
    place: Place,
    /// What the function being checked must return.
    ret: Ty,
    /// The strongest effect used in the current function body.
    effect: Effect,
    loops: usize,
    /// The worker whose hooks are being checked.
    worker: Option<String>,
}

pub fn check(program: &Program) -> Vec<Diagnostic> {
    let mut c = Checker {
        diags: Vec::new(),
        scopes: vec![HashMap::new()],
        fns: HashMap::new(),
        place: Place::Const,
        ret: Ty::Unit,
        effect: Effect::Pure,
        loops: 0,
        worker: None,
    };
    let mut workers: HashMap<&str, Span> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Let(l) => {
                c.place = Place::Const;
                if l.mutable {
                    c.err(
                        Diagnostic::error("E0596", "top-level values are constants", l.name.span)
                            .label("can't be `mut`")
                            .help(format!("remove `mut`, or declare `let mut {}` inside a function or hook", l.name.name)),
                    );
                }
                c.let_stmt(l);
            }
            Item::Fn(f) => c.fn_decl(f),
            Item::Worker(w) => {
                if let Some(first) = workers.insert(&w.name.name, w.name.span) {
                    let _ = first;
                    c.diags.push(
                        Diagnostic::error("E0428", format!("the worker `{}` is defined twice", w.name.name), w.name.span)
                            .label("redefined here")
                            .help("worker names must be unique across every worker file"),
                    );
                }
                c.worker(w);
            }
        }
    }
    c.diags
}

/// Does this block return (or otherwise never finish) on every path?
fn always_returns(block: &Block) -> bool {
    block.iter().any(|stmt| match stmt {
        Stmt::Return { .. } => true,
        Stmt::If { branches, otherwise: Some(other), .. } => {
            branches.iter().all(|(_, b)| always_returns(b)) && always_returns(other)
        }
        _ => false,
    })
}

impl Checker {
    fn err(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    fn mismatch(&mut self, expected: &Ty, found: &Ty, span: Span) {
        let mut d = Diagnostic::error("E0308", "mismatched types", span)
            .label(format!("expected `{expected}`, found `{found}`"));
        d = match (expected, found) {
            (Ty::Duration, Ty::Str | Ty::Int) => d.help("write a duration literal such as `30s`, `5m` or `1h30m`"),
            (Ty::Float, Ty::Int) => d.help("write a float literal such as `2.0`, or convert with `float(x)`"),
            (Ty::Int, Ty::Float) => d.help("convert with `int(x)`, which rounds toward zero"),
            (Ty::Str, Ty::Int | Ty::Float | Ty::Bool | Ty::Duration) => d.help("convert with `str(x)`, or use an f-string: `f\"{x}\"`"),
            (Ty::Schedule, Ty::Str) => d.help("use `every(5m)`, `cron(\"0 * * * *\")`, `Schedule.daemon`, `Schedule.startup` or `Schedule.manual`"),
            (Ty::Restart, Ty::Str) => d.help("use `Restart.always`, `Restart.on_failure` or `Restart.never`"),
            (Ty::Overlap, Ty::Str) => d.help("use `Overlap.skip` or `Overlap.replace`"),
            _ => d,
        };
        self.err(d);
    }

    fn expect(&mut self, expected: &Ty, expr: &Expr) -> Ty {
        let found = self.expr(expr);
        if !expected.accepts(&found) {
            self.mismatch(expected, &found, expr.span);
        }
        found
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn declare(&mut self, name: &str, var: Var) {
        self.scopes.last_mut().unwrap().insert(name.to_owned(), var);
    }

    fn visible_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
        names.extend(["Restart", "Overlap", "Schedule"].map(String::from));
        names
    }

    fn resolve_type(&mut self, t: &TypeExpr) -> Ty {
        let arity = |c: &mut Self, n: usize| {
            if t.args.len() != n {
                c.err(Diagnostic::error("E0107", format!("`{}` takes {n} type argument(s), but {} were given", t.name, t.args.len()), t.span));
                false
            } else {
                true
            }
        };
        match t.name.as_str() {
            "int" | "float" | "bool" | "str" | "duration" | "Schedule" | "Restart" | "Overlap" | "Status" => {
                arity(self, 0);
                match t.name.as_str() {
                    "int" => Ty::Int,
                    "float" => Ty::Float,
                    "bool" => Ty::Bool,
                    "str" => Ty::Str,
                    "duration" => Ty::Duration,
                    "Schedule" => Ty::Schedule,
                    "Restart" => Ty::Restart,
                    "Status" => Ty::Status,
                    _ => Ty::Overlap,
                }
            }
            "list" => {
                if arity(self, 1) { Ty::List(Box::new(self.resolve_type(&t.args[0]))) } else { Ty::Unknown }
            }
            "map" => {
                if !arity(self, 2) {
                    return Ty::Unknown;
                }
                if self.resolve_type(&t.args[0]) != Ty::Str {
                    self.err(Diagnostic::error("E0277", "map keys must be `str`", t.args[0].span));
                }
                Ty::Map(Box::new(self.resolve_type(&t.args[1])))
            }
            other => {
                let known = ["int", "float", "bool", "str", "duration", "list", "map", "Schedule", "Restart", "Overlap", "Status"];
                let mut d = Diagnostic::error("E0412", format!("cannot find type `{other}` in this scope"), t.span)
                    .label("not a type");
                if let Some(s) = suggest(other, known) {
                    d = d.help(format!("a type with a similar name exists: `{s}`"));
                }
                self.err(d);
                Ty::Unknown
            }
        }
    }

    // ── declarations ──────────────────────────────────────────────────

    fn let_stmt(&mut self, l: &Let) {
        let found = self.expr(&l.value);
        let ty = match &l.ty {
            Some(t) => {
                let declared = self.resolve_type(t);
                if !declared.accepts(&found) {
                    self.mismatch(&declared, &found, l.value.span);
                }
                declared.join(&found)
            }
            None => {
                if !found.is_known() && found != Ty::Unknown {
                    self.err(
                        Diagnostic::error("E0282", "type annotations needed", l.value.span)
                            .label("the element type of this empty collection is unknown")
                            .help(format!("give it a type: `let {}: list[str] = []`", l.name.name)),
                    );
                }
                found.clone()
            }
        };
        if found == Ty::Unit {
            self.err(Diagnostic::error("E0308", "this expression has no value", l.value.span).label("returns `()`"));
        }
        self.declare(&l.name.name, Var { ty, mutable: l.mutable });
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        let name = &f.name.name;
        if self.fns.contains_key(name) || signature_of(name).is_some() {
            let what = if self.fns.contains_key(name) { "defined twice" } else { "a built-in function" };
            self.err(Diagnostic::error("E0428", format!("the name `{name}` is {what}"), f.name.span).label("redefined here"));
        }
        let params: Vec<Ty> = f.params.iter().map(|(_, t)| self.resolve_type(t)).collect();
        let ret = f.ret.as_ref().map_or(Ty::Unit, |t| self.resolve_type(t));

        let saved = (self.place, std::mem::replace(&mut self.ret, ret.clone()), self.effect, self.loops);
        self.place = Place::Fn;
        self.effect = Effect::Pure;
        self.loops = 0;
        // Functions see the top-level `let`s, but not each other's locals.
        self.scopes.push(HashMap::new());
        let mut seen = HashSet::new();
        for ((p, _), ty) in f.params.iter().zip(&params) {
            if !seen.insert(&p.name) {
                self.err(Diagnostic::error("E0415", format!("parameter `{}` is bound more than once", p.name), p.span));
            }
            self.declare(&p.name, Var { ty: ty.clone(), mutable: false });
        }
        self.block(&f.body);
        self.scopes.pop();
        if ret != Ty::Unit && !always_returns(&f.body) {
            self.err(
                Diagnostic::error("E0317", format!("function `{name}` may not return a value"), f.span)
                    .label(format!("expected `{ret}` on every path"))
                    .help("end every branch with `return ...`"),
            );
        }
        let effect = self.effect;
        (self.place, self.ret, self.effect, self.loops) = saved;
        // Declared only now: a function can't call itself, so evaluation
        // always terminates.
        self.fns.insert(name.clone(), FnSig { params, ret, effect });
    }

    fn worker(&mut self, w: &WorkerDecl) {
        self.worker = Some(w.name.name.clone());
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for member in &w.members {
            let (name, span) = match member {
                Member::Field { name, .. } | Member::Hook { name, .. } => (name.name.as_str(), name.span),
            };
            if seen.insert(name, span).is_some() {
                self.err(Diagnostic::error("E0062", format!("`{name}` is set more than once"), span).label("set again here"));
            }
            match member {
                Member::Field { name, value } => self.worker_field(name, value),
                Member::Hook { name, body } => self.hook(name, body),
            }
        }
        if !seen.contains_key("run") {
            self.err(
                Diagnostic::error("E0063", format!("worker `{}` has nothing to run", w.name.name), w.span)
                    .label("missing `run`")
                    .help("add `run = \"command\"` (through `sh -c`) or `run = [\"program\", \"arg\"]`"),
            );
        }
    }

    fn worker_field(&mut self, name: &Ident, value: &Expr) {
        self.place = Place::Const;
        if name.name == "run" {
            let found = self.expr(value);
            if !matches!(found, Ty::Str | Ty::Unknown) && !Ty::List(Box::new(Ty::Str)).accepts(&found) {
                self.err(
                    Diagnostic::error("E0308", "mismatched types", value.span)
                        .label(format!("expected `str` or `list[str]`, found `{found}`")),
                );
            }
            return;
        }
        let Some(expected) = field_ty(&name.name) else {
            let hooks_and_fields = FIELDS.iter().map(|(f, _)| *f).chain(HOOKS.iter().copied());
            let mut d = Diagnostic::error("E0560", format!("workers have no field named `{}`", name.name), name.span)
                .label("unknown field");
            if let Some(s) = suggest(&name.name, hooks_and_fields) {
                d = d.help(format!("did you mean `{s}`?"));
            } else {
                let all: Vec<&str> = FIELDS.iter().map(|(f, _)| *f).collect();
                d = d.note(format!("the fields are: {}", all.join(", ")));
            }
            if HOOKS.contains(&name.name.as_str()) {
                d = d.help(format!("`{}` is a hook: write `{}:` followed by a block", name.name, name.name));
            }
            self.err(d);
            self.expr(value);
            return;
        };
        if HOOKS.contains(&name.name.as_str()) {
            return;
        }
        self.expect(&expected, value);
    }

    fn hook(&mut self, name: &Ident, body: &Block) {
        let (place, ret) = match name.name.as_str() {
            "when" => (Place::When, Ty::Bool),
            "on_success" | "on_failure" => (Place::Hook, Ty::Unit),
            other => {
                let mut d = Diagnostic::error("E0560", format!("workers have no hook named `{other}`"), name.span)
                    .label("unknown hook")
                    .note("the hooks are `when:`, `on_success:` and `on_failure:`");
                if let Some(s) = suggest(other, HOOKS.iter().copied()) {
                    d = d.help(format!("did you mean `{s}`?"));
                } else if field_ty(other).is_some() || other == "run" {
                    d = d.help(format!("`{other}` is a field: write `{other} = ...`"));
                }
                self.err(d);
                return;
            }
        };
        self.place = place;
        self.ret = ret.clone();
        self.loops = 0;
        self.scopes.push(HashMap::from([("status".to_owned(), Var { ty: Ty::Status, mutable: false })]));
        self.block(body);
        self.scopes.pop();
        if ret == Ty::Bool && !always_returns(body) {
            self.err(
                Diagnostic::error("E0317", "`when:` may not decide", name.span)
                    .label("expected a `bool` on every path")
                    .help("write `when: <condition>` on one line, or end every branch of the block with `return true` / `return false`"),
            );
        }
    }

    // ── statements ────────────────────────────────────────────────────

    fn block(&mut self, block: &Block) {
        self.scopes.push(HashMap::new());
        for stmt in block {
            self.stmt(stmt);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(l) => self.let_stmt(l),
            Stmt::Assign { target, value } => {
                let found = self.expr(value);
                match self.lookup(&target.name).cloned() {
                    None => self.unknown_name(&target.name, target.span),
                    Some(var) if !var.mutable => self.err(
                        Diagnostic::error("E0384", format!("cannot assign twice to immutable variable `{}`", target.name), target.span)
                            .label("cannot assign twice")
                            .help(format!("make it mutable where it is declared: `let mut {}`", target.name)),
                    ),
                    Some(var) => {
                        if !var.ty.accepts(&found) {
                            self.mismatch(&var.ty, &found, value.span);
                        }
                    }
                }
            }
            Stmt::Expr(e) => {
                let ty = self.expr(e);
                if ty != Ty::Unit && ty != Ty::Unknown && !matches!(e.kind, ExprKind::Call(..)) {
                    self.err(
                        Diagnostic::error("E0308", "this value is computed and then thrown away", e.span)
                            .label(format!("a `{ty}` that is never used"))
                            .help("assign it with `let`, or return it"),
                    );
                }
            }
            Stmt::If { branches, otherwise, .. } => {
                for (cond, body) in branches {
                    self.expect(&Ty::Bool, cond);
                    self.block(body);
                }
                if let Some(body) = otherwise {
                    self.block(body);
                }
            }
            Stmt::For { var, iter, body, .. } => {
                let ty = self.expr(iter);
                let elem = match ty {
                    Ty::List(t) => *t,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        let mut d = Diagnostic::error("E0277", format!("`{other}` is not iterable"), iter.span)
                            .label("expected a `list`");
                        if matches!(other, Ty::Map(_)) {
                            d = d.help("loop over `keys(m)`");
                        } else if other == Ty::Int {
                            d = d.help("loop over `range(n)`");
                        }
                        self.err(d);
                        Ty::Unknown
                    }
                };
                self.scopes.push(HashMap::from([(var.name.clone(), Var { ty: elem, mutable: false })]));
                self.loops += 1;
                self.block(body);
                self.loops -= 1;
                self.scopes.pop();
            }
            Stmt::Return { value, span } => {
                if self.place == Place::Const {
                    return;
                }
                let found = value.as_ref().map_or(Ty::Unit, |v| self.expr(v));
                let expected = self.ret.clone();
                if !expected.accepts(&found) {
                    let at = value.as_ref().map_or(*span, |v| v.span);
                    let mut d = Diagnostic::error("E0308", "mismatched types", at)
                        .label(format!("expected `{expected}`, found `{found}`"));
                    if self.place == Place::Hook {
                        d = d.help("`on_success:` and `on_failure:` return nothing: write a bare `return`");
                    } else if self.place == Place::When {
                        d = d.help("`when:` decides with `return true` or `return false`");
                    }
                    self.err(d);
                }
            }
            Stmt::Pass => {}
            Stmt::Break(span) | Stmt::Continue(span) => {
                if self.loops == 0 {
                    let word = if matches!(stmt, Stmt::Break(_)) { "break" } else { "continue" };
                    self.err(Diagnostic::error("E0268", format!("`{word}` outside of a loop"), *span).label("cannot be used outside a `for` loop"));
                }
            }
        }
    }

    fn unknown_name(&mut self, name: &str, span: Span) {
        let mut d = Diagnostic::error("E0425", format!("cannot find value `{name}` in this scope"), span).label("not found in this scope");
        let names = self.visible_names();
        if let Some(s) = suggest(name, names.iter().map(String::as_str)) {
            d = d.help(format!("a value with a similar name exists: `{s}`"));
        } else if name == "status" {
            d = d.note("`status` exists only inside `when:`, `on_success:` and `on_failure:`; pass it to a function as a value");
        } else if name == "True" || name == "False" {
            d = d.help(format!("booleans are lowercase: `{}`", name.to_lowercase()));
        } else if name == "None" {
            d = d.note("there is no `None`: every value has a type");
        }
        self.err(d);
    }

    // ── expressions ───────────────────────────────────────────────────

    fn expr(&mut self, e: &Expr) -> Ty {
        match &e.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::Duration(_) => Ty::Duration,
            ExprKind::FStr(parts) => {
                for part in parts {
                    if let FPartE::Expr(inner) = part {
                        let ty = self.expr(inner);
                        if !ty.is_displayable() {
                            self.err(
                                Diagnostic::error("E0277", format!("`{ty}` can't be formatted in an f-string"), inner.span)
                                    .label("only int, float, bool, str and duration can be formatted")
                                    .help("for a list, format `join(xs, \", \")`"),
                            );
                        }
                    }
                }
                Ty::Str
            }
            ExprKind::Name(name) => match self.lookup(name) {
                Some(var) => var.ty.clone(),
                None => {
                    if ["Restart", "Overlap", "Schedule"].contains(&name.as_str()) {
                        self.err(
                            Diagnostic::error("E0423", format!("expected value, found enum `{name}`"), e.span)
                                .help(format!("pick a variant: {}", enum_variants(name).iter().map(|v| format!("`{name}.{v}`")).collect::<Vec<_>>().join(", "))),
                        );
                    } else if self.fns.contains_key(name) || signature_of(name).is_some() {
                        self.err(
                            Diagnostic::error("E0423", format!("expected value, found function `{name}`"), e.span)
                                .help(format!("call it: `{name}(...)`")),
                        );
                    } else {
                        self.unknown_name(name, e.span);
                    }
                    Ty::Unknown
                }
            },
            ExprKind::List(items) => {
                let mut elem = Ty::Unknown;
                for item in items {
                    let ty = self.expr(item);
                    if !elem.accepts(&ty) {
                        self.err(
                            Diagnostic::error("E0308", "mismatched types", item.span)
                                .label(format!("expected `{elem}`, found `{ty}`"))
                                .note("every item of a list has the same type"),
                        );
                    } else {
                        elem = elem.join(&ty);
                    }
                }
                Ty::List(Box::new(elem))
            }
            ExprKind::Map(pairs) => {
                let mut value_ty = Ty::Unknown;
                let mut keys = HashSet::new();
                for (k, v) in pairs {
                    self.expect(&Ty::Str, k);
                    if let ExprKind::Str(s) = &k.kind
                        && !keys.insert(s.clone())
                    {
                        self.err(Diagnostic::error("E0062", format!("key `{s}` appears more than once"), k.span));
                    }
                    let ty = self.expr(v);
                    if !value_ty.accepts(&ty) {
                        self.err(
                            Diagnostic::error("E0308", "mismatched types", v.span)
                                .label(format!("expected `{value_ty}`, found `{ty}`"))
                                .note("every value of a map has the same type"),
                        );
                    } else {
                        value_ty = value_ty.join(&ty);
                    }
                }
                Ty::Map(Box::new(value_ty))
            }
            ExprKind::Unary(op, inner) => {
                let ty = self.expr(inner);
                match (op, &ty) {
                    (_, Ty::Unknown) => Ty::Unknown,
                    (UnOp::Not, Ty::Bool) => Ty::Bool,
                    (UnOp::Neg, Ty::Int | Ty::Float) => ty,
                    (UnOp::Not, _) => {
                        self.mismatch(&Ty::Bool, &ty, inner.span);
                        Ty::Bool
                    }
                    (UnOp::Neg, _) => {
                        self.err(Diagnostic::error("E0600", format!("cannot apply unary operator `-` to type `{ty}`"), e.span));
                        Ty::Unknown
                    }
                }
            }
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs, e.span),
            ExprKind::Call(callee, args) => self.call(callee, args, e.span),
            ExprKind::Field(base, field) => {
                if let ExprKind::Name(ns) = &base.kind
                    && self.lookup(ns).is_none()
                    && ["Restart", "Overlap", "Schedule"].contains(&ns.as_str())
                {
                    return match enum_variant(ns, &field.name) {
                        Some(ty) => ty,
                        None => {
                            let mut d = Diagnostic::error("E0599", format!("no variant named `{}` in `{ns}`", field.name), field.span)
                                .label("variant not found");
                            if let Some(s) = suggest(&field.name, enum_variants(ns).iter().copied()) {
                                d = d.help(format!("did you mean `{ns}.{s}`?"));
                            } else {
                                d = d.note(format!("`{ns}` has: {}", enum_variants(ns).join(", ")));
                            }
                            if ns == "Schedule" {
                                d = d.help("timed schedules are built with `every(5m)` or `cron(\"0 * * * *\")`");
                            }
                            self.err(d);
                            Ty::Unknown
                        }
                    };
                }
                let ty = self.expr(base);
                match ty {
                    Ty::Unknown => Ty::Unknown,
                    Ty::Status => status_field(&field.name).unwrap_or_else(|| {
                        let mut d = Diagnostic::error("E0609", format!("no field `{}` on type `Status`", field.name), field.span)
                            .label("unknown field");
                        if let Some(s) = suggest(&field.name, STATUS_FIELDS.iter().map(|(n, _, _)| *n)) {
                            d = d.help(format!("did you mean `{s}`?"));
                        }
                        let all: Vec<&str> = STATUS_FIELDS.iter().map(|(n, _, _)| *n).collect();
                        self.err(d.note(format!("`Status` has: {}", all.join(", "))));
                        Ty::Unknown
                    }),
                    other => {
                        let mut d = Diagnostic::error("E0609", format!("no field `{}` on type `{other}`", field.name), field.span);
                        if signature_of(&field.name).is_some() || self.fns.contains_key(&field.name) {
                            d = d.help(format!("`{}` is a function: call it with `.{}()`", field.name, field.name));
                        }
                        self.err(d);
                        Ty::Unknown
                    }
                }
            }
            ExprKind::Index(base, index) => {
                let ty = self.expr(base);
                match ty {
                    Ty::List(elem) => {
                        self.expect(&Ty::Int, index);
                        *elem
                    }
                    Ty::Map(value) => {
                        self.expect(&Ty::Str, index);
                        *value
                    }
                    Ty::Unknown => {
                        self.expr(index);
                        Ty::Unknown
                    }
                    other => {
                        self.err(Diagnostic::error("E0608", format!("cannot index into a value of type `{other}`"), e.span));
                        Ty::Unknown
                    }
                }
            }
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        use Ty::*;
        if matches!(op, BinOp::And | BinOp::Or) {
            self.expect(&Bool, lhs);
            self.expect(&Bool, rhs);
            return Bool;
        }
        let l = self.expr(lhs);
        let r = self.expr(rhs);
        if l == Unknown || r == Unknown {
            return match op {
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::In | BinOp::NotIn => Bool,
                _ => l.join(&r),
            };
        }
        let result = match op {
            BinOp::Add => match (&l, &r) {
                (Int, Int) | (Float, Float) | (Str, Str) | (Duration, Duration) => Some(l.clone()),
                (List(_), List(_)) if l.accepts(&r) => Some(l.join(&r)),
                _ => None,
            },
            BinOp::Sub => match (&l, &r) {
                (Int, Int) | (Float, Float) | (Duration, Duration) => Some(l.clone()),
                _ => None,
            },
            BinOp::Mul => match (&l, &r) {
                (Int, Int) | (Float, Float) => Some(l.clone()),
                (Duration, Int) | (Int, Duration) => Some(Duration),
                _ => None,
            },
            BinOp::Div => match (&l, &r) {
                (Int, Int) | (Float, Float) => Some(l.clone()),
                (Duration, Int) => Some(Duration),
                (Duration, Duration) => Some(Int),
                _ => None,
            },
            BinOp::Rem => match (&l, &r) {
                (Int, Int) => Some(Int),
                _ => None,
            },
            BinOp::Eq | BinOp::Ne => (l.accepts(&r) && l != Unit && l != Status).then_some(Bool),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                (l == r && matches!(l, Int | Float | Duration | Str)).then_some(Bool)
            }
            BinOp::In | BinOp::NotIn => match (&l, &r) {
                (Str, Str) | (Str, Map(_)) => Some(Bool),
                (t, List(e)) if e.accepts(t) => Some(Bool),
                _ => None,
            },
            BinOp::And | BinOp::Or => unreachable!(),
        };
        result.unwrap_or_else(|| {
            let verb = match op {
                BinOp::Add => format!("cannot add `{r}` to `{l}`"),
                BinOp::Sub => format!("cannot subtract `{r}` from `{l}`"),
                BinOp::Mul => format!("cannot multiply `{l}` by `{r}`"),
                BinOp::Div => format!("cannot divide `{l}` by `{r}`"),
                BinOp::In | BinOp::NotIn => format!("cannot look for a `{l}` in a `{r}`"),
                _ => format!("cannot compare `{l}` with `{r}`"),
            };
            let mut d = Diagnostic::error("E0369", verb, span).label(format!("`{l}` {} `{r}`", op.symbol()));
            d = match (&l, &r) {
                (Int, Float) | (Float, Int) => d.help("there are no implicit conversions: use `float(x)` or `int(x)` on one side"),
                (Str, Int | Float | Bool | Duration) | (Int | Float | Bool | Duration, Str) => {
                    d.help("convert with `str(x)`, or build the text with an f-string: `f\"... {x}\"`")
                }
                (Duration, Float) | (Float, Duration) => d.help("durations multiply by an `int`: `2 * 30s`"),
                _ => d,
            };
            self.err(d);
            Unknown
        })
    }

    fn call(&mut self, callee: &Ident, args: &[Expr], span: Span) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(a)).collect();
        let name = callee.name.as_str();

        let (ret, effect) = if let Some(sig) = self.fns.get(name).cloned() {
            if sig.params.len() != args.len() {
                self.err(
                    Diagnostic::error("E0061", format!("this function takes {} argument(s) but {} were supplied", sig.params.len(), args.len()), span)
                        .label(format!("`{name}` called with the wrong number of arguments")),
                );
            } else {
                for ((param, found), arg) in sig.params.iter().zip(&arg_tys).zip(args) {
                    if !param.accepts(found) {
                        self.mismatch(param, found, arg.span);
                    }
                }
            }
            (sig.ret, sig.effect)
        } else {
            match builtin(name, &arg_tys) {
                Sig::Ok(ret, effect) => (ret, effect),
                Sig::BadArgs(expected) => {
                    let found: Vec<String> = arg_tys.iter().map(|t| format!("`{t}`")).collect();
                    let label = if found.is_empty() { "called with no arguments".to_string() } else { format!("called with {}", found.join(", ")) };
                    self.err(Diagnostic::error("E0061", format!("wrong arguments to `{name}`"), span).label(label).note(expected));
                    return builtin_ret_guess(name);
                }
                Sig::Unknown => {
                    let mut d = Diagnostic::error("E0425", format!("cannot find function `{name}` in this scope"), callee.span)
                        .label("not found in this scope");
                    let candidates = self.fns.keys().map(String::as_str).chain(BUILTINS.iter().map(|(n, _)| *n));
                    if let Some(s) = suggest(name, candidates) {
                        d = d.help(format!("a function with a similar name exists: `{s}`"));
                    }
                    d = d.note("functions must be defined above the code that calls them, and can't call themselves");
                    self.err(d);
                    return Ty::Unknown;
                }
            }
        };
        self.allow_effect(effect, name, span);
        if name == "trigger"
            && let [Expr { kind: ExprKind::Str(target), .. }] = args
            && self.worker.as_deref() == Some(target.as_str())
            && self.place == Place::Hook
        {
            self.err(
                Diagnostic::error("E0391", format!("worker `{target}` triggers itself"), span)
                    .label("this would run it again forever")
                    .help("to repeat it, give it a schedule such as `every(5m)`, or use `retries` for failures"),
            );
        }
        ret
    }

    fn allow_effect(&mut self, effect: Effect, name: &str, span: Span) {
        self.effect = self.effect.max(effect);
        let problem = match (self.place, effect) {
            (_, Effect::Pure) | (Place::Hook, _) | (Place::Fn, _) | (Place::When, Effect::Probe) => None,
            (Place::Const, Effect::Probe) => Some((
                format!("`{name}()` reads live state, so it can't be used in a field or a top-level `let`"),
                "those are evaluated once, when the file is loaded; use it inside `when:` or a hook, which run every time",
            )),
            (Place::Const, Effect::Action) => Some((
                format!("`{name}()` has side effects, so it can't be used in a field or a top-level `let`"),
                "side effects belong in `on_success:` or `on_failure:`",
            )),
            (Place::When, Effect::Action) => Some((
                format!("`{name}()` has side effects, so it can't be used in `when:`"),
                "`when:` only decides whether to run; act in `on_success:` or `on_failure:`",
            )),
        };
        if let Some((message, help)) = problem {
            self.err(Diagnostic::error("E0015", message, span).label("not allowed here").help(help));
        }
    }
}

/// After an argument error, the built-in's usual return type (so the error
/// doesn't cascade).
fn builtin_ret_guess(name: &str) -> Ty {
    match builtin(name, &[Ty::Unknown, Ty::Unknown, Ty::Unknown][..arity_guess(name)]) {
        Sig::Ok(ret, _) => ret,
        _ => Ty::Unknown,
    }
}

fn arity_guess(name: &str) -> usize {
    signature_of(name).map_or(0, |s| {
        let inside = s.split_once('(').and_then(|(_, r)| r.split_once(')')).map_or("", |(a, _)| a);
        if inside.trim().is_empty() { 0 } else { inside.split(',').count().min(3) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::awl::{lexer::lex, parser::parse};

    fn errors(src: &str) -> Vec<&'static str> {
        let program = parse(lex(src).unwrap()).unwrap();
        check(&program).into_iter().map(|d| d.code).collect()
    }

    fn ok(src: &str) {
        let program = parse(lex(src).unwrap()).unwrap();
        let diags = check(&program);
        assert!(diags.is_empty(), "{}", diags.iter().map(|d| d.render("t.awl", src)).collect::<String>());
    }

    #[test]
    fn a_full_worker_checks() {
        ok(r#"
let quiet: list[int] = [22, 23, 0, 1]

fn is_quiet(h: int) -> bool:
    return h in quiet

fn label(code: int) -> str:
    if code == 0:
        return "ok"
    elif code < 0:
        return "never ran"
    else:
        return f"failed ({code})"

worker backup:
    description = "Nightly backup"
    run = ["restic", "backup", "~/docs"]
    schedule = cron("0 3 * * *")
    timeout = 2h + 30m
    retries = 2
    restart = Restart.on_failure
    overlap = Overlap.skip
    env = {"RESTIC_REPOSITORY": "/mnt/backup"}
    when:
        if not on_ac():
            return false
        return disk_free("/") > 10 and not is_quiet(hour())
    on_failure:
        notify("backup", f"{label(status.exit)} after {status.duration}")
        if status.failures > 3:
            pause()
    on_success: log("backup done in " + str(secs(status.duration)) + "s")
"#);
    }

    #[test]
    fn type_errors_are_reported() {
        let base = "worker w:\n    run = \"true\"\n";
        assert_eq!(errors(&format!("{base}    timeout = \"30s\"\n")), ["E0308"]);
        assert_eq!(errors(&format!("{base}    retries = 1.5\n")), ["E0308"]);
        assert_eq!(errors(&format!("{base}    schedule = \"daemon\"\n")), ["E0308"]);
        assert_eq!(errors(&format!("{base}    timout = 5s\n")), ["E0560"]);
        assert_eq!(errors(&format!("{base}    retries = 1\n    retries = 2\n")), ["E0062"]);
        assert_eq!(errors(&format!("{base}    restart = Restart.sometimes\n")), ["E0599"]);
        assert_eq!(errors("worker w:\n    timeout = 5s\n"), ["E0063"]);
        assert_eq!(errors("let x = 1 + 2.0\n"), ["E0369"]);
        assert_eq!(errors("let x = \"a\" + 1\n"), ["E0369"]);
        assert_eq!(errors("let x = 1\nlet y = z\n"), ["E0425"]);
        assert_eq!(errors("let x = []\n"), ["E0282"]);
        assert_eq!(errors("let x = [1, \"a\"]\n"), ["E0308"]);
        assert_eq!(errors("let x: flaot = 1.0\n"), ["E0412"]);
        assert_eq!(errors("let x = len(1)\n"), ["E0061"]);
        assert_eq!(errors("let x = nope(1)\n"), ["E0425"]);
        assert_eq!(errors("let mut x = 1\n"), ["E0596"]);
    }

    #[test]
    fn mutability_and_control_flow() {
        assert_eq!(errors("fn f() -> int:\n    let x = 1\n    x = 2\n    return x\n"), ["E0384"]);
        ok("fn f() -> int:\n    let mut x = 1\n    x = 2\n    return x\n");
        assert_eq!(errors("fn f(x: int) -> int:\n    if x > 0:\n        return 1\n"), ["E0317"]);
        assert_eq!(errors("fn f() -> int:\n    return \"a\"\n"), ["E0308"]);
        assert_eq!(errors("fn f():\n    break\n"), ["E0268"]);
        assert_eq!(errors("fn f():\n    for x in 5:\n        pass\n"), ["E0277"]);
        assert_eq!(errors("fn f() -> int:\n    return f()\n"), ["E0425"]);
        assert_eq!(errors("worker w:\n    run = \"x\"\n    when:\n        if on_ac():\n            return true\n"), ["E0317"]);
        assert_eq!(errors("worker w:\n    run = \"x\"\n    when: 1\n"), ["E0308"]);
    }

    #[test]
    fn effects_are_confined() {
        // Live state at load time.
        assert_eq!(errors("let h = hour()\n"), ["E0015"]);
        assert_eq!(errors("worker w:\n    run = \"x\"\n    description = env(\"USER\")\n"), ["E0015"]);
        // Side effects in a condition, directly or through a function.
        assert_eq!(errors("worker w:\n    run = \"x\"\n    when:\n        log(\"x\")\n        return true\n"), ["E0015"]);
        assert_eq!(errors("fn shout() -> bool:\n    notify(\"a\", \"b\")\n    return true\nworker w:\n    run = \"x\"\n    when: shout()\n"), ["E0015"]);
        // `status` only exists in hooks.
        assert_eq!(errors("fn f() -> int:\n    return status.exit\n"), ["E0425"]);
        assert_eq!(errors("worker w:\n    run = \"x\"\n    on_failure: log(status.code)\n"), ["E0609"]);
        assert_eq!(errors("worker w:\n    run = \"x\"\n    on_fail: pass\n"), ["E0560"]);
        assert_eq!(errors("worker w:\n    run = \"x\"\n    on_success: trigger(\"w\")\n"), ["E0391"]);
        ok("fn bad(s: Status) -> bool:\n    return s.failures > 3\nworker w:\n    run = \"x\"\n    when: not bad(status)\n");
    }
}
