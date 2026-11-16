//! Compiler errors, rendered the way rustc renders them: a code, the file,
//! line and column, the source line and a caret under the problem.

use std::fmt::Write;

/// A byte range in the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end: end.max(start) }
    }

    pub fn to(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
    /// Shown under the caret.
    pub label: Option<String>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self { code, message: message.into(), span, label: None, notes: Vec::new(), helps: Vec::new() }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }

    /// 1-based line and column (in characters) of the span start.
    pub fn line_col(&self, src: &str) -> (usize, usize) {
        let start = self.span.start.min(src.len());
        let before = &src[..start];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        (line, col)
    }

    /// One line: `file.awl:4:15: error[E0308]: mismatched types: expected ...`.
    pub fn short(&self, file: &str, src: &str) -> String {
        let (line, col) = self.line_col(src);
        let label = self.label.as_deref().map(|l| format!(": {l}")).unwrap_or_default();
        format!("{file}:{line}:{col}: error[{}]: {}{label}", self.code, self.message)
    }

    /// The full rustc-style report.
    pub fn render(&self, file: &str, src: &str) -> String {
        let (line, col) = self.line_col(src);
        let text = src.lines().nth(line - 1).unwrap_or("");
        let gutter = line.to_string().len();
        let pad = " ".repeat(gutter);
        let mut out = String::new();
        let _ = writeln!(out, "error[{}]: {}", self.code, self.message);
        let _ = writeln!(out, "{pad}--> {file}:{line}:{col}");
        let _ = writeln!(out, "{pad} |");
        let _ = writeln!(out, "{line} | {text}");
        // The caret covers the span, but never past the end of this line.
        let line_start: usize = src.lines().take(line - 1).map(|l| l.len() + 1).sum();
        let line_end = line_start + text.len();
        let end = self.span.end.min(line_end).max(self.span.start.min(line_end));
        let width = src.get(self.span.start.min(line_end)..end).map_or(1, |s| s.chars().count()).max(1);
        let lead: String =
            text.chars().take(col - 1).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
        let label = self.label.as_deref().map(|l| format!(" {l}")).unwrap_or_default();
        let _ = writeln!(out, "{pad} | {lead}{}{label}", "^".repeat(width));
        if !self.notes.is_empty() || !self.helps.is_empty() {
            let _ = writeln!(out, "{pad} |");
        }
        for note in &self.notes {
            let _ = writeln!(out, "{pad} = note: {note}");
        }
        for help in &self.helps {
            let _ = writeln!(out, "{pad} = help: {help}");
        }
        out
    }
}

/// Every diagnostic of a file, rendered, plus a summary line.
pub fn render_all(diags: &[Diagnostic], file: &str, src: &str) -> String {
    let mut out = String::new();
    for d in diags {
        out.push_str(&d.render(file, src));
        out.push('\n');
    }
    let n = diags.len();
    let _ = writeln!(out, "error: could not compile `{file}` due to {n} previous error{}", if n == 1 { "" } else { "s" });
    out
}

/// Levenshtein distance, for "did you mean" suggestions.
pub fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// The closest candidate, if it is close enough to be a likely typo.
pub fn suggest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .into_iter()
        .map(|c| (distance(word, c), c))
        .filter(|(d, c)| *d <= (word.len().max(c.len()) / 3).max(1))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_like_rustc() {
        let src = "worker a:\n    timeout = \"30s\"\n";
        let start = src.find('"').unwrap();
        let d = Diagnostic::error("E0308", "mismatched types", Span::new(start, start + 5))
            .label("expected `duration`, found `str`")
            .help("write a duration literal like `30s`");
        let text = d.render("a.awl", src);
        assert!(text.contains("--> a.awl:2:15"), "{text}");
        assert!(text.contains("2 |     timeout = \"30s\""), "{text}");
        assert!(text.contains("  |               ^^^^^ expected `duration`, found `str`"), "{text}");
        assert!(text.contains("= help: write a duration"), "{text}");
        assert_eq!(d.short("a.awl", src), "a.awl:2:15: error[E0308]: mismatched types: expected `duration`, found `str`");
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest("timout", ["timeout", "retries"]), Some("timeout"));
        assert_eq!(suggest("zzz", ["timeout", "retries"]), None);
    }
}
