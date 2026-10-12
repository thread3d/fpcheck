//! P0701 `unsafe-outside-unsafe` (spec section 6.2, construct list in 7.2).
//!
//! Only constructs that can be recognized without type information are
//! reported, because a false positive here costs more than a false negative
//! (spec section 7.4). Constructs needing resolution — pointer arithmetic,
//! untyped parameters, hard casts that change managed-ness — are left to the
//! `{@unsafe}` marker on the construct that does know.

use crate::analyzer::{in_unsafe_region, next_significant, LintContext};
use crate::diagnostic::{Diagnostic, Level};
use crate::lexer::{TokKind, Token};

/// Routine calls that bypass the type system.
const UNSAFE_CALLS: &[&str] = &[
    "Addr", "Move", "FillChar", "FillByte", "Pointer", "PtrInt", "PtrUInt", "IntPtr", "UIntPtr",
];

pub fn unsafe_outside_unsafe(ctx: &LintContext<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    for (index, token) in ctx.tokens.iter().enumerate() {
        if in_unsafe_region(ctx.tokens, index) {
            continue;
        }
        let Some(construct) = unsafe_construct(ctx.tokens, index, token) else {
            continue;
        };

        out.push(
            Diagnostic::lint(
                "P0701",
                Level::Error,
                format!("unsafe construct `{construct}` outside an `{{@unsafe}}` region"),
                token.span.clone(),
            )
            .with_help("avoid it, or justify it with `{@unsafe(reason: \"...\")}`")
            .with_note("this construct bypasses the guarantees the analyzer relies on"),
        );
    }

    out
}

fn unsafe_construct(tokens: &[Token], index: usize, token: &Token) -> Option<String> {
    if token.kind == TokKind::Symbol && token.text == "@" {
        // The spec exempts procedural-value assignment (`@Handler := ...`),
        // which is the one common legitimate use of `@`. Anything else is
        // address-of, which requires an unsafe region.
        if is_procedural_assignment(tokens, index) {
            return None;
        }
        return Some("@".to_string());
    }

    if token.kind == TokKind::Keyword {
        if token.eq_ignore_case("absolute") {
            return Some("absolute".to_string());
        }
        if token.eq_ignore_case("asm") {
            return Some("asm".to_string());
        }
    }

    if token.kind == TokKind::Ident {
        if let Some(name) = UNSAFE_CALLS.iter().find(|n| token.eq_ignore_case(n)) {
            let called = next_significant(tokens, index + 1)
                .map(|(_, t)| t.kind == TokKind::Symbol && t.text == "(")
                .unwrap_or(false);
            if called {
                return Some(format!("{name}("));
            }
        }
    }

    None
}

/// `@Name := @Impl` — assignment to a procedural variable, which the spec
/// exempts from the address-of rule.
///
/// Both `@` in such a statement are exempt, so the whole statement is scanned
/// for the `@ Ident :=` shape rather than judging each `@` alone.
fn is_procedural_assignment(tokens: &[Token], index: usize) -> bool {
    let (start, end) = statement_bounds(tokens, index);

    let mut i = start;
    while i < end {
        if tokens[i].kind == TokKind::Symbol && tokens[i].text == "@" {
            if let Some((name_index, name)) = next_significant(tokens, i + 1) {
                if name.kind == TokKind::Ident && name_index < end {
                    if let Some((_, assign)) = next_significant(tokens, name_index + 1) {
                        if assign.text == ":=" {
                            return true;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// Statement range around `index`, delimited by `;`, `begin`, `end`, `else`,
/// `then` or `do`, ignoring separators nested in parentheses or brackets.
fn statement_bounds(tokens: &[Token], index: usize) -> (usize, usize) {
    let mut start = 0usize;
    let mut back = index;
    while back > 0 {
        back -= 1;
        let token = &tokens[back];
        if token.is_trivia() {
            continue;
        }
        let word = token.text.to_ascii_lowercase();
        if matches!(
            word.as_str(),
            ";" | "begin" | "end" | "else" | "then" | "do"
        ) {
            start = back + 1;
            break;
        }
    }

    let mut end = tokens.len();
    let mut depth = 0i32;
    let mut forward = index;
    while forward < tokens.len() {
        let token = &tokens[forward];
        if !token.is_trivia() {
            match token.text.as_str() {
                "(" | "[" => depth += 1,
                ")" | "]" => depth -= 1,
                ";" if depth <= 0 => {
                    end = forward;
                    break;
                }
                _ => {}
            }
            if forward > index && (token.eq_ignore_case("begin") || token.eq_ignore_case("end")) {
                end = forward;
                break;
            }
        }
        forward += 1;
    }

    (start, end)
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
            .filter(|d| d.code == "P0701")
            .collect()
    }

    #[test]
    fn address_of_is_flagged() {
        let diags = run("begin\n  p := @value;\nend.\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, Level::Error);
        assert!(diags[0].message.contains('@'), "{}", diags[0].message);
    }

    #[test]
    fn procedural_value_assignment_is_not_flagged() {
        // Assigning to a procedural variable is the exempted case.
        assert!(run("begin\n  @Handler := @Impl;\nend.\n").is_empty());
    }

    #[test]
    fn unsafe_routine_calls_are_flagged() {
        assert_eq!(run("begin\n  Move(a, b, SizeOf(a));\nend.\n").len(), 1);
        assert_eq!(run("begin\n  FillChar(buf, 10, 0);\nend.\n").len(), 1);
        assert_eq!(run("begin\n  p := Addr(x);\nend.\n").len(), 1);
        assert_eq!(run("begin\n  p := Pointer(q);\nend.\n").len(), 1);
    }

    #[test]
    fn a_matching_type_name_is_not_a_call() {
        // `p: Pointer` declares a pointer, it does not cast.
        assert!(run("var\n  p: Pointer;\nbegin\n  p := nil;\nend.\n").is_empty());
    }

    #[test]
    fn absolute_and_asm_are_flagged() {
        assert_eq!(run("var\n  a: Integer absolute b;\nbegin\nend.\n").len(), 1);
        assert_eq!(run("begin\n  asm\n    nop\n  end;\nend.\n").len(), 1);
    }

    #[test]
    fn an_unsafe_region_permits_everything_inside_it() {
        let text = "{@unsafe(reason: \"dma buffer\")}\nbegin\n  p := @value;\n  Move(a, b, 4);\nend.\n{@end_unsafe}\n";
        assert!(run(text).is_empty());
    }

    #[test]
    fn the_region_does_not_leak_past_its_end() {
        let text = "{@unsafe(reason: \"dma buffer\")}\nbegin\n  p := @value;\nend;\n{@end_unsafe}\nbegin\n  q := @other;\nend.\n";
        assert_eq!(run(text).len(), 1);
    }

    #[test]
    fn the_message_names_the_construct() {
        let diags = run("begin\n  Move(a, b, 4);\nend.\n");
        assert!(diags[0].message.contains("Move("), "{}", diags[0].message);
        assert!(diags[0].help[0].contains("{@unsafe"), "{:?}", diags[0].help);
    }

    #[test]
    fn whitespace_between_name_and_paren_still_counts() {
        assert_eq!(run("begin\n  Move (a, b, 4);\nend.\n").len(), 1);
    }

    #[test]
    fn ordinary_code_is_untouched() {
        assert!(run("begin\n  x := y + 1;\n  p := SomeObject.Create;\nend.\n").is_empty());
    }
}
