//! Renderers: one diagnostic model, many presentations (spec section 5).
//!
//! `human` is rustc-style for people, `llm` is the compact agent-facing format,
//! and `json`/`sarif` are stable machine formats. Nothing here invents
//! information: every rendered detail is present on the [`Diagnostic`].

pub mod human;
pub mod json;
pub mod llm;
pub mod sarif;

use crate::diagnostic::{Applicability, Diagnostic, Level};
use crate::error::{Error, Result};
use crate::source::{SourceFile, SourceManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageFormat {
    Human,
    Json,
    Llm,
    Sarif,
}

impl MessageFormat {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "human" => Ok(MessageFormat::Human),
            "json" => Ok(MessageFormat::Json),
            "llm" => Ok(MessageFormat::Llm),
            "sarif" => Ok(MessageFormat::Sarif),
            other => Err(Error::Usage(format!(
                "unknown --message-format `{other}` (expected human, json, llm, or sarif)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MessageFormat::Human => "human",
            MessageFormat::Json => "json",
            MessageFormat::Llm => "llm",
            MessageFormat::Sarif => "sarif",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    /// `0` means unlimited. Applies to `human` and `llm` only.
    pub max_diagnostics: usize,
    /// Source lines shown either side of a span in the `llm` format.
    pub context_lines: u32,
    pub color: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { max_diagnostics: 15, context_lines: 2, color: false }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderSummary {
    pub errors: usize,
    pub warnings: usize,
    pub notes: usize,
    pub help: usize,
}

impl RenderSummary {
    pub fn total(&self) -> usize {
        self.errors + self.warnings + self.notes + self.help
    }
}

pub fn summarize(diags: &[Diagnostic]) -> RenderSummary {
    let mut summary = RenderSummary::default();
    for d in diags {
        match d.level {
            Level::Error => summary.errors += 1,
            Level::Warning => summary.warnings += 1,
            Level::Note => summary.notes += 1,
            Level::Help => summary.help += 1,
        }
    }
    summary
}

/// The diagnostics a length-capped renderer should show, plus the omitted count.
pub(crate) fn visible(diags: &[Diagnostic], max: usize) -> (&[Diagnostic], usize) {
    if max == 0 || diags.len() <= max {
        (diags, 0)
    } else {
        (&diags[..max], diags.len() - max)
    }
}

pub(crate) fn applicability_str(a: Applicability) -> &'static str {
    match a {
        Applicability::MachineApplicable => "machine-applicable",
        Applicability::MaybeIncorrect => "maybe-incorrect",
        Applicability::HasPlaceholders => "has-placeholders",
        Applicability::Unspecified => "unspecified",
    }
}

/// Render with the requested format.
pub fn render(
    format: MessageFormat,
    diags: &[Diagnostic],
    sources: &mut SourceManager,
    opts: &RenderOptions,
) -> String {
    match format {
        MessageFormat::Human => human::render(diags, sources, opts),
        MessageFormat::Json => json::render(diags, sources, opts),
        MessageFormat::Llm => llm::render(diags, sources, opts),
        MessageFormat::Sarif => sarif::render(diags, sources, opts),
    }
}

/// Expand tabs to the next multiple of `tab`, so carets line up with the text
/// a terminal shows.
pub(crate) fn expand_tabs(s: &str, tab: usize) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let spaces = tab - (col % tab);
            out.push_str(&" ".repeat(spaces));
            col += spaces;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// A minimal unified diff for a diagnostic's suggestions.
///
/// Edits carry explicit spans, so no diff algorithm is needed: the hunk is the
/// span's lines (with `context_lines` of surrounding context), shown before and
/// after applying each replacement.
pub(crate) fn render_suggestion_diff(
    d: &Diagnostic,
    sources: &mut SourceManager,
    context_lines: u32,
) -> String {
    let mut out = String::new();

    for suggestion in &d.suggestions {
        for edit in &suggestion.edits {
            let path = edit.span.file.clone();
            let Some(file) = sources.load_opt(&path) else {
                continue;
            };
            let original = file.text.clone();
            let old_lines: Vec<String> = (edit.span.line_start..=edit.span.line_end)
                .filter_map(|line| file.line(line).map(|s| s.to_string()))
                .collect();

            let (Some(start), Some(end)) = (
                file.offset(edit.span.line_start, edit.span.col_start),
                file.offset(edit.span.line_end, edit.span.col_end),
            ) else {
                continue;
            };
            if end < start {
                continue;
            }

            let mut replaced = String::with_capacity(original.len() + edit.replacement.len());
            replaced.push_str(&original[..start]);
            replaced.push_str(&edit.replacement);
            replaced.push_str(&original[end..]);
            let new_file = SourceFile::new(path.clone(), replaced);

            let old_changed = edit.span.line_end - edit.span.line_start + 1;
            let new_changed = old_changed + edit.replacement.matches('\n').count() as u32;

            let before = context_lines.min(edit.span.line_start.saturating_sub(1));
            let first_old = edit.span.line_start - before;
            let last_old = (edit.span.line_end + context_lines).min(file.line_count() as u32);
            let after = last_old - edit.span.line_end;

            out.push_str(&format!("--- a/{}\n", path.display()));
            out.push_str(&format!("+++ b/{}\n", path.display()));
            out.push_str(&format!(
                "@@ -{first_old},{} +{first_old},{} @@\n",
                old_changed + before + after,
                new_changed + before + after
            ));

            for line in first_old..edit.span.line_start {
                out.push_str(&format!(" {}\n", file.line(line).unwrap_or("")));
            }
            for line in &old_lines {
                out.push_str(&format!("-{line}\n"));
            }
            for line in edit.span.line_start..(edit.span.line_start + new_changed) {
                out.push_str(&format!("+{}\n", new_file.line(line).unwrap_or("")));
            }
            for line in (edit.span.line_end + 1)..=last_old {
                out.push_str(&format!(" {}\n", file.line(line).unwrap_or("")));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Edit, Label, Source, Span, Suggestion};

    const SOURCE: &str = "program p;\n  with Foo do\nbegin\nend.\n";

    fn setup() -> (SourceManager, Diagnostic) {
        let mut sources = SourceManager::new();
        sources.insert(SourceFile::new("a.pas", SOURCE));
        let span = Span::new("a.pas", 2, 3, 2, 11);
        let mut d = Diagnostic::new("P0702", Level::Error, "`with` is banned", span.clone(), Source::Fpcheck);
        d.labels.push(Label { span, text: "banned here".into() });
        d.notes.push("a why".into());
        d.help.push("bind a local instead".into());
        (sources, d)
    }

    #[test]
    fn format_parsing() {
        assert_eq!(MessageFormat::parse("human").unwrap(), MessageFormat::Human);
        assert_eq!(MessageFormat::parse("JSON").unwrap(), MessageFormat::Json);
        assert_eq!(MessageFormat::parse("llm").unwrap(), MessageFormat::Llm);
        assert_eq!(MessageFormat::parse("sarif").unwrap(), MessageFormat::Sarif);
        assert!(MessageFormat::parse("xml").is_err());
    }

    #[test]
    fn summarize_counts_each_level() {
        let (_s, d) = setup();
        let mut w = d.clone();
        w.level = Level::Warning;
        let mut n = d.clone();
        n.level = Level::Note;
        let s = summarize(&[d, w, n]);
        assert_eq!((s.errors, s.warnings, s.notes), (1, 1, 1));
        assert_eq!(s.total(), 3);
    }

    #[test]
    fn visible_caps_and_reports_omissions() {
        let (_s, d) = setup();
        let diags = vec![d.clone(), d.clone(), d.clone()];
        assert_eq!(visible(&diags, 2).1, 1);
        assert_eq!(visible(&diags, 0).1, 0);
        assert_eq!(visible(&diags, 5).1, 0);
    }

    #[test]
    fn expand_tabs_aligns_to_stops() {
        assert_eq!(expand_tabs("a\tb", 4), "a   b");
        assert_eq!(expand_tabs("\tx", 4), "    x");
        assert_eq!(expand_tabs("abcd\te", 4), "abcd    e");
        assert_eq!(expand_tabs("no tabs", 4), "no tabs");
    }

    #[test]
    fn human_shows_header_location_caret_and_advice() {
        let (mut sources, d) = setup();
        let text = human::render(&[d], &mut sources, &RenderOptions::default());
        assert!(text.contains("error[P0702]: `with` is banned"), "{text}");
        assert!(text.contains("--> a.pas:2:3"), "{text}");
        assert!(text.contains("2 |   with Foo do"), "{text}");
        assert!(text.contains("^^^^^^^^"), "{text}");
        assert!(text.contains("= note: a why"), "{text}");
        assert!(text.contains("= help: bind a local instead"), "{text}");
        assert!(text.contains("banned here"), "{text}");
    }

    #[test]
    fn human_underlines_a_multi_line_span() {
        let mut sources = SourceManager::new();
        sources.insert(SourceFile::new("m.pas", "begin\n  a := 1;\n  b := 2;\nend.\n"));
        let span = Span::new("m.pas", 2, 3, 3, 10);
        let d = Diagnostic::new("P0002", Level::Warning, "wide span", span, Source::Fpcheck);
        let text = human::render(&[d], &mut sources, &RenderOptions::default());
        assert!(text.contains("2 |   a := 1;"), "{text}");
        assert!(text.contains("3 |   b := 2;"), "{text}");
    }

    #[test]
    fn human_is_quiet_when_there_is_nothing_to_say() {
        let mut sources = SourceManager::new();
        assert_eq!(human::render(&[], &mut sources, &RenderOptions::default()), "");
    }

    #[test]
    fn human_caps_output_and_says_how_many_were_hidden() {
        let (mut sources, d) = setup();
        let diags = vec![d.clone(), d.clone(), d.clone()];
        let opts = RenderOptions { max_diagnostics: 1, ..Default::default() };
        let text = human::render(&diags, &mut sources, &opts);
        assert!(text.contains("2 more diagnostic(s) not shown"), "{text}");
    }

    #[test]
    fn human_survives_a_missing_source_file() {
        let mut sources = SourceManager::new();
        let d = Diagnostic::new(
            "F0001",
            Level::Error,
            "no source",
            Span::point("gone.pas", 4, 2),
            Source::Fpc,
        );
        let text = human::render(&[d], &mut sources, &RenderOptions::default());
        assert!(text.contains("--> gone.pas:4:2"), "{text}");
    }

    #[test]
    fn llm_is_compact_and_ends_with_the_summary_line() {
        let (mut sources, d) = setup();
        let text = llm::render(&[d], &mut sources, &RenderOptions::default());
        assert!(text.starts_with("error[P0702] a.pas:2:3: `with` is banned"), "{text}");
        assert!(text.contains("  note: a why"), "{text}");
        assert!(text.contains("  help: bind a local instead"), "{text}");
        assert!(text.ends_with("fpcheck: 1 errors, 0 warnings. Run 'fpcheck explain <code>' for details.\n"), "{text}");
        // No box drawing in the agent format.
        assert!(!text.contains('│'), "{text}");
    }

    #[test]
    fn llm_shows_context_lines_and_marks_the_span() {
        let (mut sources, d) = setup();
        let text = llm::render(&[d], &mut sources, &RenderOptions::default());
        assert!(text.contains(">2 |   with Foo do"), "{text}");
        assert!(text.contains(" 1 | program p;"), "{text}");
        assert!(text.contains("^^^^^^^^"), "{text}");
    }

    #[test]
    fn llm_reports_omitted_diagnostics() {
        let (mut sources, d) = setup();
        let diags = vec![d.clone(), d.clone(), d.clone(), d.clone()];
        let opts = RenderOptions { max_diagnostics: 2, ..Default::default() };
        let text = llm::render(&diags, &mut sources, &opts);
        assert!(text.contains("2 more diagnostic(s) omitted"), "{text}");
    }

    #[test]
    fn llm_summary_counts_all_diagnostics_not_just_shown_ones() {
        let (mut sources, d) = setup();
        let mut w = d.clone();
        w.level = Level::Warning;
        let diags = vec![d, w];
        let opts = RenderOptions { max_diagnostics: 1, ..Default::default() };
        let text = llm::render(&diags, &mut sources, &opts);
        assert!(text.contains("1 errors, 1 warnings"), "{text}");
    }

    #[test]
    fn json_is_one_object_per_line_plus_a_summary() {
        let (mut sources, d) = setup();
        let text = json::render(&[d], &mut sources, &RenderOptions::default());
        let lines: Vec<&str> = text.trim_end().lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["code"], "P0702");
        assert_eq!(first["level"], "error");
        assert_eq!(first["primary"]["line_start"], 2);
        let last: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(last["summary"]["schema"], crate::JSON_SCHEMA_VERSION);
        assert_eq!(last["summary"]["errors"], 1);
        assert_eq!(last["summary"]["total"], 1);
    }

    #[test]
    fn json_is_not_truncated_by_max_diagnostics() {
        let (mut sources, d) = setup();
        let diags = vec![d.clone(), d.clone(), d.clone()];
        let opts = RenderOptions { max_diagnostics: 1, ..Default::default() };
        let text = json::render(&diags, &mut sources, &opts);
        assert_eq!(text.trim_end().lines().count(), 4);
    }

    #[test]
    fn json_with_no_diagnostics_is_just_the_summary() {
        let mut sources = SourceManager::new();
        let text = json::render(&[], &mut sources, &RenderOptions::default());
        let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["summary"]["total"], 0);
    }

    #[test]
    fn sarif_is_valid_and_carries_regions() {
        let (mut sources, d) = setup();
        let text = sarif::render(&[d], &mut sources, &RenderOptions::default());
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["version"], "2.1.0");
        let run = &doc["runs"][0];
        assert_eq!(run["tool"]["driver"]["name"], "fpcheck");
        let result = &run["results"][0];
        assert_eq!(result["ruleId"], "P0702");
        assert_eq!(result["level"], "error");
        let region = &result["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"], 2);
        assert_eq!(region["startColumn"], 3);
        assert_eq!(region["endColumn"], 11);
    }

    #[test]
    fn suggestion_produces_a_unified_diff() {
        let (mut sources, mut d) = setup();
        d.suggestions.push(Suggestion::machine(
            "replace with an explicit target",
            vec![Edit {
                span: Span::new("a.pas", 2, 3, 2, 11),
                replacement: "Obj".into(),
            }],
        ));
        let diff = render_suggestion_diff(&d, &mut sources, 1);
        assert!(diff.contains("--- a/a.pas"), "{diff}");
        assert!(diff.contains("+++ b/a.pas"), "{diff}");
        assert!(diff.contains("-  with Foo do"), "{diff}");
        assert!(diff.contains("+  Obj do"), "{diff}");
        assert!(diff.contains(" 1 | program p;") || diff.contains(" program p;"), "{diff}");
    }

    #[test]
    fn diff_without_suggestions_is_empty() {
        let (mut sources, d) = setup();
        assert_eq!(render_suggestion_diff(&d, &mut sources, 2), "");
    }

    #[test]
    fn dispatch_routes_each_format() {
        let (mut sources, d) = setup();
        let opts = RenderOptions::default();
        assert!(render(MessageFormat::Human, &[d.clone()], &mut sources, &opts).contains("-->"));
        assert!(render(MessageFormat::Json, &[d.clone()], &mut sources, &opts).contains("\"summary\""));
        assert!(render(MessageFormat::Llm, &[d.clone()], &mut sources, &opts).contains("fpcheck: "));
        assert!(render(MessageFormat::Sarif, &[d], &mut sources, &opts).contains("2.1.0"));
    }

    #[test]
    fn empty_input_renders_nothing_for_every_format() {
        let mut sources = SourceManager::new();
        let opts = RenderOptions::default();
        assert_eq!(render(MessageFormat::Human, &[], &mut sources, &opts), "");
        assert_eq!(render(MessageFormat::Llm, &[], &mut sources, &opts).lines().count(), 1);
        assert!(render(MessageFormat::Sarif, &[], &mut sources, &opts).contains("results"));
    }
}
