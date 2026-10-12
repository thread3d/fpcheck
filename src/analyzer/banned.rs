//! P0702 `with-statement` and P0703 `goto` (spec section 6.2).
//!
//! `with` is banned outright: it makes name resolution depend on the type of an
//! expression, which is exactly the information an agent (or reader) does not
//! have locally. `goto` is a warning for the same reason, less absolutely.

use crate::analyzer::LintContext;
use crate::diagnostic::{Diagnostic, Level};
use crate::lexer::TokKind;

pub fn with_statement(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    ctx.tokens
        .iter()
        .filter(|t| t.kind == TokKind::Keyword && t.eq_ignore_case("with"))
        .map(|t| {
            Diagnostic::lint(
                "P0702",
                Level::Error,
                "`with` is banned",
                t.span.clone(),
            )
            .with_help("bind the target to a local variable and qualify field accesses explicitly")
            .with_note("`with` hides which object a name belongs to")
        })
        .collect()
}

pub fn goto(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    ctx.tokens
        .iter()
        .filter(|t| t.kind == TokKind::Keyword && t.eq_ignore_case("goto"))
        .map(|t| {
            Diagnostic::lint("P0703", Level::Warning, "`goto` jumps out of the local flow", t.span.clone())
                .with_help("restructure with `break`, `continue`, `exit`, or a nested routine")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::{analyze_source, AnalysisOptions};
    use crate::source::SourceFile;

    fn run(text: &str) -> Vec<Diagnostic> {
        analyze_source(&SourceFile::new("t.pas", text), &AnalysisOptions::default()).diagnostics
    }

    fn code(text: &str, code: &str) -> Vec<Diagnostic> {
        run(text).into_iter().filter(|d| d.code == code).collect()
    }

    #[test]
    fn with_is_an_error() {
        let diags = code("begin\n  with Foo do Bar;\nend.\n", "P0702");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, Level::Error);
        assert_eq!(diags[0].primary.line_start, 2);
    }

    #[test]
    fn with_inside_a_statement_is_found() {
        assert_eq!(code("begin\n  if a then with B do C;\nend.\n", "P0702").len(), 1);
    }

    #[test]
    fn nested_with_is_reported_once_per_occurrence() {
        let text = "begin\n  with A do\n    with B do C;\nend.\n";
        assert_eq!(code(text, "P0702").len(), 2);
    }

    #[test]
    fn a_escaped_with_is_an_identifier_not_the_keyword() {
        assert!(code("begin\n  &with := 1;\nend.\n", "P0702").is_empty());
    }

    #[test]
    fn with_in_a_comment_or_string_is_ignored() {
        assert!(code("{ with Foo do }\nbegin\n  x := 'with Foo';\nend.\n", "P0702").is_empty());
    }

    #[test]
    fn goto_is_a_warning() {
        let diags = code("label done;\nbegin\n  goto done;\n  done:\nend.\n", "P0703");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, Level::Warning);
    }

    #[test]
    fn the_goto_message_and_help_are_actionable() {
        let diags = code("begin\n  goto done;\nend.\n", "P0703");
        assert!(!diags[0].help.is_empty());
        assert!(diags[0].help[0].contains("break"), "{:?}", diags[0].help);
    }

    #[test]
    fn label_declarations_alone_are_not_flagged() {
        // Only the jump is reported; the `label` section keyword is not noise.
        assert!(code("label done;\nbegin\nend.\n", "P0703").is_empty());
    }

    #[test]
    fn clean_code_is_clean() {
        assert!(run("begin\n  Foo.Bar := 1;\nend.\n").is_empty());
    }
}
