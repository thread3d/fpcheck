//! Parsing FPC's output into structured messages (spec section 4.2 step 1).
//!
//! FPC prints `file(line,col) Severity: text` — a *point*, not a span — mixed
//! with banners and progress lines. This module is deliberately tolerant: real
//! output varies between versions and platforms, and a line we cannot classify
//! must never abort the run.

use crate::diagnostic::Level;
use std::path::PathBuf;

/// What kind of line this was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKind {
    /// `Error:` / `Warning:` / `Note:` / `Hint:` with or without a location.
    Diagnostic,
    /// `Fatal:` — compilation stops.
    Fatal,
    /// `Free Pascal Compiler version ...`, `Copyright ...`, `Target OS: ...`.
    Banner,
    /// `Compiling x.pas`, `Linking`, `N lines compiled`.
    Progress,
    /// Anything else; kept so nothing is silently lost.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFpcMessage {
    pub kind: RawKind,
    /// `None` only for lines with no recognizable severity.
    pub severity: Option<Level>,
    pub file: Option<PathBuf>,
    /// 0 when the compiler gave no column.
    pub line: u32,
    pub col: u32,
    /// A numeric message id, when the compiler printed one.
    pub fpc_id: Option<String>,
    /// The message text, with the location and severity stripped.
    pub text: String,
    /// The original line, preserved verbatim for `fpc_raw`.
    pub raw: String,
}

impl RawFpcMessage {
    fn new(kind: RawKind, raw: &str) -> Self {
        RawFpcMessage {
            kind,
            severity: None,
            file: None,
            line: 0,
            col: 0,
            fpc_id: None,
            text: String::new(),
            raw: raw.to_string(),
        }
    }

    /// A location-bearing or bare-severity diagnostic worth turning into output.
    pub fn is_diagnostic(&self) -> bool {
        matches!(self.kind, RawKind::Diagnostic | RawKind::Fatal)
    }

    /// FPC's closing summary, which is not itself a diagnostic.
    ///
    /// Two forms exist:
    ///
    /// * `Fatal: (1018) Compilation aborted`
    /// * `demo.pas(17) Fatal: (10026) There were 4 errors compiling module, stopping`
    ///
    /// The second reports a **basename** even under `-vb`, so keeping it would
    /// put an unresolvable path into the results as well as duplicating a count
    /// the individual diagnostics already carry.
    pub fn is_compile_summary(&self) -> bool {
        if self.kind != RawKind::Fatal {
            return false;
        }
        let text = self.text.trim();
        text.eq_ignore_ascii_case("compilation aborted")
            || (text.starts_with("There were") && text.contains("errors compiling module"))
    }
}

/// Severity prefixes, longest first so `Fatal:` is not read as `Fatal`+junk.
const SEVERITIES: &[(&str, Level, RawKind)] = &[
    ("Fatal:", Level::Error, RawKind::Fatal),
    ("Error:", Level::Error, RawKind::Diagnostic),
    ("Warning:", Level::Warning, RawKind::Diagnostic),
    ("Note:", Level::Note, RawKind::Diagnostic),
    ("Hint:", Level::Help, RawKind::Diagnostic),
];

/// Parse an entire compiler run's merged output.
pub fn parse_output(output: &str) -> Vec<RawFpcMessage> {
    output.lines().map(parse_line).collect()
}

/// Parse one line.
pub fn parse_line(line: &str) -> RawFpcMessage {
    let line = line.trim_end_matches(['\r', '\n']);

    if line.trim().is_empty() {
        return RawFpcMessage::new(RawKind::Other, line);
    }

    // 1. `file(line,col) Severity: text`
    if let Some((file, line_no, col, rest)) = split_location(line) {
        if let Some((severity, kind, text)) = parse_severity(rest) {
            let (fpc_id, text) = split_message_id(text);
            let mut msg = RawFpcMessage::new(kind, line);
            msg.severity = Some(severity);
            msg.file = Some(file);
            msg.line = line_no;
            msg.col = col;
            msg.fpc_id = fpc_id;
            msg.text = text;
            return msg;
        }
    }

    // 2. A bare severity with no location, e.g. `Fatal: Compilation aborted`.
    if let Some((severity, kind, text)) = parse_severity(line) {
        let (fpc_id, text) = split_message_id(text);
        let mut msg = RawFpcMessage::new(kind, line);
        msg.severity = Some(severity);
        msg.fpc_id = fpc_id;
        msg.text = text;
        return msg;
    }

    // 3. Banner and progress noise.
    let trimmed = line.trim();
    if trimmed.starts_with("Free Pascal Compiler version")
        || trimmed.starts_with("Copyright (c)")
        || trimmed.starts_with("Target OS:")
        || trimmed.starts_with("Compiler version")
    {
        return RawFpcMessage::new(RawKind::Banner, line);
    }
    if trimmed.starts_with("Compiling ")
        || trimmed.starts_with("Assembling ")
        || trimmed.starts_with("Linking ")
        || trimmed.starts_with("Closing ")
        || trimmed.starts_with("Searching ")
        || trimmed.contains("lines compiled")
        || trimmed.ends_with("second(s)")
    {
        return RawFpcMessage::new(RawKind::Progress, line);
    }

    RawFpcMessage::new(RawKind::Other, line)
}

/// Split `path(line)` or `path(line,col)` off the front of a line.
///
/// Scans parentheses from the left and takes the first whose contents parse as
/// `N` or `N,N`. Scanning from the left matters: message text frequently
/// contains parentheses of its own (`Error: (5025) ...`, `(something) went
/// wrong`), while a file name containing parentheses is rare and still works
/// because the bogus group simply fails to parse.
fn split_location(line: &str) -> Option<(PathBuf, u32, u32, &str)> {
    let mut search = 0usize;
    while let Some(open_rel) = line[search..].find('(') {
        let open = search + open_rel;
        let close_rel = line[open..].find(')')?;
        let close = open + close_rel;
        let inside = &line[open + 1..close];
        if let Some((line_no, col)) = parse_line_col(inside) {
            let file = line[..open].trim();
            if !file.is_empty() {
                return Some((PathBuf::from(file), line_no, col, &line[close + 1..]));
            }
        }
        search = open + 1;
    }
    None
}

/// Parse `N` or `N,N` into `(line, col)`, where a missing column is `0`.
fn parse_line_col(inside: &str) -> Option<(u32, u32)> {
    let (line_part, col_part) = match inside.split_once(',') {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (inside.trim(), None),
    };
    if line_part.is_empty() {
        return None;
    }
    let line_no: u32 = line_part.parse().ok()?;
    let col: u32 = match col_part {
        Some(c) => {
            if c.is_empty() {
                return None;
            }
            c.parse().ok()?
        }
        None => 0,
    };
    Some((line_no, col))
}

/// Recognize a severity prefix, case-insensitively.
fn parse_severity(rest: &str) -> Option<(Level, RawKind, &str)> {
    let rest = rest.trim_start();
    for (prefix, level, kind) in SEVERITIES {
        if rest.len() >= prefix.len() && rest[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return Some((*level, *kind, rest[prefix.len()..].trim_start()));
        }
    }
    None
}

/// Pull a leading `(1234) ` message id off the text, when present.
fn split_message_id(text: &str) -> (Option<String>, String) {
    let t = text.trim_start();
    if let Some(after_open) = t.strip_prefix('(') {
        if let Some(close) = after_open.find(')') {
            let inside = &after_open[..close];
            if !inside.is_empty() && inside.chars().all(|c| c.is_ascii_digit()) {
                let remainder = after_open[close + 1..].trim_start();
                return (Some(inside.to_string()), remainder.to_string());
            }
        }
    }
    (None, text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_located_error() {
        let m = parse_line("src/lexer.pas(84,5) Error: Identifier not found \"tkString\"");
        assert_eq!(m.kind, RawKind::Diagnostic);
        assert_eq!(m.severity, Some(Level::Error));
        assert_eq!(
            m.file.as_deref(),
            Some(std::path::Path::new("src/lexer.pas"))
        );
        assert_eq!((m.line, m.col), (84, 5));
        assert_eq!(m.text, "Identifier not found \"tkString\"");
        assert_eq!(
            m.raw,
            "src/lexer.pas(84,5) Error: Identifier not found \"tkString\""
        );
    }

    #[test]
    fn parses_every_severity_word() {
        assert_eq!(
            parse_line("a.pas(1,1) Warning: w").severity,
            Some(Level::Warning)
        );
        assert_eq!(parse_line("a.pas(1,1) Note: n").severity, Some(Level::Note));
        assert_eq!(parse_line("a.pas(1,1) Hint: h").severity, Some(Level::Help));
        assert_eq!(parse_line("a.pas(1,1) Fatal: f").kind, RawKind::Fatal);
        assert_eq!(
            parse_line("a.pas(1,1) Fatal: f").severity,
            Some(Level::Error)
        );
    }

    #[test]
    fn severity_word_is_case_insensitive() {
        assert_eq!(
            parse_line("a.pas(1,1) error: e").severity,
            Some(Level::Error)
        );
    }

    #[test]
    fn parses_a_line_without_a_column() {
        let m = parse_line("a.pas(12) Error: something");
        assert_eq!((m.line, m.col), (12, 0));
        assert_eq!(m.text, "something");
    }

    #[test]
    fn parses_a_bare_fatal_with_no_location() {
        let m = parse_line("Fatal: Compilation aborted");
        assert_eq!(m.kind, RawKind::Fatal);
        assert!(m.file.is_none());
        assert!(m.is_compile_summary());
    }

    #[test]
    fn compile_summaries_are_recognized_and_real_fatals_are_diagnostics() {
        assert!(parse_line("Fatal: Compilation aborted").is_compile_summary());
        assert!(parse_line(
            "a.pas(9) Fatal: (10026) There were 3 errors compiling module, stopping"
        )
        .is_compile_summary());
        let other = parse_line("Fatal: Cannot open file \"x\"");
        assert!(other.is_diagnostic());
        assert!(!other.is_compile_summary());
    }

    #[test]
    fn a_non_fatal_message_is_never_a_summary() {
        assert!(!parse_line("a.pas(1,1) Error: There were problems").is_compile_summary());
    }

    #[test]
    fn extracts_a_numeric_message_id_when_present() {
        let m = parse_line("a.pas(3,4) Error: (5025) Identifier not found \"x\"");
        assert_eq!(m.fpc_id.as_deref(), Some("5025"));
        assert_eq!(m.text, "Identifier not found \"x\"");
    }

    #[test]
    fn parenthesised_text_that_is_not_an_id_is_kept() {
        let m = parse_line("a.pas(3,4) Error: (something) went wrong");
        assert_eq!(m.fpc_id, None);
        assert_eq!(m.text, "(something) went wrong");
    }

    #[test]
    fn file_names_with_spaces_and_parens_survive() {
        let m = parse_line("my dir/my (odd) unit.pas(7,2) Error: boom");
        assert_eq!(
            m.file.as_deref(),
            Some(std::path::Path::new("my dir/my (odd) unit.pas"))
        );
        assert_eq!(m.text, "boom");
    }

    #[test]
    fn classifies_banner_and_progress_lines() {
        assert_eq!(
            parse_line("Free Pascal Compiler version 3.2.2 [2021/05/16] for x86_64").kind,
            RawKind::Banner
        );
        assert_eq!(
            parse_line("Copyright (c) 1993-2021 by Florian Klaempfl and others").kind,
            RawKind::Banner
        );
        assert_eq!(
            parse_line("Target OS: Darwin for x86_64").kind,
            RawKind::Banner
        );
        assert_eq!(
            parse_line("Compiling src/lexer.pas").kind,
            RawKind::Progress
        );
        assert_eq!(parse_line("Linking mdiscd").kind, RawKind::Progress);
        assert_eq!(
            parse_line("12 lines compiled, 0.1 sec").kind,
            RawKind::Progress
        );
    }

    #[test]
    fn unclassifiable_lines_are_kept_as_other() {
        let m = parse_line("something unexpected happened");
        assert_eq!(m.kind, RawKind::Other);
        assert_eq!(m.raw, "something unexpected happened");
    }

    #[test]
    fn blank_lines_are_other_and_do_not_panic() {
        assert_eq!(parse_line("").kind, RawKind::Other);
        assert_eq!(parse_line("   ").kind, RawKind::Other);
    }

    #[test]
    fn carriage_returns_are_stripped() {
        let m = parse_line("a.pas(1,1) Error: boom\r");
        assert_eq!(m.text, "boom");
        assert!(!m.raw.ends_with('\r'));
    }

    #[test]
    fn full_run_output_is_parsed_in_order() {
        let text = "\
Free Pascal Compiler version 3.2.2 [2021/05/16] for x86_64
Copyright (c) 1993-2021 by Florian Klaempfl and others
Target OS: Darwin for x86_64
Compiling hello.pas
hello.pas(7,3) Error: Identifier not found \"x\"
hello.pas(9,1) Fatal: There were 1 errors compiling module, stopping
Fatal: Compilation aborted
";
        let msgs = parse_output(text);
        assert_eq!(msgs.len(), 7);
        assert_eq!(msgs[0].kind, RawKind::Banner);
        assert_eq!(msgs[3].kind, RawKind::Progress);
        assert_eq!(msgs[4].kind, RawKind::Diagnostic);
        assert_eq!(msgs[5].kind, RawKind::Fatal);
        assert!(msgs[6].is_compile_summary());
    }

    #[test]
    fn a_line_with_a_location_but_no_severity_is_other() {
        let m = parse_line("a.pas(1,1) just some note text");
        assert_eq!(m.kind, RawKind::Other);
    }
}
