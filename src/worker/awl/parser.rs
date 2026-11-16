//! Tokens → syntax tree, by recursive descent.

use super::ast::*;
use super::diag::{Diagnostic, Span};
use super::lexer::{FPart, Kw, Tok, Token};

/// Deepest nesting of expressions and blocks, so a pathological file is an
/// error instead of a stack overflow.
const MAX_NESTING: usize = 32;

pub fn parse(tokens: Vec<Token>) -> Result<Program, Diagnostic> {
    let mut p = Parser { tokens, pos: 0, depth: 0 };
    let mut items = Vec::new();
    let mut extents = Vec::new();
    p.skip_newlines();
    while !p.at(&Tok::Eof) {
        let start = p.span().start;
        items.push(p.item()?);
        // End at the last real token: the closing dedent sits after any
        // comment lines that follow the item.
        let end = p.tokens[..p.pos]
            .iter()
            .rev()
            .find(|t| !matches!(t.tok, Tok::Newline | Tok::Indent | Tok::Dedent))
            .map_or(start, |t| t.span.end);
        extents.push((start, end));
        p.skip_newlines();
    }
    Ok(Program { items, extents })
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

type PResult<T> = Result<T, Diagnostic>;

fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Ident(name) => format!("identifier `{name}`"),
        Tok::Int(n) => format!("`{n}`"),
        Tok::Float(n) => format!("`{n}`"),
        Tok::Str(_) | Tok::FStr(_) => "a string".into(),
        Tok::Duration(_) => "a duration".into(),
        Tok::Kw(kw) => format!("keyword `{}`", format!("{kw:?}").to_lowercase()),
        Tok::Op(op) => format!("`{op}`"),
        Tok::Newline => "end of line".into(),
        Tok::Indent => "an indented block".into(),
        Tok::Dedent => "the end of the block".into(),
        Tok::Eof => "end of file".into(),
    }
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek() == tok
    }

    fn at_op(&self, op: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }

    fn at_kw(&self, kw: Kw) -> bool {
        *self.peek() == Tok::Kw(kw)
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn eat_op(&mut self, op: &str) -> bool {
        let yes = self.at_op(op);
        if yes {
            self.advance();
        }
        yes
    }

    fn eat_kw(&mut self, kw: Kw) -> bool {
        let yes = self.at_kw(kw);
        if yes {
            self.advance();
        }
        yes
    }

    /// Runs `f` one nesting level deeper.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        if self.depth >= MAX_NESTING {
            return Err(Diagnostic::error("E0108", "nested too deeply", self.span())
                .label(format!("more than {MAX_NESTING} levels of nesting")));
        }
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn unexpected(&self, expected: &str) -> Diagnostic {
        let found = describe(self.peek());
        Diagnostic::error("E0100", format!("expected {expected}, found {found}"), self.span())
            .label(format!("expected {expected}"))
    }

    fn expect_op(&mut self, op: &str) -> PResult<Span> {
        if self.at_op(op) { Ok(self.advance().span) } else { Err(self.unexpected(&format!("`{op}`"))) }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        match self.peek().clone() {
            Tok::Ident(name) => Ok(Ident { name, span: self.advance().span }),
            Tok::Kw(kw) => {
                let word = format!("{kw:?}").to_lowercase();
                Err(Diagnostic::error("E0101", format!("expected {what}, found keyword `{word}`"), self.span())
                    .label("keywords can't be used as names"))
            }
            _ => Err(self.unexpected(what)),
        }
    }

    fn skip_newlines(&mut self) {
        while self.at(&Tok::Newline) {
            self.advance();
        }
    }

    fn end_of_line(&mut self) -> PResult<()> {
        match self.peek() {
            Tok::Newline => {
                self.advance();
                Ok(())
            }
            Tok::Eof | Tok::Dedent => Ok(()),
            _ => Err(self.unexpected("end of line")),
        }
    }

    // ── items ─────────────────────────────────────────────────────────

    fn item(&mut self) -> PResult<Item> {
        match self.peek() {
            Tok::Kw(Kw::Let) => Ok(Item::Let(self.let_stmt()?)),
            Tok::Kw(Kw::Fn) => Ok(Item::Fn(self.fn_decl()?)),
            Tok::Kw(Kw::Worker) => Ok(Item::Worker(self.worker()?)),
            Tok::Indent => Err(Diagnostic::error("E0102", "unexpected indent", self.span())),
            _ => Err(self.unexpected("`worker`, `fn` or `let` at the top level")
                .help("top-level code is only declarations; put statements inside a hook such as `on_success:`")),
        }
    }

    fn let_stmt(&mut self) -> PResult<Let> {
        let start = self.advance().span; // let
        let mutable = self.eat_kw(Kw::Mut);
        let name = self.ident("a variable name")?;
        let ty = if self.eat_op(":") { Some(self.type_expr()?) } else { None };
        self.expect_op("=")?;
        let value = self.expr()?;
        let span = start.to(value.span);
        self.end_of_line()?;
        Ok(Let { name, mutable, ty, value, span })
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let name = self.ident("a type")?;
        let mut args = Vec::new();
        let mut span = name.span;
        if self.eat_op("[") {
            loop {
                args.push(self.type_expr()?);
                if !self.eat_op(",") {
                    break;
                }
            }
            span = span.to(self.expect_op("]")?);
        }
        Ok(TypeExpr { name: name.name, args, span })
    }

    fn fn_decl(&mut self) -> PResult<FnDecl> {
        let start = self.advance().span; // fn
        let name = self.ident("a function name")?;
        self.expect_op("(")?;
        let mut params = Vec::new();
        while !self.at_op(")") {
            let param = self.ident("a parameter name")?;
            if !self.at_op(":") {
                return Err(Diagnostic::error("E0103", "missing type for function parameter", param.span)
                    .label("needs a type")
                    .help(format!("parameters are typed: `{}: int`", param.name)));
            }
            self.advance();
            params.push((param, self.type_expr()?));
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        let ret = if self.eat_op("->") { Some(self.type_expr()?) } else { None };
        let head = start.to(self.prev_span());
        self.expect_op(":")?;
        let body = self.block()?;
        Ok(FnDecl { name, params, ret, body, span: head })
    }

    fn worker(&mut self) -> PResult<WorkerDecl> {
        let start = self.advance().span; // worker
        let name = self.ident("a worker name")?;
        let span = start.to(name.span);
        self.expect_op(":")?;
        self.end_of_line()?;
        if !self.at(&Tok::Indent) {
            return Err(self.unexpected("an indented block of worker fields"));
        }
        self.advance();
        let mut members = Vec::new();
        while !matches!(self.peek(), Tok::Dedent | Tok::Eof) {
            let field = self.ident("a worker field or hook")?;
            if self.eat_op("=") {
                let value = self.expr()?;
                self.end_of_line()?;
                members.push(Member::Field { name: field, value });
            } else if self.eat_op(":") {
                let body = if self.at(&Tok::Newline) {
                    self.block_after_colon()?
                } else {
                    // `when: cond` on one line means `return cond`.
                    let stmt = self.simple_stmt()?;
                    let stmt = match stmt {
                        Stmt::Expr(e) if field.name == "when" => Stmt::Return { span: e.span, value: Some(e) },
                        other => other,
                    };
                    self.end_of_line()?;
                    vec![stmt]
                };
                members.push(Member::Hook { name: field, body });
            } else {
                return Err(self.unexpected("`=` or `:`")
                    .help("fields are written `name = value`; hooks are written `when:`, `on_success:` or `on_failure:`"));
            }
        }
        self.advance(); // dedent
        Ok(WorkerDecl { name, members, span })
    }

    // ── statements ────────────────────────────────────────────────────

    /// `:` already eaten; a newline and an indented block follow.
    fn block_after_colon(&mut self) -> PResult<Block> {
        self.nested(Self::block_after_colon_inner)
    }

    fn block_after_colon_inner(&mut self) -> PResult<Block> {
        self.end_of_line()?;
        if !self.at(&Tok::Indent) {
            return Err(self.unexpected("an indented block"));
        }
        self.advance();
        let mut stmts = Vec::new();
        while !matches!(self.peek(), Tok::Dedent | Tok::Eof) {
            stmts.push(self.stmt()?);
        }
        self.advance();
        Ok(stmts)
    }

    fn block(&mut self) -> PResult<Block> {
        if self.at(&Tok::Newline) {
            self.block_after_colon()
        } else {
            let stmt = self.simple_stmt()?;
            self.end_of_line()?;
            Ok(vec![stmt])
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            Tok::Kw(Kw::If) => self.if_stmt(),
            Tok::Kw(Kw::For) => {
                let start = self.advance().span;
                let var = self.ident("a loop variable")?;
                if !self.eat_kw(Kw::In) {
                    return Err(self.unexpected("`in`"));
                }
                let iter = self.expr()?;
                let span = start.to(iter.span);
                self.expect_op(":")?;
                let body = self.block()?;
                Ok(Stmt::For { var, iter, body, span })
            }
            Tok::Kw(Kw::Let) => Ok(Stmt::Let(self.let_stmt()?)),
            Tok::Indent => Err(Diagnostic::error("E0102", "unexpected indent", self.span())),
            _ => {
                let stmt = self.simple_stmt()?;
                self.end_of_line()?;
                Ok(stmt)
            }
        }
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let start = self.advance().span; // if
        let mut branches = Vec::new();
        let cond = self.expr()?;
        self.expect_op(":")?;
        branches.push((cond, self.block()?));
        let mut otherwise = None;
        loop {
            if self.eat_kw(Kw::Elif) {
                let cond = self.expr()?;
                self.expect_op(":")?;
                branches.push((cond, self.block()?));
            } else if self.eat_kw(Kw::Else) {
                self.expect_op(":")?;
                otherwise = Some(self.block()?);
                break;
            } else {
                break;
            }
        }
        Ok(Stmt::If { branches, otherwise, span: start })
    }

    /// A statement that fits on one line.
    fn simple_stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            Tok::Kw(Kw::Pass) => {
                self.advance();
                Ok(Stmt::Pass)
            }
            Tok::Kw(Kw::Break) => Ok(Stmt::Break(self.advance().span)),
            Tok::Kw(Kw::Continue) => Ok(Stmt::Continue(self.advance().span)),
            Tok::Kw(Kw::Return) => {
                let span = self.advance().span;
                let value = if matches!(self.peek(), Tok::Newline | Tok::Dedent | Tok::Eof) {
                    None
                } else {
                    Some(self.expr()?)
                };
                Ok(Stmt::Return { span: value.as_ref().map_or(span, |v| span.to(v.span)), value })
            }
            Tok::Kw(Kw::Let) => {
                Err(Diagnostic::error("E0104", "`let` must be on its own line", self.span()))
            }
            _ => {
                let expr = self.expr()?;
                if self.at_op("=") {
                    let ExprKind::Name(name) = &expr.kind else {
                        return Err(Diagnostic::error("E0105", "invalid assignment target", expr.span)
                            .label("only a variable can be assigned to"));
                    };
                    self.advance();
                    let value = self.expr()?;
                    return Ok(Stmt::Assign { target: Ident { name: name.clone(), span: expr.span }, value });
                }
                Ok(Stmt::Expr(expr))
            }
        }
    }

    // ── expressions, lowest precedence first ──────────────────────────

    pub fn expr(&mut self) -> PResult<Expr> {
        self.nested(Self::or_expr)
    }

    fn binary(lhs: Expr, op: BinOp, rhs: Expr) -> Expr {
        let span = lhs.span.to(rhs.span);
        Expr { kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span }
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        while self.eat_kw(Kw::Or) {
            let rhs = self.and_expr()?;
            lhs = Self::binary(lhs, BinOp::Or, rhs);
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.not_expr()?;
        while self.eat_kw(Kw::And) {
            let rhs = self.not_expr()?;
            lhs = Self::binary(lhs, BinOp::And, rhs);
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if self.at_kw(Kw::Not) {
            let start = self.advance().span;
            let inner = self.nested(Self::not_expr)?;
            let span = start.to(inner.span);
            return Ok(Expr { kind: ExprKind::Unary(UnOp::Not, Box::new(inner)), span });
        }
        self.comparison()
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let lhs = self.additive()?;
        let op = match self.peek() {
            Tok::Op("==") => BinOp::Eq,
            Tok::Op("!=") => BinOp::Ne,
            Tok::Op("<") => BinOp::Lt,
            Tok::Op("<=") => BinOp::Le,
            Tok::Op(">") => BinOp::Gt,
            Tok::Op(">=") => BinOp::Ge,
            Tok::Kw(Kw::In) => BinOp::In,
            Tok::Kw(Kw::Not) if self.tokens.get(self.pos + 1).is_some_and(|t| t.tok == Tok::Kw(Kw::In)) => {
                self.advance();
                BinOp::NotIn
            }
            _ => return Ok(lhs),
        };
        self.advance();
        let rhs = self.additive()?;
        if matches!(self.peek(), Tok::Op("==" | "!=" | "<" | "<=" | ">" | ">=")) {
            return Err(Diagnostic::error("E0106", "comparison operators cannot be chained", self.span())
                .help("split it with `and`: `a < b and b < c`"));
        }
        Ok(Self::binary(lhs, op, rhs))
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Op("+") => BinOp::Add,
                Tok::Op("-") => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.multiplicative()?;
            lhs = Self::binary(lhs, op, rhs);
        }
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Op("*") => BinOp::Mul,
                Tok::Op("/") => BinOp::Div,
                Tok::Op("%") => BinOp::Rem,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.unary()?;
            lhs = Self::binary(lhs, op, rhs);
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.at_op("-") {
            let start = self.advance().span;
            let inner = self.nested(Self::unary)?;
            let span = start.to(inner.span);
            return Ok(Expr { kind: ExprKind::Unary(UnOp::Neg, Box::new(inner)), span });
        }
        self.postfix()
    }

    fn args(&mut self) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.at_op(")") {
            args.push(self.expr()?);
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(args)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.atom()?;
        loop {
            if self.at_op("(") {
                let ExprKind::Name(name) = &expr.kind else {
                    return Err(Diagnostic::error("E0107", "only named functions can be called", expr.span));
                };
                let callee = Ident { name: name.clone(), span: expr.span };
                self.advance();
                let args = self.args()?;
                let span = expr.span.to(self.prev_span());
                expr = Expr { kind: ExprKind::Call(callee, args), span };
            } else if self.eat_op(".") {
                let field = self.ident("a field or method name")?;
                if self.eat_op("(") {
                    // `x.f(a)` is `f(x, a)`.
                    let mut args = vec![expr.clone()];
                    args.extend(self.args()?);
                    let span = expr.span.to(self.prev_span());
                    expr = Expr { kind: ExprKind::Call(field, args), span };
                } else {
                    let span = expr.span.to(field.span);
                    expr = Expr { kind: ExprKind::Field(Box::new(expr), field), span };
                }
            } else if self.eat_op("[") {
                let index = self.expr()?;
                let end = self.expect_op("]")?;
                let span = expr.span.to(end);
                expr = Expr { kind: ExprKind::Index(Box::new(expr), Box::new(index)), span };
            } else {
                return Ok(expr);
            }
        }
    }

    fn atom(&mut self) -> PResult<Expr> {
        let starts_atom = matches!(
            self.peek(),
            Tok::Int(_)
                | Tok::Float(_)
                | Tok::Str(_)
                | Tok::FStr(_)
                | Tok::Duration(_)
                | Tok::Ident(_)
                | Tok::Kw(Kw::True | Kw::False)
                | Tok::Op("(" | "[" | "{")
        );
        if !starts_atom {
            let mut d = self.unexpected("an expression");
            if self.at_op("=") {
                d = d.help("`=` assigns; compare with `==`");
            }
            return Err(d);
        }
        let token = self.advance();
        let span = token.span;
        let kind = match token.tok {
            Tok::Int(n) => ExprKind::Int(n),
            Tok::Float(f) => ExprKind::Float(f),
            Tok::Str(s) => ExprKind::Str(s),
            Tok::Duration(ms) => ExprKind::Duration(ms),
            Tok::Kw(Kw::True) => ExprKind::Bool(true),
            Tok::Kw(Kw::False) => ExprKind::Bool(false),
            Tok::Ident(name) => ExprKind::Name(name),
            Tok::FStr(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    out.push(match part {
                        FPart::Lit(s) => FPartE::Lit(s),
                        FPart::Expr(tokens, _) => {
                            let mut sub = Parser { tokens, pos: 0, depth: self.depth };
                            let e = sub.expr()?;
                            if !sub.at(&Tok::Eof) {
                                return Err(sub.unexpected("`}`"));
                            }
                            FPartE::Expr(e)
                        }
                    });
                }
                ExprKind::FStr(out)
            }
            Tok::Op("(") => {
                let inner = self.expr()?;
                self.expect_op(")")?;
                return Ok(Expr { kind: inner.kind, span: span.to(self.prev_span()) });
            }
            Tok::Op("[") => {
                let mut items = Vec::new();
                while !self.at_op("]") {
                    items.push(self.expr()?);
                    if !self.eat_op(",") {
                        break;
                    }
                }
                let end = self.expect_op("]")?;
                return Ok(Expr { kind: ExprKind::List(items), span: span.to(end) });
            }
            Tok::Op("{") => {
                let mut pairs = Vec::new();
                while !self.at_op("}") {
                    let key = self.expr()?;
                    self.expect_op(":")?;
                    let value = self.expr()?;
                    pairs.push((key, value));
                    if !self.eat_op(",") {
                        break;
                    }
                }
                let end = self.expect_op("}")?;
                return Ok(Expr { kind: ExprKind::Map(pairs), span: span.to(end) });
            }
            _ => unreachable!("checked by starts_atom"),
        };
        Ok(Expr { kind, span })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::awl::lexer::lex;

    fn parse_src(src: &str) -> PResult<Program> {
        parse(lex(src)?)
    }

    #[test]
    fn parses_a_worker_with_hooks() {
        let program = parse_src(
            "let limit: int = 90\n\
             fn busy(load: float) -> bool:\n    return load > 2.0\n\n\
             worker backup:\n\
             \x20   run = [\"restic\", \"backup\"]\n\
             \x20   schedule = cron(\"0 3 * * *\")\n\
             \x20   when: on_ac() and not busy(load())\n\
             \x20   on_failure:\n\
             \x20       if status.exit == 2:\n\
             \x20           notify(\"backup\", f\"exit {status.exit}\")\n\
             \x20       else:\n\
             \x20           pass\n",
        )
        .unwrap();
        assert_eq!(program.items.len(), 3);
        let Item::Worker(w) = &program.items[2] else { panic!() };
        assert_eq!(w.members.len(), 4);
        let Member::Hook { name, body } = &w.members[2] else { panic!() };
        assert_eq!(name.name, "when");
        assert!(matches!(body[0], Stmt::Return { .. }));
    }

    #[test]
    fn precedence() {
        let program = parse_src("let x = 1 + 2 * 3 == 7 and not false\n").unwrap();
        let Item::Let(l) = &program.items[0] else { panic!() };
        let ExprKind::Binary(BinOp::And, lhs, _) = &l.value.kind else { panic!("{:?}", l.value) };
        assert!(matches!(lhs.kind, ExprKind::Binary(BinOp::Eq, _, _)));
    }

    #[test]
    fn method_calls_are_function_calls() {
        let program = parse_src("let x = \"a\".upper()\n").unwrap();
        let Item::Let(l) = &program.items[0] else { panic!() };
        assert!(matches!(&l.value.kind, ExprKind::Call(f, args) if f.name == "upper" && args.len() == 1));
    }

    #[test]
    fn syntax_errors() {
        assert_eq!(parse_src("log(\"x\")\n").unwrap_err().code, "E0100");
        assert_eq!(parse_src("fn f(x) -> int:\n    return 1\n").unwrap_err().code, "E0103");
        assert_eq!(parse_src("let x = 1 < 2 < 3\n").unwrap_err().code, "E0106");
        assert_eq!(parse_src("worker w:\n    run \"x\"\n").unwrap_err().code, "E0100");
        assert_eq!(parse_src("let fn = 1\n").unwrap_err().code, "E0101");
    }
}
