//! The `--message-format=llm` renderer (spec section 5.3) — the main target.
//!
//! Optimized for an agent's context window: root causes first (the order comes
//! from the normalizer), capped, no ANSI colors, no box drawing, no progress
//! output, and a unified diff for every suggestion that can be applied.

use crate::diagnostic::Diagnostic;
use crate::render::{
    applicability_str, expand_tabs, render_suggestion_diff, summarize, visible, RenderOptions,
};
use crate::source::SourceManager;

pub fn render(diags: &[Diagnostic], sources: &mut SourceManager, opts: &RenderOptions) -> String {
    let (shown, omitted) = visible(diags, opts.max_diagnostics);
    let mut out = String::new();

    for d in shown {
        out.push_str(&format!(
            "{}[{}] {}: {}\n",
            d.level.as_str(),
            d.code,
            d.primary.location(),
            d.message
        ));
        render_excerpt(d, sources, opts, &mut out);

        for note in &d.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
        for help in &d.help {
            out.push_str(&format!("  help: {help}\n"));
        }
        for label in &d.labels {
            out.push_str(&format!("  label: {}\n", label.text));
        }
        for suggestion in &d.suggestions {
            if suggestion.applicability.renders_as_diff() && !suggestion.edits.is_empty() {
                out.push_str(&format!(
                    "  fix [{}]: {}\n",
                    applicability_str(suggestion.applicability),
                    suggestion.message
                ));
                let diff = render_suggestion_diff(d, sources, opts.context_lines);
                for line in diff.lines() {
                    out.push_str(&format!("    {line}\n"));
                }
            } else {
                out.push_str(&format!("  help: {}\n", suggestion.message));
            }
        }
        out.push('\n');
    }

    if omitted > 0 {
        out.push_str(&format!("... {omitted} more diagnostic(s) omitted\n"));
    }

    let summary = summarize(diags);
    out.push_str(&format!(
        "fpcheck: {} errors, {} warnings. Run 'fpcheck explain <code>' for details.\n",
        summary.errors, summary.warnings
    ));
    out
}

/// `context_lines` lines either side of the span, with the span marked.
fn render_excerpt(
    d: &Diagnostic,
    sources: &mut SourceManager,
    opts: &RenderOptions,
    out: &mut String,
) {
    let ctx = opts.context_lines;
    let first = d.primary.line_start.saturating_sub(ctx).max(1);
    let last = d.primary.line_end + ctx;

    let mut texts: Vec<(u32, String)> = Vec::new();
    if let Some(file) = sources.load_opt(&d.primary.file) {
        for line in first..=last {
            if let Some(text) = file.line(line) {
                texts.push((line, expand_tabs(text, 4)));
            }
        }
    }
    if texts.is_empty() {
        return;
    }

    let width = texts
        .iter()
        .map(|(l, _)| l.to_string().len())
        .max()
        .unwrap_or(1);

    for (line, text) in &texts {
        let in_span = *line >= d.primary.line_start && *line <= d.primary.line_end;
        let marker = if in_span { '>' } else { ' ' };
        out.push_str(&format!(
            "{marker}{:>width$} | {text}\n",
            line,
            width = width
        ));
        if in_span {
            let start = if *line == d.primary.line_start {
                d.primary.col_start
            } else {
                1
            };
            let end = if *line == d.primary.line_end {
                d.primary.col_end
            } else {
                text.chars().count() as u32 + 1
            };
            let pad = " ".repeat(start.saturating_sub(1) as usize);
            let carets = "^".repeat(end.saturating_sub(start).max(1) as usize);
            out.push_str(&format!(" {:>width$} | {pad}{carets}\n", "", width = width));
        }
    }
}
