//! The analyzer: high-confidence lints over the token stream (spec section 6).
//!
//! Design rules from the spec that this module enforces:
//!
//! * **Few, precise, high-confidence errors.** Where the analyzer cannot decide,
//!   it stays silent — a false negative beats a false positive (section 7.4).
//! * Cross-routine reasoning uses annotations only, never whole-program
//!   inference, so everything here is intraprocedural and lexical.
//! * Suppression goes through `{@allow(CODE, reason: "...")}` and `reason:` is
//!   mandatory; omitting it is itself an error (P0001, section 6.3).
//! * Default levels live with the lint that declares the code stable, not in the
//!   catalog, so a code is defined in exactly one place.

pub mod banned;
pub mod directives;
pub mod exceptions;
pub mod markers;
pub mod numeric;
pub mod unsafe_;

use crate::catalog::Catalog;
use crate::config::{LevelSetting, Strictness};
use crate::diagnostic::{Diagnostic, Level, Source, Span};
use crate::error::{Error, Result};
use crate::lexer::{tokenize, TokKind, Token};
use crate::source::SourceFile;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a lint sees.
pub struct LintContext<'a> {
    pub file: &'a SourceFile,
    pub tokens: &'a [Token],
    /// From `[lints.per-unit]`; annotation-dependent lints are downgraded in
    /// `lenient` units (spec section 7.3).
    pub strictness: Strictness,
}

/// A registered lint. The code is declared here and is stable forever.
pub struct Lint {
    pub code: &'static str,
    pub name: &'static str,
    pub default_level: Level,
    pub run: fn(&LintContext<'_>) -> Vec<Diagnostic>,
}

const LINTS: &[Lint] = &[
    Lint {
        code: "P0304",
        name: "checks-disabled",
        default_level: Level::Error,
        run: directives::checks_disabled,
    },
    Lint {
        code: "P0306",
        name: "float-equality",
        default_level: Level::Warning,
        run: numeric::float_equality,
    },
    Lint {
        code: "P0701",
        name: "unsafe-outside-unsafe",
        default_level: Level::Error,
        run: unsafe_::unsafe_outside_unsafe,
    },
    Lint {
        code: "P0702",
        name: "with-statement",
        default_level: Level::Error,
        run: banned::with_statement,
    },
    Lint {
        code: "P0703",
        name: "goto",
        default_level: Level::Warning,
        run: banned::goto,
    },
    Lint {
        code: "P0801",
        name: "empty-except",
        default_level: Level::Error,
        run: exceptions::empty_except,
    },
    Lint {
        code: "P0802",
        name: "catch-all-except",
        default_level: Level::Warning,
        run: exceptions::catch_all_except,
    },
    Lint {
        code: "P0902",
        name: "todo-marker",
        default_level: Level::Warning,
        run: markers::todo_marker,
    },
];

/// Every registered lint.
pub fn lints() -> &'static [Lint] {
    LINTS
}

pub fn lint_by_code(code: &str) -> Option<&'static Lint> {
    LINTS.iter().find(|l| l.code.eq_ignore_ascii_case(code))
}

#[derive(Debug, Clone, Default)]
pub struct AnalysisOptions {
    /// `CODE -> level`, from `[lints]` merged with CLI flags.
    pub level_overrides: BTreeMap<String, LevelSetting>,
    pub catalog: Catalog,
    pub strictness: Strictness,
}

#[derive(Debug, Default, Clone)]
pub struct AnalysisResult {
    pub diagnostics: Vec<Diagnostic>,
    /// Diagnostics removed by `{@allow(...)}`.
    pub suppressed_by_allow: usize,
}

/// Analyze one already-loaded source file.
pub fn analyze_source(file: &SourceFile, opts: &AnalysisOptions) -> AnalysisResult {
    let tokens = tokenize(&file.path, &file.text);
    let allows = collect_allows(&tokens);
    let ctx = LintContext { file, tokens: &tokens, strictness: opts.strictness };

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    for lint in LINTS {
        for mut diag in (lint.run)(&ctx) {
            let setting = opts.level_overrides.get(&lint.code.to_ascii_uppercase()).copied();
            match setting {
                Some(LevelSetting::Allow) => continue,
                Some(s) => diag.level = s.to_level().unwrap_or(lint.default_level),
                None => diag.level = lint.default_level,
            }
            diag.source = Source::Fpcheck;
            opts.catalog.enrich(&mut diag);
            diagnostics.push(diag);
        }
    }

    // An `{@allow}` without a reason is itself an error (section 6.3).
    for allow in &allows {
        if allow.reason.is_none() {
            let mut diag = Diagnostic::lint(
                "P0001",
                Level::Error,
                "`{@allow}` without a `reason:`",
                allow.span.clone(),
            )
            .with_help("add `reason: \"...\"` explaining why this diagnostic does not apply")
            .with_note("every suppression must be auditable; there is no blanket disable");
            opts.catalog.enrich(&mut diag);
            diagnostics.push(diag);
        }
    }

    let mut result = AnalysisResult::default();
    for diag in diagnostics {
        if is_suppressed(&allows, &diag) {
            result.suppressed_by_allow += 1;
            continue;
        }
        result.diagnostics.push(diag);
    }
    sort_by_location(&mut result.diagnostics);
    result
}

/// Read and analyze a file from disk.
pub fn analyze_file(path: &Path, opts: &AnalysisOptions) -> Result<AnalysisResult> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::io(path.display().to_string(), e))?;
    Ok(analyze_source(&SourceFile::new(path, text), opts))
}

/// Analyze many files, returning every diagnostic in file order.
pub fn analyze_paths(paths: &[PathBuf], opts: &AnalysisOptions) -> Result<AnalysisResult> {
    let mut combined = AnalysisResult::default();
    for path in paths {
        let one = analyze_file(path, opts)?;
        combined.diagnostics.extend(one.diagnostics);
        combined.suppressed_by_allow += one.suppressed_by_allow;
    }
    sort_by_location(&mut combined.diagnostics);
    Ok(combined)
}

fn sort_by_location(diags: &mut [Diagnostic]) {
    diags.sort_by(|a, b| {
        a.primary
            .file
            .cmp(&b.primary.file)
            .then_with(|| a.primary.line_start.cmp(&b.primary.line_start))
            .then_with(|| a.primary.col_start.cmp(&b.primary.col_start))
            .then_with(|| a.code.cmp(&b.code))
    });
}

/// Expand configured roots into Pascal source files.
///
/// Entries may be files, directories, or simple globs ending in `/**`. Hidden
/// directories and non-Pascal files are skipped.
pub fn collect_sources(roots: &[String]) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for root in roots {
        let cleaned = root.trim_end_matches("/**").trim_end_matches("/*");
        let path = PathBuf::from(cleaned);
        if path.is_file() {
            found.push(path);
        } else if path.is_dir() {
            walk(&path, &mut found)?;
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir.display().to_string(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(dir.display().to_string(), e))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else if is_pascal_source(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_pascal_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("pas") | Some("pp") | Some("p") | Some("inc")
    )
}

// ---------------------------------------------------------------------------
// `{@allow(...)}` handling
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Allow {
    pub codes: Vec<String>,
    pub reason: Option<String>,
    pub span: Span,
}

/// Collect every `{@allow(...)}` annotation in a file.
pub(crate) fn collect_allows(tokens: &[Token]) -> Vec<Allow> {
    let mut out = Vec::new();
    for token in tokens {
        if token.kind != TokKind::Comment {
            continue;
        }
        let Some(body) = token.comment_body() else { continue };
        let body = body.trim();
        let Some(rest) = body.strip_prefix("@allow") else { continue };
        let rest = rest.trim();

        let mut codes = Vec::new();
        let mut reason = None;
        if let Some(inner) = rest.strip_prefix('(') {
            let inner = inner.trim_end_matches(')');
            for part in split_top_level(inner) {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                if let Some(value) = part.strip_prefix("reason") {
                    let value = value.trim_start().trim_start_matches(':').trim();
                    let value = value.trim_matches('"').trim();
                    if !value.is_empty() {
                        reason = Some(value.to_string());
                    }
                } else {
                    codes.push(part.to_ascii_uppercase());
                }
            }
        }
        out.push(Allow { codes, reason, span: token.span.clone() });
    }
    out
}

/// Split on commas that are not inside double quotes.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ',' if !in_quotes => {
                parts.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    parts.push(current);
    parts
}

/// An annotation applies to what follows it; the nearest preceding one wins.
fn is_suppressed(allows: &[Allow], diag: &Diagnostic) -> bool {
    let target = (diag.primary.line_start, diag.primary.col_start);
    let nearest = allows
        .iter()
        .filter(|a| a.span.file == diag.primary.file)
        .filter(|a| (a.span.line_start, a.span.col_start) <= target)
        .max_by_key(|a| (a.span.line_start, a.span.col_start));
    match nearest {
        Some(allow) => allow.codes.iter().any(|c| c.eq_ignore_ascii_case(&diag.code)),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Shared token helpers
// ---------------------------------------------------------------------------

/// The next token at or after `index` that is not a comment or directive.
pub(crate) fn next_significant(tokens: &[Token], index: usize) -> Option<(usize, &Token)> {
    tokens
        .iter()
        .enumerate()
        .skip(index)
        .find(|(_, t)| !t.is_trivia() && t.kind != TokKind::Eof)
}

/// Whether `index` sits inside an `{@unsafe(...)}` region.
pub(crate) fn in_unsafe_region(tokens: &[Token], index: usize) -> bool {
    let mut depth = 0i32;
    for token in tokens[..index].iter().rev() {
        if token.kind != TokKind::Comment {
            continue;
        }
        let Some(body) = token.comment_body() else { continue };
        let body = body.trim();
        if body.starts_with("@end_unsafe") {
            depth += 1;
        } else if body.starts_with("@unsafe") {
            if depth == 0 {
                return true;
            }
            depth -= 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(text: &str) -> AnalysisResult {
        analyze_source(&SourceFile::new("t.pas", text), &AnalysisOptions::default())
    }

    fn codes(text: &str) -> Vec<String> {
        analyze(text).diagnostics.into_iter().map(|d| d.code).collect()
    }

    #[test]
    fn registered_codes_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for lint in lints() {
            assert!(seen.insert(lint.code), "duplicate code {}", lint.code);
            assert_eq!(lint.code.len(), 5, "{}", lint.code);
            assert!(lint.code.starts_with('P'), "{}", lint.code);
        }
    }

    #[test]
    fn lint_lookup_is_case_insensitive() {
        assert!(lint_by_code("p0702").is_some());
        assert!(lint_by_code("P0702").is_some());
        assert!(lint_by_code("P9999").is_none());
    }

    #[test]
    fn clean_code_produces_no_diagnostics() {
        let text = "program p;\nvar\n  x: Integer;\nbegin\n  x := 1;\n  Writeln(x);\nend.\n";
        assert!(codes(text).is_empty(), "{:?}", codes(text));
    }

    #[test]
    fn allow_with_a_reason_suppresses_the_lint() {
        let text =
            "{@allow(P0702, reason: \"legacy interop\")}\nbegin\n  with Foo do Bar;\nend.\n";
        let result = analyze(text);
        assert!(
            !result.diagnostics.iter().any(|d| d.code == "P0702"),
            "{:?}",
            result.diagnostics
        );
        assert_eq!(result.suppressed_by_allow, 1);
    }

    #[test]
    fn allow_without_a_reason_is_an_error() {
        let text = "{@allow(P0702)}\nbegin\n  with Foo do Bar;\nend.\n";
        let result = analyze(text);
        assert!(
            result.diagnostics.iter().any(|d| d.code == "P0001"),
            "{:?}",
            result.diagnostics
        );
    }

    #[test]
    fn allow_for_a_different_code_does_not_suppress() {
        let text = "{@allow(P0801, reason: \"nope\")}\nbegin\n  with Foo do Bar;\nend.\n";
        assert!(codes(text).contains(&"P0702".to_string()));
    }

    #[test]
    fn allow_with_commas_in_the_reason_still_parses() {
        let text = "{@allow(P0702, reason: \"a, b, c\")}\nbegin\n  with Foo do Bar;\nend.\n";
        let result = analyze(text);
        assert!(!result.diagnostics.iter().any(|d| d.code == "P0702"));
        assert!(!result.diagnostics.iter().any(|d| d.code == "P0001"));
    }

    #[test]
    fn allow_can_list_several_codes() {
        let text =
            "{@allow(P0702, P0703, reason: \"both\")}\nbegin\n  with Foo do goto done;\nend.\n";
        let result = analyze(text);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    #[test]
    fn allow_only_applies_after_it() {
        let text = "begin\n  with Foo do Bar;\nend.\n{@allow(P0702, reason: \"later\")}\n";
        assert!(codes(text).contains(&"P0702".to_string()));
    }

    #[test]
    fn level_overrides_change_the_reported_level() {
        let mut opts = AnalysisOptions::default();
        opts.level_overrides.insert("P0702".into(), LevelSetting::Warn);
        let text = "begin\n  with Foo do Bar;\nend.\n";
        let result = analyze_source(&SourceFile::new("t.pas", text), &opts);
        assert_eq!(result.diagnostics[0].level, Level::Warning);
    }

    #[test]
    fn allow_override_removes_the_lint_entirely() {
        let mut opts = AnalysisOptions::default();
        opts.level_overrides.insert("P0702".into(), LevelSetting::Allow);
        let text = "begin\n  with Foo do Bar;\nend.\n";
        let result = analyze_source(&SourceFile::new("t.pas", text), &opts);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn diagnostics_come_back_in_source_order() {
        let text = "begin\n  with Foo do Bar;\n  goto done;\nend.\n";
        let diags = analyze(text).diagnostics;
        let positions: Vec<(u32, u32)> = diags
            .iter()
            .map(|d| (d.primary.line_start, d.primary.col_start))
            .collect();
        let mut sorted = positions.clone();
        sorted.sort();
        assert_eq!(positions, sorted);
    }

    #[test]
    fn unsafe_region_detection() {
        let tokens = tokenize(
            Path::new("t.pas"),
            "{@unsafe(reason: \"io\")}\n@x\n{@end_unsafe}\n@y\n",
        );
        let x_index = tokens.iter().position(|t| t.text == "@" && t.line == 2).unwrap();
        let y_index = tokens.iter().position(|t| t.text == "@" && t.line == 4).unwrap();
        assert!(in_unsafe_region(&tokens, x_index));
        assert!(!in_unsafe_region(&tokens, y_index));
    }

    #[test]
    fn collect_sources_finds_pascal_files_and_skips_noise() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("a.pas"), "unit a; end.").unwrap();
        std::fs::write(src.join("nested/b.pp"), "unit b; end.").unwrap();
        std::fs::write(src.join("c.txt"), "not pascal").unwrap();
        std::fs::write(src.join("a.ppu"), "compiled").unwrap();
        std::fs::create_dir_all(src.join(".hidden")).unwrap();
        std::fs::write(src.join(".hidden/d.pas"), "unit d; end.").unwrap();

        let found = collect_sources(&[src.display().to_string()]).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.pas", "b.pp"]);
    }

    #[test]
    fn collect_sources_accepts_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only.pas");
        std::fs::write(&file, "unit only; end.").unwrap();
        let found = collect_sources(&[file.display().to_string()]).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn collect_sources_ignores_missing_roots() {
        let found = collect_sources(&["/definitely/not/here".to_string()]).unwrap();
        assert!(found.is_empty());
    }
}
