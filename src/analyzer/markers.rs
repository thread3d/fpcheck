//! P0902 `todo-marker` (spec section 6.2).
//!
//! `Todo`, `Unreachable`, `Unimplemented` and `Ensure` come from the support
//! unit (spec section 8). A `Todo` that ships means unfinished work shipped, so
//! it warns by default.

use crate::analyzer::{next_significant, LintContext};
use crate::diagnostic::{Diagnostic, Level};
use crate::lexer::TokKind;

/// Marker routines that must not survive to a release.
const MARKERS: &[&str] = &["Todo", "Unreachable", "Unimplemented"];

pub fn todo_marker(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    for (index, token) in ctx.tokens.iter().enumerate() {
        if token.kind != TokKind::Ident {
            continue;
        }
        let Some(name) = MARKERS.iter().find(|m| token.eq_ignore_case(m)) else {
            continue;
        };

        // Only a call counts; a mention in another identifier does not.
        let called = next_significant(ctx.tokens, index + 1)
            .map(|(_, t)| t.kind == TokKind::Symbol && t.text == "(")
            .unwrap_or(false);
        if !called {
            continue;
        }

        out.push(
            Diagnostic::lint(
                "P0902",
                Level::Warning,
                format!("`{name}` marker left in the code"),
                token.span.clone(),
            )
            .with_help("implement it, or remove the marker before shipping")
            .with_note("`Todo` and friends raise, so this path is not finished"),
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::{analyze_source, AnalysisOptions};
    use crate::source::SourceFile;

    fn run(text: &str) -> Vec<Diagnostic> {
        analyze_source(&SourceFile::new("t.pas", text), &AnalysisOptions::default())
            .diagnostics
            .into_iter()
            .filter(|d| d.code == "P0902")
            .collect()
    }

    #[test]
    fn todo_call_is_flagged() {
        let diags = run("begin\n  Todo('wire this up');\nend.\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, Level::Warning);
        assert!(diags[0].message.contains("Todo"), "{}", diags[0].message);
    }

    #[test]
    fn every_marker_name_is_recognized() {
        assert_eq!(run("begin\n  Unreachable('x');\nend.\n").len(), 1);
        assert_eq!(run("begin\n  Unimplemented('x');\nend.\n").len(), 1);
    }

    #[test]
    fn markers_are_case_insensitive_like_pascal() {
        assert_eq!(run("begin\n  todo('x');\nend.\n").len(), 1);
        assert_eq!(run("begin\n  TODO('x');\nend.\n").len(), 1);
    }

    #[test]
    fn ensure_is_not_a_marker() {
        // Ensure is a legitimate assertion helper (spec section 8).
        assert!(run("begin\n  Ensure(x > 0, 'positive');\nend.\n").is_empty());
    }

    #[test]
    fn a_bare_identifier_is_not_a_call() {
        assert!(run("var\n  Todo: Integer;\nbegin\n  Todo := 1;\nend.\n").is_empty());
    }

    #[test]
    fn names_containing_a_marker_are_not_matches() {
        assert!(run("begin\n  TodoLater('x');\nend.\n").is_empty());
        assert!(run("begin\n  MyTodo('x');\nend.\n").is_empty());
    }

    #[test]
    fn comments_and_strings_do_not_trigger() {
        assert!(run("begin\n  { Todo('x') }\n  s := 'Todo(''x'')';\nend.\n").is_empty());
    }

    #[test]
    fn whitespace_between_name_and_paren_still_counts() {
        assert_eq!(run("begin\n  Todo ('x');\nend.\n").len(), 1);
    }

    #[test]
    fn each_call_is_reported() {
        assert_eq!(run("begin\n  Todo('a');\n  Todo('b');\nend.\n").len(), 2);
    }
}
