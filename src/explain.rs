//! `fpcheck explain CODE` (spec sections 3 and 6.4).
//!
//! Reads the same catalog the analyzer uses, plus `docs/<CODE>.md` — the
//! long-form explanation with a wrong and a right example. Working for a code
//! that has no implementation yet is intentional: the codes are stable, so a
//! user (or agent) can read about one before it ships.

use crate::catalog::Catalog;
use crate::error::{Error, Result};
use std::path::Path;

/// Render the explanation for a code.
pub fn render(code: &str, catalog: &Catalog, docs_dir: &Path) -> Result<String> {
    let code = code.trim().to_ascii_uppercase();
    if code.is_empty() {
        return Err(Error::Usage("`fpcheck explain` needs a diagnostic code".into()));
    }

    let mut out = String::new();
    let mut found = false;

    if let Some(entry) = catalog.lint(&code) {
        found = true;
        out.push_str(&format!("{code}: {}\n\n", entry.name));
        out.push_str(&format!("{}\n", entry.headline));
        if let Some(label) = &entry.label {
            out.push_str(&format!("  label: {label}\n"));
        }
        for help in &entry.help {
            out.push_str(&format!("  help: {help}\n"));
        }
        if let Some(note) = &entry.note {
            out.push_str(&format!("  note: {note}\n"));
        }
        out.push('\n');
    }

    if let Some(rule) = catalog.message_by_code(&code) {
        found = true;
        out.push_str(&format!("{code}: normalized FPC compiler message\n\n"));
        out.push_str(&format!("  matches: {}\n", rule.template.render_literal()));
        out.push_str(&format!("  message: {}\n", rule.headline.render_literal()));
        for help in &rule.help {
            out.push_str(&format!("  help: {}\n", help.render_literal()));
        }
        if let Some(note) = &rule.note {
            out.push_str(&format!("  note: {}\n", note.render_literal()));
        }
        out.push('\n');
    }

    let doc = docs_dir.join(format!("{code}.md"));
    if doc.is_file() {
        found = true;
        let text = std::fs::read_to_string(&doc)
            .map_err(|e| Error::io(doc.display().to_string(), e))?;
        out.push_str(&text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    if !found {
        return Err(Error::Usage(format!(
            "unknown diagnostic code `{code}` (see catalog/lints.toml for the full list)"
        )));
    }

    Ok(out)
}

/// Every code `explain` knows about, for `--help` style listings and tests.
pub fn known_codes(catalog: &Catalog) -> Vec<String> {
    catalog.lint_codes().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog::builtin()
    }

    #[test]
    fn explains_a_lint_from_the_catalog() {
        let text = render("P0702", &catalog(), Path::new("/nonexistent")).unwrap();
        assert!(text.contains("P0702: with-statement"), "{text}");
        assert!(text.contains("`with` is banned"), "{text}");
        assert!(text.contains("help:"), "{text}");
    }

    #[test]
    fn code_lookup_is_case_insensitive_and_trimmed() {
        let text = render("  p0702 ", &catalog(), Path::new("/nonexistent")).unwrap();
        assert!(text.contains("P0702"), "{text}");
    }

    #[test]
    fn unknown_code_is_a_usage_error() {
        let err = render("P9999", &catalog(), Path::new("/nonexistent")).unwrap_err();
        assert!(matches!(err, Error::Usage(_)), "{err:?}");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn empty_code_is_rejected() {
        assert!(render("", &catalog(), Path::new("/nonexistent")).is_err());
    }

    #[test]
    fn the_doc_file_is_appended_when_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("P0702.md"),
            "# P0702\n\n## Failing example\n\n```pascal\nwith A do B;\n```\n",
        )
        .unwrap();
        let text = render("P0702", &catalog(), dir.path()).unwrap();
        assert!(text.contains("## Failing example"), "{text}");
        assert!(text.contains("with A do B;"), "{text}");
        // The catalog summary still comes first.
        assert!(text.find("`with` is banned").unwrap() < text.find("## Failing").unwrap());
    }

    #[test]
    fn a_generated_code_with_only_a_doc_file_still_explains() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("F5025.md"), "explanation body\n").unwrap();
        let text = render("F5025", &catalog(), dir.path()).unwrap();
        assert_eq!(text, "explanation body\n");
    }

    #[test]
    fn a_normalized_compiler_message_is_explained_by_code() {
        let c = catalog();
        let code = c
            .match_message("Identifier not found \"x\"")
            .expect("seed message")
            .code;
        let text = render(&code, &c, Path::new("/nonexistent")).unwrap();
        assert!(text.contains("normalized FPC compiler message"), "{text}");
        assert!(text.contains("Identifier not found"), "{text}");
    }

    #[test]
    fn every_registered_lint_has_a_catalog_entry() {
        let c = catalog();
        for lint in crate::analyzer::lints() {
            assert!(
                c.lint(lint.code).is_some(),
                "lint {} is registered but has no catalog/lints.toml entry",
                lint.code
            );
        }
    }

    #[test]
    fn known_codes_includes_seeded_entries() {
        let codes = known_codes(&catalog());
        assert!(codes.iter().any(|c| c == "P0702"));
    }
}
