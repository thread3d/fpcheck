//! `fpcheck fix` — apply `MachineApplicable` suggestions (spec section 3).
//!
//! Only `MachineApplicable` suggestions are applied; `MaybeIncorrect` ones are
//! shown by the renderers but never written. Edits are applied to a copy first
//! ([`plan`]) so `--dry-run` and the tests can inspect the result, and
//! overlapping edits are skipped rather than merged.

use crate::diagnostic::{Diagnostic, Span};
use crate::error::{Error, Result};
use crate::source::SourceFile;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The result of applying every applicable suggestion to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFix {
    pub path: PathBuf,
    pub original: String,
    pub fixed: String,
    pub applied: usize,
    pub skipped: usize,
}

impl FileFix {
    pub fn changed(&self) -> bool {
        self.fixed != self.original
    }
}

/// Compute the fixes for a set of diagnostics without touching the disk.
pub fn plan(diags: &[Diagnostic]) -> Result<Vec<FileFix>> {
    let mut by_file: BTreeMap<PathBuf, Vec<(Span, String)>> = BTreeMap::new();

    for diag in diags {
        for suggestion in &diag.suggestions {
            if !suggestion.applicability.is_safe_to_apply() {
                continue;
            }
            for edit in &suggestion.edits {
                by_file
                    .entry(edit.span.file.clone())
                    .or_default()
                    .push((edit.span.clone(), edit.replacement.clone()));
            }
        }
    }

    let mut out = Vec::new();
    for (path, edits) in by_file {
        let original = std::fs::read_to_string(&path)
            .map_err(|e| Error::io(path.display().to_string(), e))?;
        let file = SourceFile::new(&path, original.clone());

        let mut ranges: Vec<(usize, usize, String)> = Vec::new();
        let mut skipped = 0usize;
        for (span, replacement) in edits {
            match (
                file.offset(span.line_start, span.col_start),
                file.offset(span.line_end, span.col_end),
            ) {
                (Some(start), Some(end))
                    if end >= start
                        && original.is_char_boundary(start)
                        && original.is_char_boundary(end) =>
                {
                    ranges.push((start, end, replacement));
                }
                _ => skipped += 1,
            }
        }

        // Accept non-overlapping edits in the order the diagnostics were
        // reported, so root causes (reported first) win over later ones.
        let mut accepted: Vec<(usize, usize, String)> = Vec::new();
        for (start, end, replacement) in ranges {
            let overlaps = accepted
                .iter()
                .any(|(kept_start, kept_end, _)| start < *kept_end && *kept_start < end);
            if overlaps {
                skipped += 1;
                continue;
            }
            accepted.push((start, end, replacement));
        }

        // Apply from the end of the file backwards so earlier offsets stay valid.
        accepted.sort_by(|a, b| b.0.cmp(&a.0));

        let mut fixed = original.clone();
        let mut applied = 0usize;
        for (start, end, replacement) in accepted {
            fixed.replace_range(start..end, &replacement);
            applied += 1;
        }

        out.push(FileFix { path, original, fixed, applied, skipped });
    }

    Ok(out)
}

/// Write the planned fixes. Returns the number of files changed.
pub fn apply(fixes: &[FileFix]) -> Result<usize> {
    let mut changed = 0usize;
    for fix in fixes {
        if fix.changed() {
            std::fs::write(&fix.path, &fix.fixed)
                .map_err(|e| Error::io(fix.path.display().to_string(), e))?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// A short human summary of a plan.
pub fn summarize(fixes: &[FileFix], dry_run: bool) -> String {
    let applied: usize = fixes.iter().map(|f| f.applied).sum();
    let skipped: usize = fixes.iter().map(|f| f.skipped).sum();
    let changed = fixes.iter().filter(|f| f.changed()).count();
    let verb = if dry_run { "would apply" } else { "applied" };
    let mut out = format!("fpcheck fix: {verb} {applied} edit(s) in {changed} file(s)");
    if skipped > 0 {
        out.push_str(&format!(", skipped {skipped}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Applicability, Edit, Level, Source, Suggestion};

    fn write_file(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn diag_with(path: &std::path::Path, span: Span, replacement: &str, applicability: Applicability) -> Diagnostic {
        let mut d = Diagnostic::new(
            "P0702",
            Level::Error,
            "`with` is banned",
            span.clone(),
            Source::Fpcheck,
        );
        d.suggestions.push(Suggestion {
            applicability,
            message: "qualify the access".into(),
            edits: vec![Edit { span, replacement: replacement.to_string() }],
        });
        let _ = path;
        d
    }

    #[test]
    fn a_single_edit_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.pas", "begin\n  with Foo do\nend.\n");
        let span = Span::new(&path, 2, 3, 2, 11);
        let diag = diag_with(&path, span, "Obj", Applicability::MachineApplicable);

        let plan = plan(&[diag]).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].applied, 1);
        assert!(plan[0].fixed.contains("  Obj do"), "{}", plan[0].fixed);
        assert!(plan[0].changed());
    }

    #[test]
    fn maybe_incorrect_edits_are_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.pas", "begin\n  with Foo do\nend.\n");
        let span = Span::new(&path, 2, 3, 2, 11);
        let diag = diag_with(&path, span, "Obj", Applicability::MaybeIncorrect);
        let plan = plan(&[diag]).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn several_edits_in_one_file_keep_their_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.pas", "aa bb cc\n");
        let first = diag_with(&path, Span::new(&path, 1, 1, 1, 3), "XX", Applicability::MachineApplicable);
        let second = diag_with(&path, Span::new(&path, 1, 7, 1, 9), "ZZ", Applicability::MachineApplicable);

        let plan = plan(&[first, second]).unwrap();
        assert_eq!(plan[0].fixed, "XX bb ZZ\n", "{}", plan[0].fixed);
        assert_eq!(plan[0].applied, 2);
    }

    #[test]
    fn overlapping_edits_skip_the_later_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.pas", "abcdef\n");
        let wide = diag_with(&path, Span::new(&path, 1, 1, 1, 7), "WIDE", Applicability::MachineApplicable);
        let narrow = diag_with(&path, Span::new(&path, 1, 3, 1, 5), "NARROW", Applicability::MachineApplicable);

        let plan = plan(&[wide, narrow]).unwrap();
        assert_eq!(plan[0].applied, 1);
        assert_eq!(plan[0].skipped, 1);
        assert_eq!(plan[0].fixed, "WIDE\n");
    }

    #[test]
    fn apply_writes_only_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.pas", "begin\n  with Foo do\nend.\n");
        let span = Span::new(&path, 2, 3, 2, 11);
        let diag = diag_with(&path, span, "Obj", Applicability::MachineApplicable);
        let plan = plan(&[diag]).unwrap();
        assert_eq!(apply(&plan).unwrap(), 1);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("  Obj do"), "{on_disk}");
    }

    #[test]
    fn a_plan_with_no_edits_changes_nothing() {
        let plan = plan(&[]).unwrap();
        assert!(plan.is_empty());
        assert_eq!(apply(&plan).unwrap(), 0);
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let diag = diag_with(
            std::path::Path::new("gone.pas"),
            Span::new("gone.pas", 1, 1, 1, 2),
            "x",
            Applicability::MachineApplicable,
        );
        let err = plan(&[diag]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn summarize_reports_applied_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.pas", "abcdef\n");
        let wide = diag_with(&path, Span::new(&path, 1, 1, 1, 7), "WIDE", Applicability::MachineApplicable);
        let narrow = diag_with(&path, Span::new(&path, 1, 3, 1, 5), "NARROW", Applicability::MachineApplicable);
        let plan = plan(&[wide, narrow]).unwrap();
        let text = summarize(&plan, true);
        assert!(text.contains("would apply 1 edit(s) in 1 file(s)"), "{text}");
        assert!(text.contains("skipped 1"), "{text}");
    }

    #[test]
    fn edits_from_several_files_are_grouped() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_file(dir.path(), "a.pas", "aa\n");
        let b = write_file(dir.path(), "b.pas", "bb\n");
        let da = diag_with(&a, Span::new(&a, 1, 1, 1, 3), "AA", Applicability::MachineApplicable);
        let db = diag_with(&b, Span::new(&b, 1, 1, 1, 3), "BB", Applicability::MachineApplicable);
        let plan = plan(&[da, db]).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(apply(&plan).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "AA\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "BB\n");
    }
}
