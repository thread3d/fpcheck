//! The single diagnostic model, rendered many ways (spec section 5).
//!
//! Every producer — the FPC message parser and the analyzer alike — builds
//! these values. Renderers never invent information; anything a renderer shows
//! is present here.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A source region. Lines and columns are 1-based; columns count characters
/// (FPC counts bytes, which is identical for ASCII source).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub file: PathBuf,
    pub line_start: u32,
    pub col_start: u32,
    pub line_end: u32,
    pub col_end: u32,
}

impl Span {
    pub fn new(
        file: impl Into<PathBuf>,
        line_start: u32,
        col_start: u32,
        line_end: u32,
        col_end: u32,
    ) -> Self {
        Span {
            file: file.into(),
            line_start,
            col_start,
            line_end,
            col_end,
        }
    }

    /// A zero-width position, as FPC reports. Callers expand this to a real
    /// span via the lexer (spec section 4.2 step 2).
    pub fn point(file: impl Into<PathBuf>, line: u32, col: u32) -> Self {
        Span::new(file, line, col, line, col)
    }

    pub fn is_empty(&self) -> bool {
        self.line_start == self.line_end && self.col_start == self.col_end
    }

    pub fn lines(&self) -> u32 {
        self.line_end.saturating_sub(self.line_start) + 1
    }

    /// Smallest span covering both, or `None` when the files differ.
    pub fn union(&self, other: &Span) -> Option<Span> {
        if self.file != other.file {
            return None;
        }
        let (line_start, col_start) =
            if (self.line_start, self.col_start) <= (other.line_start, other.col_start) {
                (self.line_start, self.col_start)
            } else {
                (other.line_start, other.col_start)
            };
        let (line_end, col_end) =
            if (self.line_end, self.col_end) >= (other.line_end, other.col_end) {
                (self.line_end, self.col_end)
            } else {
                (other.line_end, other.col_end)
            };
        Some(Span::new(
            self.file.clone(),
            line_start,
            col_start,
            line_end,
            col_end,
        ))
    }

    pub fn file_name(&self) -> String {
        self.file
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file.to_string_lossy().into_owned())
    }

    /// `file:line:col`, the form the spec requires in every renderer.
    pub fn location(&self) -> String {
        format!(
            "{}:{}:{}",
            self.file.display(),
            self.line_start,
            self.col_start
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Note,
    Help,
}

impl Level {
    /// Sort key: errors first, so root causes are reported before noise.
    pub fn rank(self) -> u8 {
        match self {
            Level::Error => 0,
            Level::Warning => 1,
            Level::Note => 2,
            Level::Help => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Note => "note",
            Level::Help => "help",
        }
    }

    /// Parse the severity word FPC prints.
    pub fn from_fpc(word: &str) -> Option<Level> {
        match word.to_ascii_lowercase().as_str() {
            "error" | "fatal" => Some(Level::Error),
            "warning" => Some(Level::Warning),
            "note" => Some(Level::Note),
            "hint" => Some(Level::Help),
            _ => None,
        }
    }
}

/// Same semantics as rustc (spec section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Applicability {
    MachineApplicable,
    MaybeIncorrect,
    HasPlaceholders,
    Unspecified,
}

impl Applicability {
    pub fn is_safe_to_apply(self) -> bool {
        matches!(self, Applicability::MachineApplicable)
    }

    /// Both `MachineApplicable` and `MaybeIncorrect` suggestions are rendered
    /// as diffs in `--message-format=llm` (spec section 5.3).
    pub fn renders_as_diff(self) -> bool {
        matches!(
            self,
            Applicability::MachineApplicable | Applicability::MaybeIncorrect
        )
    }
}

/// A secondary span with a short caption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub span: Span,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub applicability: Applicability,
    pub message: String,
    pub edits: Vec<Edit>,
}

impl Suggestion {
    pub fn machine(message: impl Into<String>, edits: Vec<Edit>) -> Self {
        Suggestion {
            applicability: Applicability::MachineApplicable,
            message: message.into(),
            edits,
        }
    }
}

/// Where a diagnostic came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Fpc,
    Fpcheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub level: Level,
    pub message: String,
    pub primary: Span,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub help: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Diagnostic>,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fpc_raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain: Option<String>,
}

impl Diagnostic {
    pub fn new(
        code: impl Into<String>,
        level: Level,
        message: impl Into<String>,
        primary: Span,
        source: Source,
    ) -> Self {
        let code = code.into();
        Diagnostic {
            explain: Some(format!("fpcheck explain {}", code)),
            code,
            level,
            message: message.into(),
            primary,
            labels: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            suggestions: Vec::new(),
            related: Vec::new(),
            source,
            fpc_raw: None,
        }
    }

    /// An analyzer lint, the common case.
    pub fn lint(
        code: impl Into<String>,
        level: Level,
        message: impl Into<String>,
        primary: Span,
    ) -> Self {
        Diagnostic::new(code, level, message, primary, Source::Fpcheck)
    }

    pub fn with_label(mut self, span: Span, text: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            text: text.into(),
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    pub fn with_suggestion(mut self, s: Suggestion) -> Self {
        self.suggestions.push(s);
        self
    }

    pub fn with_related(mut self, d: Diagnostic) -> Self {
        self.related.push(d);
        self
    }

    /// True when a suggestion can be applied by `fpcheck fix` without review.
    pub fn machine_applicable(&self) -> impl Iterator<Item = &Suggestion> {
        self.suggestions
            .iter()
            .filter(|s| s.applicability.is_safe_to_apply())
    }

    /// The span a renderer should highlight: the widest of the primary span and
    /// any label span on the same file, so a caret is never smaller than the
    /// point FPC gave us.
    pub fn headline_span(&self) -> &Span {
        &self.primary
    }

    pub fn file(&self) -> &Path {
        &self.primary.file
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d() -> Diagnostic {
        Diagnostic::lint(
            "P0702",
            Level::Error,
            "`with` is banned",
            Span::new("a.pas", 3, 5, 3, 9),
        )
    }

    #[test]
    fn explain_hint_is_generated_from_code() {
        assert_eq!(d().explain.as_deref(), Some("fpcheck explain P0702"));
    }

    #[test]
    fn location_is_file_line_col() {
        assert_eq!(d().primary.location(), "a.pas:3:5");
    }

    #[test]
    fn union_of_same_file_covers_both() {
        let a = Span::point("a.pas", 2, 9);
        let b = Span::new("a.pas", 2, 3, 4, 1);
        let u = a.union(&b).unwrap();
        assert_eq!(
            (u.line_start, u.col_start, u.line_end, u.col_end),
            (2, 3, 4, 1)
        );
    }

    #[test]
    fn union_across_files_is_none() {
        let a = Span::point("a.pas", 1, 1);
        let b = Span::point("b.pas", 1, 1);
        assert!(a.union(&b).is_none());
    }

    #[test]
    fn levels_sort_errors_first() {
        let mut v = vec![Level::Help, Level::Error, Level::Warning, Level::Note];
        v.sort_by_key(|l| l.rank());
        assert_eq!(
            v,
            vec![Level::Error, Level::Warning, Level::Note, Level::Help]
        );
    }

    #[test]
    fn fpc_severity_words_map() {
        assert_eq!(Level::from_fpc("Fatal"), Some(Level::Error));
        assert_eq!(Level::from_fpc("Hint"), Some(Level::Help));
        assert_eq!(Level::from_fpc("nonsense"), None);
    }

    #[test]
    fn json_uses_spec_field_names_and_level_spelling() {
        let json = serde_json::to_string(&d()).unwrap();
        assert!(json.contains("\"level\":\"error\""), "{}", json);
        assert!(json.contains("\"code\":\"P0702\""), "{}", json);
        assert!(json.contains("\"line_start\":3"), "{}", json);
        assert!(json.contains("\"source\":\"fpcheck\""), "{}", json);
        // Empty vectors stay out of the wire format.
        assert!(!json.contains("\"labels\""), "{}", json);
    }

    #[test]
    fn applicability_survives_round_trip() {
        let s = Suggestion {
            applicability: Applicability::MachineApplicable,
            message: "add else".into(),
            edits: vec![Edit {
                span: Span::point("a.pas", 1, 1),
                replacement: "else".into(),
            }],
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"MachineApplicable\""), "{}", json);
        let back: Suggestion = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
