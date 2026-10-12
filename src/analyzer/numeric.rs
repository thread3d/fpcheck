//! P0306 `float-equality` (spec section 6.2).
//!
//! Deliberately narrow. Without a type oracle the analyzer cannot know that an
//! arbitrary `a = b` compares floats, so it reports an exact comparison only
//! when it can see a floating-point operand *and* the comparison is part of a
//! control-flow condition. That keeps `const Pi = 3.14;` and default parameter
//! values out of the results.

use crate::analyzer::LintContext;
use crate::diagnostic::{Diagnostic, Level};
use crate::lexer::{TokKind, Token};
use std::collections::HashSet;

const FLOAT_TYPES: &[&str] = &["single", "double", "real", "extended", "currency", "comp"];

pub fn float_equality(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    let float_names = float_typed_names(ctx.tokens);
    let mut out = Vec::new();

    for (index, token) in ctx.tokens.iter().enumerate() {
        if token.kind != TokKind::Symbol || (token.text != "=" && token.text != "<>") {
            continue;
        }
        if !in_condition(ctx.tokens, index) {
            continue;
        }
        let left = previous_significant(ctx.tokens, index);
        let right = next(ctx.tokens, index + 1).map(|(_, t)| t);

        let offending = [left, right].into_iter().flatten().find(|t| {
            is_float_literal(t)
                || (t.kind == TokKind::Ident && float_names.contains(&t.text.to_ascii_lowercase()))
        });

        let Some(offending) = offending else { continue };

        out.push(
            Diagnostic::lint(
                "P0306",
                Level::Warning,
                format!(
                    "floating-point value `{}` compared with `{}`",
                    offending.text, token.text
                ),
                token.span.clone(),
            )
            .with_label(offending.span.clone(), "floating-point operand")
            .with_help("compare with an epsilon, e.g. `Abs(a - b) < 1e-9`")
            .with_note("binary floating point cannot represent most decimal values exactly"),
        );
    }

    out
}

/// Names declared with a floating-point type anywhere in the file.
fn float_typed_names(tokens: &[Token]) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut index = 0usize;
    while index < tokens.len() {
        // `name : Type` where Type is a float type.
        if tokens[index].kind == TokKind::Ident {
            if let Some((colon_index, colon)) = next(tokens, index + 1) {
                if colon.text == ":" {
                    if let Some((_, type_token)) = next(tokens, colon_index + 1) {
                        if type_token.kind == TokKind::Ident
                            && FLOAT_TYPES.iter().any(|f| type_token.eq_ignore_case(f))
                        {
                            names.insert(tokens[index].text.to_ascii_lowercase());
                        }
                    }
                }
            }
        }
        index += 1;
    }
    names
}

fn next(tokens: &[Token], from: usize) -> Option<(usize, &Token)> {
    tokens
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, t)| !t.is_trivia() && t.kind != TokKind::Eof)
}

fn previous_significant(tokens: &[Token], before: usize) -> Option<&Token> {
    tokens[..before]
        .iter()
        .rev()
        .find(|t| !t.is_trivia() && t.kind != TokKind::Eof)
}

/// The comparison must sit in an `if`/`while`/`until` condition, which is what
/// separates a comparison from a constant or type declaration.
fn in_condition(tokens: &[Token], eq_index: usize) -> bool {
    for token in tokens[..eq_index].iter().rev() {
        if token.is_trivia() {
            continue;
        }
        let word = token.text.to_ascii_lowercase();
        if matches!(
            word.as_str(),
            ";" | "begin"
                | "end"
                | "const"
                | "type"
                | "var"
                | "threadvar"
                | "resourcestring"
                | "label"
                | "implementation"
                | "interface"
                | "program"
                | "unit"
                | "uses"
                | "procedure"
                | "function"
                | "constructor"
                | "destructor"
        ) {
            return false;
        }
        if matches!(word.as_str(), "if" | "while" | "until") {
            return true;
        }
    }
    false
}

fn is_float_literal(token: &Token) -> bool {
    if token.kind != TokKind::Number {
        return false;
    }
    let t = token.text.to_ascii_lowercase();
    // Base-prefixed literals never use an exponent, and `$1E` is not 1e.
    if t.starts_with('$') || t.starts_with('&') || t.starts_with('%') {
        return false;
    }
    t.contains('.') || t.contains('e')
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
            .filter(|d| d.code == "P0306")
            .collect()
    }

    #[test]
    fn comparing_against_a_float_literal_warns() {
        let diags = run("begin\n  if x = 0.1 then y := 1;\nend.\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, Level::Warning);
    }

    #[test]
    fn not_equal_against_a_float_literal_warns() {
        assert_eq!(run("begin\n  while x <> 1.5 do y := 1;\nend.\n").len(), 1);
    }

    #[test]
    fn a_declared_float_variable_warns() {
        let text = "var\n  ratio: Double;\nbegin\n  if ratio = limit then y := 1;\nend.\n";
        let diags = run(text);
        assert_eq!(diags.len(), 1, "{:?}", diags);
        assert!(diags[0].message.contains("ratio"), "{}", diags[0].message);
    }

    #[test]
    fn a_constant_declaration_is_not_a_comparison() {
        assert!(run("const\n  Pi = 3.14159;\nbegin\nend.\n").is_empty());
    }

    #[test]
    fn a_default_parameter_value_is_not_a_comparison() {
        assert!(run("procedure P(x: Double = 1.5);\nbegin\nend;\n").is_empty());
    }

    #[test]
    fn an_assignment_is_not_a_comparison() {
        assert!(run("begin\n  x := 1.5;\nend.\n").is_empty());
    }

    #[test]
    fn integer_comparisons_are_ignored() {
        assert!(run("begin\n  if i = 3 then y := 1;\nend.\n").is_empty());
    }

    #[test]
    fn a_float_comparison_outside_a_condition_is_ignored() {
        assert!(run("begin\n  flag := x = 1.5;\nend.\n").is_empty());
    }

    #[test]
    fn until_conditions_are_checked() {
        assert_eq!(
            run("begin\n  repeat\n    y := 1;\n  until x = 0.5;\nend.\n").len(),
            1
        );
    }

    #[test]
    fn the_help_suggests_an_epsilon() {
        let diags = run("begin\n  if x = 0.1 then y := 1;\nend.\n");
        assert!(diags[0].help[0].contains("epsilon"), "{:?}", diags[0].help);
        assert_eq!(diags[0].labels.len(), 1);
    }

    #[test]
    fn a_hex_literal_with_letters_is_not_a_float() {
        assert!(run("begin\n  if i = $1E then y := 1;\nend.\n").is_empty());
    }
}
