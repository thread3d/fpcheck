//! P0801 `empty-except` and P0802 `catch-all-except` (spec section 6.2).
//!
//! Both need real block structure, so they share a handler-body scanner that
//! respects nesting of `begin`, `try`, `case`, `repeat`, `record` and `asm`.

use crate::analyzer::LintContext;
use crate::diagnostic::{Diagnostic, Level};
use crate::lexer::{TokKind, Token};

pub fn empty_except(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    for (index, token) in ctx.tokens.iter().enumerate() {
        if !is_except(token) {
            continue;
        }
        let Some((start, end)) = handler_body_range(ctx.tokens, index) else {
            continue;
        };
        if significant_between(ctx.tokens, start, end).is_empty() {
            out.push(
                Diagnostic::lint(
                    "P0801",
                    Level::Error,
                    "`except` block swallows the exception silently",
                    token.span.clone(),
                )
                .with_help("handle the specific exception, log it, or remove the `try..except`")
                .with_note("a silent `except` turns a failure into a wrong answer"),
            );
        }
    }

    out
}

pub fn catch_all_except(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    for (index, token) in ctx.tokens.iter().enumerate() {
        if !is_except(token) {
            continue;
        }
        let Some((start, end)) = handler_body_range(ctx.tokens, index) else {
            continue;
        };
        let body = significant_between(ctx.tokens, start, end);
        // An empty handler is P0801's business, not P0802's.
        if body.is_empty() {
            continue;
        }

        let first = body[0];
        let bare = !first.eq_ignore_case("on");
        let catches_everything = !bare && on_clause_catches_exception(ctx.tokens, start, end);
        if !bare && !catches_everything {
            continue;
        }

        if body.iter().any(|t| t.eq_ignore_case("raise"))
            || body.iter().any(|t| looks_like_logging(t))
        {
            continue;
        }

        out.push(
            Diagnostic::lint(
                "P0802",
                Level::Warning,
                "catch-all `except` without re-raise or logging",
                token.span.clone(),
            )
            .with_help("catch specific exception classes")
            .with_help("or log and re-raise with `raise;`")
            .with_note("`on E: Exception` hides bugs the caller expected to see"),
        );
    }

    out
}

fn is_except(token: &Token) -> bool {
    token.kind == TokKind::Keyword && token.eq_ignore_case("except")
}

/// The handler body runs from just after `except` to the `end` that closes the
/// enclosing `try`. Returns `(first_body_index, end_index)`.
pub(crate) fn handler_body_range(tokens: &[Token], except_index: usize) -> Option<(usize, usize)> {
    let start = except_index + 1;
    let mut depth = 0i32;
    let mut index = start;

    while index < tokens.len() {
        let token = &tokens[index];
        if !token.is_trivia() && token.kind != TokKind::Eof {
            let word = token.text.to_ascii_lowercase();
            match word.as_str() {
                "begin" | "try" | "case" | "repeat" | "record" | "asm" => depth += 1,
                "until" => {
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                "end" => {
                    if depth == 0 {
                        return Some((start, index));
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

fn significant_between(tokens: &[Token], start: usize, end: usize) -> Vec<&Token> {
    tokens[start..end]
        .iter()
        .filter(|t| !t.is_trivia() && t.kind != TokKind::Eof)
        .collect()
}

/// Whether an `on ...` clause names the root `Exception` class.
fn on_clause_catches_exception(tokens: &[Token], start: usize, end: usize) -> bool {
    let mut seen_on = false;
    for token in tokens[start..end].iter() {
        if token.is_trivia() {
            continue;
        }
        if !seen_on {
            if token.eq_ignore_case("on") {
                seen_on = true;
            }
            continue;
        }
        if token.eq_ignore_case("do") {
            return false;
        }
        if token.kind == TokKind::Ident && token.eq_ignore_case("Exception") {
            return true;
        }
    }
    false
}

/// Names that suggest the exception was recorded rather than ignored.
fn looks_like_logging(token: &Token) -> bool {
    if token.kind != TokKind::Ident {
        return false;
    }
    let name = token.text.to_ascii_lowercase();
    name.contains("log")
        || name.contains("trace")
        || name.contains("report")
        || name.starts_with("warn")
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
    fn a_bare_empty_except_is_an_error() {
        let diags = code(
            "begin\n  try\n    Risky;\n  except\n  end;\nend.\n",
            "P0801",
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, Level::Error);
    }

    #[test]
    fn an_except_with_only_a_comment_is_still_empty() {
        let diags = code(
            "begin\n  try\n    Risky;\n  except\n    { nothing }\n  end;\nend.\n",
            "P0801",
        );
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn an_except_with_a_handler_is_not_empty() {
        assert!(code(
            "begin\n  try\n    Risky;\n  except\n    on E: EFoo do Handle(E);\n  end;\nend.\n",
            "P0801"
        )
        .is_empty());
    }

    #[test]
    fn nested_blocks_do_not_confuse_the_handler_scanner() {
        let text = "begin\n  try\n    Risky;\n  except\n    begin\n      case x of\n        1: y := 1;\n      end;\n    end;\n  end;\nend.\n";
        // The handler is not empty, so P0801 stays quiet...
        assert!(code(text, "P0801").is_empty());
        // ...but a bare `except` that neither re-raises nor logs is still a
        // catch-all, even when its body is a compound statement.
        assert_eq!(code(text, "P0802").len(), 1, "{:?}", code(text, "P0802"));
    }

    #[test]
    fn a_bare_except_with_a_body_is_a_catch_all_warning() {
        let diags = code(
            "begin\n  try\n    Risky;\n  except\n    y := 0;\n  end;\nend.\n",
            "P0802",
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, Level::Warning);
    }

    #[test]
    fn on_exception_is_a_catch_all_warning() {
        let diags = code(
            "begin\n  try\n    Risky;\n  except\n    on E: Exception do y := 0;\n  end;\nend.\n",
            "P0802",
        );
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn a_specific_exception_is_not_a_catch_all() {
        assert!(code(
            "begin\n  try\n    Risky;\n  except\n    on E: EMyError do y := 0;\n  end;\nend.\n",
            "P0802"
        )
        .is_empty());
    }

    #[test]
    fn re_raising_silences_the_catch_all_warning() {
        let text = "begin\n  try\n    Risky;\n  except\n    on E: Exception do begin Log(E); raise; end;\n  end;\nend.\n";
        assert!(code(text, "P0802").is_empty(), "{:?}", code(text, "P0802"));
    }

    #[test]
    fn logging_silences_the_catch_all_warning() {
        let text = "begin\n  try\n    Risky;\n  except\n    on E: Exception do LogError(E);\n  end;\nend.\n";
        assert!(code(text, "P0802").is_empty());
    }

    #[test]
    fn an_empty_handler_is_not_also_reported_as_catch_all() {
        let text = "begin\n  try\n    Risky;\n  except\n  end;\nend.\n";
        assert_eq!(code(text, "P0802").len(), 0);
        assert_eq!(code(text, "P0801").len(), 1);
    }

    #[test]
    fn two_handlers_are_reported_separately() {
        let text =
            "begin\n  try\n    A;\n  except\n  end;\n  try\n    B;\n  except\n  end;\nend.\n";
        assert_eq!(code(text, "P0801").len(), 2);
    }

    #[test]
    fn a_nested_try_inside_a_handler_is_scanned_correctly() {
        // The outer handler holds a nested `try`; only the inner, empty handler
        // is empty, and it is the one that must be reported.
        let text = "begin\n  try\n    A;\n  except\n    begin\n      try\n        B;\n      except\n      end;\n    end;\n  end;\nend.\n";
        let diags = code(text, "P0801");
        assert_eq!(diags.len(), 1, "{:?}", diags);
        assert_eq!(diags[0].primary.line_start, 8, "{:?}", diags);
    }

    #[test]
    fn repeat_until_inside_a_handler_does_not_end_it_early() {
        let text =
            "begin\n  try\n    A;\n  except\n    repeat\n      B;\n    until Done;\n  end;\nend.\n";
        assert!(code(text, "P0801").is_empty());
    }
}
