//! P0304 `checks-disabled` (spec section 6.2).
//!
//! Turning off `{$R-}`, `{$Q-}` or `{$I-}` silently removes runtime safety, so
//! it is denied outside an `{@unsafe(reason: "...")}` region. Both the short
//! (`{$R-}`) and long (`{$RANGECHECKS OFF}`) spellings are recognized.

use crate::analyzer::{in_unsafe_region, LintContext};
use crate::diagnostic::{Diagnostic, Edit, Level, Span, Suggestion};
use crate::lexer::TokKind;

pub fn checks_disabled(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    for (index, token) in ctx.tokens.iter().enumerate() {
        if token.kind != TokKind::Directive {
            continue;
        }
        let Some((name, arg)) = token.directive_parts() else {
            continue;
        };

        let arg_lower = arg.to_ascii_lowercase();
        let disabled = arg_lower == "-" || arg_lower == "off";
        if !disabled {
            continue;
        }

        let Some(what) = check_name(&name) else {
            continue;
        };
        if in_unsafe_region(ctx.tokens, index) {
            continue;
        }

        let diag = Diagnostic::lint(
            "P0304",
            Level::Error,
            format!("`{}` disables {what}", token.text),
            span_of(token),
        )
        .with_help("remove the directive, or put it inside an `{@unsafe(reason: \"...\")}` region")
        .with_note("runtime checks are the safety net this project relies on")
        .with_suggestion(Suggestion::machine(
            format!("remove `{}` to re-enable {what}", token.text),
            vec![Edit {
                span: token.span.clone(),
                replacement: String::new(),
            }],
        ));
        out.push(diag);
    }

    out
}

/// Which check a directive name refers to, for the message and for filtering.
fn check_name(name: &str) -> Option<&'static str> {
    match name.to_ascii_uppercase().as_str() {
        "R" | "RANGECHECKS" => Some("range checking"),
        "Q" | "OVERFLOWCHECKS" => Some("overflow checking"),
        "I" | "IOCHECKS" => Some("I/O checking"),
        _ => None,
    }
}

fn span_of(token: &crate::lexer::Token) -> Span {
    token.span.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::{analyze_source, AnalysisOptions};
    use crate::diagnostic::Applicability;
    use crate::source::SourceFile;

    fn run(text: &str) -> Vec<Diagnostic> {
        analyze_source(&SourceFile::new("t.pas", text), &AnalysisOptions::default())
            .diagnostics
            .into_iter()
            .filter(|d| d.code == "P0304")
            .collect()
    }

    #[test]
    fn short_forms_are_flagged() {
        assert_eq!(run("{$R-}\nunit a; end.").len(), 1);
        assert_eq!(run("{$Q-}\nunit a; end.").len(), 1);
        assert_eq!(run("{$I-}\nunit a; end.").len(), 1);
    }

    #[test]
    fn long_forms_are_flagged() {
        assert_eq!(run("{$RANGECHECKS OFF}\nunit a; end.").len(), 1);
        assert_eq!(run("{$IOCHECKS OFF}\nunit a; end.").len(), 1);
    }

    #[test]
    fn enabling_forms_are_not_flagged() {
        assert!(run("{$R+}\nunit a; end.").is_empty());
        assert!(run("{$RANGECHECKS ON}\nunit a; end.").is_empty());
        assert!(run("{$Q+}{$I+}\nunit a; end.").is_empty());
    }

    #[test]
    fn unrelated_directives_are_ignored() {
        assert!(run("{$mode objfpc}{$H+}{$IFDEF FOO}\nunit a; end.").is_empty());
    }

    #[test]
    fn an_unsafe_region_permits_it() {
        let text = "{@unsafe(reason: \"raw io\")}\n{$I-}\n{@end_unsafe}\nunit a; end.";
        assert!(run(text).is_empty());
    }

    #[test]
    fn after_the_unsafe_region_ends_it_is_flagged_again() {
        let text = "{@unsafe(reason: \"raw io\")}\n{$I-}\n{@end_unsafe}\n{$Q-}\nunit a; end.";
        assert_eq!(run(text).len(), 1);
    }

    #[test]
    fn the_message_names_the_directive_and_the_check() {
        let diags = run("{$R-}\nunit a; end.");
        assert!(diags[0].message.contains("{$R-}"), "{}", diags[0].message);
        assert!(
            diags[0].message.contains("range checking"),
            "{}",
            diags[0].message
        );
        assert!(!diags[0].help.is_empty());
    }

    #[test]
    fn the_span_covers_the_directive() {
        let diags = run("unit a;\n{$R-}\nend.");
        assert_eq!(diags[0].primary.line_start, 2);
        assert_eq!(diags[0].primary.col_start, 1);
        assert_eq!(diags[0].primary.col_end, 6);
    }

    #[test]
    fn parenthesised_directives_are_recognized_too() {
        assert_eq!(run("(*$R-*)\nunit a; end.").len(), 1);
    }

    #[test]
    fn removing_the_directive_is_a_machine_applicable_fix() {
        let diags = run("{$R-}\nunit a; end.");
        assert_eq!(diags[0].suggestions.len(), 1);
        let suggestion = &diags[0].suggestions[0];
        assert_eq!(suggestion.applicability, Applicability::MachineApplicable);
        assert_eq!(suggestion.edits.len(), 1);
        assert!(suggestion.edits[0].replacement.is_empty());
        assert_eq!(suggestion.edits[0].span.line_start, 1);
    }

    #[test]
    fn the_fix_removes_only_the_directive_text() {
        let diags = run("unit a;\n{$Q-}\nend.");
        let edit = &diags[0].suggestions[0].edits[0];
        assert_eq!((edit.span.line_start, edit.span.col_start), (2, 1));
        assert_eq!(edit.span.col_end, 6);
    }
}
