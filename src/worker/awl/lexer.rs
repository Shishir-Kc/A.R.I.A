//! Source text → tokens. Indentation is significant (like Python): every
//! deeper line opens an `Indent`, every shallower one closes `Dedent`s, and
//! line breaks inside brackets are ignored.

use super::diag::{Diagnostic, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    /// `f"exit {status.exit}"`: literal text and the tokens of each `{...}`.
    FStr(Vec<FPart>),
    /// A duration literal (`30s`, `1h30m`, `250ms`), in milliseconds.
    Duration(i64),
    Kw(Kw),
    Op(&'static str),
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FPart {
    Lit(String),
    Expr(Vec<Token>, Span),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kw {
    Let,
    Mut,
    Fn,
    Return,
    If,
    Elif,
    Else,
    For,
    In,
    And,
    Or,
    Not,
    True,
    False,
    Worker,
    Pass,
    Break,
    Continue,
}

impl Kw {
    fn from(word: &str) -> Option<Kw> {
        Some(match word {
            "let" => Kw::Let,
            "mut" => Kw::Mut,
            "fn" => Kw::Fn,
            "return" => Kw::Return,
            "if" => Kw::If,
            "elif" => Kw::Elif,
            "else" => Kw::Else,
            "for" => Kw::For,
            "in" => Kw::In,
            "and" => Kw::And,
            "or" => Kw::Or,
            "not" => Kw::Not,
            "true" => Kw::True,
            "false" => Kw::False,
            "worker" => Kw::Worker,
            "pass" => Kw::Pass,
            "break" => Kw::Break,
            "continue" => Kw::Continue,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// Longest first, so `<=` wins over `<`.
const OPS: &[&str] = &[
    "->", "==", "!=", "<=", ">=", "+", "-", "*", "/", "%", "<", ">", "=", ":", ",", ".", "(", ")", "[", "]", "{",
    "}",
];

pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    Lexer { src, pos: 0, base: 0, depth: 0, out: Vec::new() }.run(true)
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    /// Offset of `src` in the file (non-zero inside an f-string).
    base: usize,
    /// Bracket nesting: newlines and indentation don't count inside brackets.
    depth: usize,
    out: Vec<Token>,
}

impl Lexer<'_> {
    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.base + start, self.base + end)
    }

    fn push(&mut self, tok: Tok, start: usize) {
        let span = self.span(start, self.pos);
        self.out.push(Token { tok, span });
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.src[self.pos..].chars().nth(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn run(mut self, layout: bool) -> Result<Vec<Token>, Diagnostic> {
        let mut indents: Vec<usize> = vec![0];
        let mut indent_char: Option<char> = None;
        let mut at_line_start = layout;

        loop {
            if at_line_start {
                at_line_start = false;
                // Measure this line's indentation; skip blank and comment-only lines.
                let line_start = self.pos;
                let mut width = 0;
                let mut chars = Vec::new();
                while let Some(c @ (' ' | '\t')) = self.peek() {
                    chars.push((self.pos, c));
                    self.bump();
                    width += 1;
                }
                match self.peek() {
                    None => break,
                    Some('\n') | Some('\r') | Some('#') => {
                        // Blank and comment-only lines don't count, whatever their whitespace.
                        self.skip_comment();
                        if self.peek().is_some() {
                            self.bump();
                        }
                        at_line_start = true;
                        continue;
                    }
                    _ => {}
                }
                for (at, c) in chars {
                    if *indent_char.get_or_insert(c) != c {
                        return Err(Diagnostic::error(
                            "E0001",
                            "mixed tabs and spaces in indentation",
                            self.span(at, at + 1),
                        )
                        .label("indent with spaces only (or tabs only) throughout the file"));
                    }
                }
                let current = *indents.last().unwrap();
                if width > current {
                    indents.push(width);
                    self.push(Tok::Indent, line_start);
                } else if width < current {
                    while width < *indents.last().unwrap() {
                        indents.pop();
                        self.push(Tok::Dedent, self.pos);
                    }
                    if width != *indents.last().unwrap() {
                        return Err(Diagnostic::error(
                            "E0002",
                            "unindent does not match any outer indentation level",
                            self.span(line_start, self.pos.max(line_start + 1)),
                        ));
                    }
                }
            }

            let Some(c) = self.peek() else { break };
            let start = self.pos;
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '#' => self.skip_comment(),
                '\\' if self.peek2() == Some('\n') => {
                    // Explicit line continuation.
                    self.bump();
                    self.bump();
                }
                '\n' => {
                    self.bump();
                    if self.depth == 0 && layout {
                        if !matches!(self.out.last().map(|t| &t.tok), Some(Tok::Newline) | None) {
                            self.push(Tok::Newline, start);
                        }
                        at_line_start = true;
                    }
                }
                '"' => {
                    let text = self.string(start)?;
                    self.push(Tok::Str(text), start);
                }
                'f' if self.peek2() == Some('"') => {
                    self.bump();
                    let parts = self.fstring(start)?;
                    self.push(Tok::FStr(parts), start);
                }
                c if c.is_ascii_digit() => self.number(start)?,
                c if c.is_alphabetic() || c == '_' => {
                    while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        self.bump();
                    }
                    let word = &self.src[start..self.pos];
                    let tok = match Kw::from(word) {
                        Some(kw) => Tok::Kw(kw),
                        None => Tok::Ident(word.to_owned()),
                    };
                    self.push(tok, start);
                }
                _ => {
                    let rest = &self.src[self.pos..];
                    let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) else {
                        self.bump();
                        return Err(Diagnostic::error(
                            "E0003",
                            format!("unexpected character `{c}`"),
                            self.span(start, self.pos),
                        ));
                    };
                    self.pos += op.len();
                    match *op {
                        "(" | "[" | "{" => self.depth += 1,
                        ")" | "]" | "}" => self.depth = self.depth.saturating_sub(1),
                        _ => {}
                    }
                    self.push(Tok::Op(op), start);
                }
            }
        }

        if layout {
            if !matches!(self.out.last().map(|t| &t.tok), Some(Tok::Newline) | None) {
                self.push(Tok::Newline, self.pos);
            }
            while indents.len() > 1 {
                indents.pop();
                self.push(Tok::Dedent, self.pos);
            }
        }
        self.push(Tok::Eof, self.pos);
        Ok(self.out)
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while self.peek().is_some_and(|c| c != '\n') {
                self.bump();
            }
        } else if self.peek() == Some('\r') {
            self.bump();
        }
    }

    fn escape(&mut self, start: usize) -> Result<char, Diagnostic> {
        let at = self.pos;
        Ok(match self.bump() {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('0') => '\0',
            Some('\\') => '\\',
            Some('"') => '"',
            Some('{') => '{',
            Some('}') => '}',
            Some(other) => {
                return Err(Diagnostic::error(
                    "E0004",
                    format!("unknown escape `\\{other}`"),
                    self.span(at - 1, self.pos),
                ));
            }
            None => return Err(self.unterminated(start)),
        })
    }

    fn unterminated(&self, start: usize) -> Diagnostic {
        Diagnostic::error("E0005", "unterminated string", self.span(start, start + 1))
            .label("this string is never closed")
    }

    fn string(&mut self, start: usize) -> Result<String, Diagnostic> {
        self.bump(); // opening quote
        let mut text = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(self.unterminated(start)),
                Some('"') => return Ok(text),
                Some('\\') => text.push(self.escape(start)?),
                Some(c) => text.push(c),
            }
        }
    }

    fn fstring(&mut self, start: usize) -> Result<Vec<FPart>, Diagnostic> {
        self.bump(); // opening quote
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(self.unterminated(start)),
                Some('"') => break,
                Some('\\') => text.push(self.escape(start)?),
                Some('{') => {
                    if !text.is_empty() {
                        parts.push(FPart::Lit(std::mem::take(&mut text)));
                    }
                    let inner_start = self.pos;
                    let mut depth = 0;
                    loop {
                        match self.peek() {
                            None | Some('\n') => return Err(self.unterminated(start)),
                            Some('}') if depth == 0 => break,
                            Some('"') => {
                                // A string inside the braces: `f"{get(m, \"k\", 0)}"`.
                                let at = self.pos;
                                self.string(at)?;
                            }
                            Some(c) => {
                                if "([{".contains(c) {
                                    depth += 1;
                                } else if ")]}".contains(c) {
                                    depth -= 1;
                                }
                                self.bump();
                            }
                        }
                    }
                    let inner = &self.src[inner_start..self.pos];
                    let span = self.span(inner_start, self.pos);
                    self.bump(); // closing brace
                    if inner.trim().is_empty() {
                        return Err(Diagnostic::error("E0006", "empty `{}` in f-string", span)
                            .help("write `\\{` for a literal brace"));
                    }
                    let tokens = Lexer { src: inner, pos: 0, base: self.base + inner_start, depth: 1, out: Vec::new() }
                        .run(false)?;
                    parts.push(FPart::Expr(tokens, span));
                }
                Some('}') => {
                    return Err(Diagnostic::error("E0006", "unmatched `}` in f-string", self.span(self.pos - 1, self.pos))
                        .help("write `\\}` for a literal brace"));
                }
                Some(c) => text.push(c),
            }
        }
        if !text.is_empty() {
            parts.push(FPart::Lit(text));
        }
        Ok(parts)
    }

    fn number(&mut self, start: usize) -> Result<(), Diagnostic> {
        let digits = |lx: &mut Self| {
            while lx.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                lx.bump();
            }
        };
        digits(self);
        let is_float = self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit());
        if is_float {
            self.bump();
            digits(self);
            let text = self.src[start..self.pos].replace('_', "");
            let value = text.parse::<f64>().map_err(|_| self.bad_number(start))?;
            self.push(Tok::Float(value), start);
            return Ok(());
        }
        if !self.peek().is_some_and(|c| c.is_alphabetic()) {
            let text = self.src[start..self.pos].replace('_', "");
            let value = text.parse::<i64>().map_err(|_| {
                Diagnostic::error("E0007", "integer literal is too large", self.span(start, self.pos))
                    .label("the largest int is 9223372036854775807")
            })?;
            self.push(Tok::Int(value), start);
            return Ok(());
        }
        // A duration: one or more <number><unit> pairs, like `1h30m`.
        let mut total: i64 = 0;
        let mut pair_start = start;
        loop {
            let n: i64 = self.src[pair_start..self.pos].replace('_', "").parse().map_err(|_| self.bad_number(start))?;
            let unit_start = self.pos;
            while self.peek().is_some_and(|c| c.is_alphabetic()) {
                self.bump();
            }
            let unit = &self.src[unit_start..self.pos];
            let ms = match unit {
                "ms" => 1,
                "s" => 1_000,
                "m" => 60_000,
                "h" => 3_600_000,
                "d" => 86_400_000,
                _ => {
                    return Err(Diagnostic::error(
                        "E0008",
                        format!("invalid suffix `{unit}` on a number"),
                        self.span(unit_start, self.pos),
                    )
                    .label("expected a duration unit")
                    .help("duration units are `ms`, `s`, `m`, `h` and `d`, e.g. `30s` or `1h30m`"));
                }
            };
            total = n.checked_mul(ms).and_then(|v| total.checked_add(v)).ok_or_else(|| {
                Diagnostic::error("E0007", "duration literal is too large", self.span(start, self.pos))
            })?;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                break;
            }
            pair_start = self.pos;
            digits(self);
        }
        self.push(Tok::Duration(total), start);
        Ok(())
    }

    fn bad_number(&self, start: usize) -> Diagnostic {
        Diagnostic::error("E0007", "invalid number", self.span(start, self.pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn indentation_becomes_indent_and_dedent() {
        let toks = kinds("worker a:\n    x = 1\n\n    # note\n    y = 2\nlet z = 3\n");
        assert_eq!(
            toks,
            [
                Tok::Kw(Kw::Worker),
                Tok::Ident("a".into()),
                Tok::Op(":"),
                Tok::Newline,
                Tok::Indent,
                Tok::Ident("x".into()),
                Tok::Op("="),
                Tok::Int(1),
                Tok::Newline,
                Tok::Ident("y".into()),
                Tok::Op("="),
                Tok::Int(2),
                Tok::Newline,
                Tok::Dedent,
                Tok::Kw(Kw::Let),
                Tok::Ident("z".into()),
                Tok::Op("="),
                Tok::Int(3),
                Tok::Newline,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn newlines_inside_brackets_are_ignored() {
        let toks = kinds("x = [\n  1,\n  2,\n]\n");
        assert!(!toks[3..toks.len() - 2].contains(&Tok::Newline), "{toks:?}");
    }

    #[test]
    fn literals() {
        assert_eq!(kinds("1h30m")[0], Tok::Duration(5_400_000));
        assert_eq!(kinds("250ms")[0], Tok::Duration(250));
        assert_eq!(kinds("1.5")[0], Tok::Float(1.5));
        assert_eq!(kinds("1_000")[0], Tok::Int(1000));
        assert_eq!(kinds(r#""a\tb""#)[0], Tok::Str("a\tb".into()));
        let Tok::FStr(parts) = &kinds(r#"f"exit {status.exit}!""#)[0] else { panic!() };
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[1], FPart::Expr(t, _) if t.len() == 4));
    }

    #[test]
    fn errors_point_at_the_problem() {
        assert_eq!(lex("x = 5y").unwrap_err().code, "E0008");
        assert_eq!(lex("x = \"open").unwrap_err().code, "E0005");
        assert_eq!(lex("a:\n  b\n\tc\n").unwrap_err().code, "E0001");
        assert_eq!(lex("a:\n    b\n  c\n").unwrap_err().code, "E0002");
        assert_eq!(lex("x = $").unwrap_err().code, "E0003");
        assert_eq!(lex("f\"{}\"").unwrap_err().code, "E0006");
        // Whitespace on a blank line is not indentation.
        assert!(lex("a:\n\tb\n  \n\tc\n").is_ok());
        // Strings inside f-string braces.
        assert!(lex("f\"{get(m, \"k\", \"d\")}!\"").is_ok());
    }
}
