//! The rustc-style human renderer (spec section 5.1).
//!
//! Rules the spec fixes: always `file:line:col`, always show the source line
//! with a caret span, secondary labels on other lines, `= note:` for why and
//! `= help:` for what to do.

use crate::diagnostic::Diagnostic;
use crate::render::{
    applicability_str, expand_tabs, render_suggestion_diff, visible, RenderOptions,
};
use crate::source::SourceManager;
use std::collections::{BTreeMap, BTreeSet};

pub fn render(diags: &[Diagnostic], sources: &mut SourceManager, opts: &RenderOptions) -> String {
    let (shown, omitted) = visible(diags, opts.max_diagnostics);
    let mut out = String::new();
    for d in shown {
        render_one(d, sources, opts, &mut out);
    }
    if omitted > 0 {
        out.push_str(&format!("... {omitted} more diagnostic(s) not shown\n"));
    }
    out
}

fn render_one(d: &Diagnostic, sources: &mut SourceManager, opts: &RenderOptions, out: &mut String) {
    out.push_str(&format!(
        "{}[{}]: {}\n",
        d.level.as_str(),
        d.code,
        d.message
    ));
    out.push_str(&format!("  --> {}\n", d.primary.location()));

    // Every line that carries a span, so labels on other lines are shown too.
    let mut lines: BTreeSet<u32> = BTreeSet::new();
    for line in d.primary.line_start..=d.primary.line_end {
        lines.insert(line);
    }
    for label in &d.labels {
        for line in label.span.line_start..=label.span.line_end {
            lines.insert(line);
        }
    }

    let width = lines
        .iter()
        .next_back()
        .copied()
        .unwrap_or(1)
        .to_string()
        .len()
        .max(1);
    let gutter = " ".repeat(width);

    // Copy the text out so the borrow of `sources` ends before rendering diffs.
    let mut texts: BTreeMap<u32, String> = BTreeMap::new();
    if let Some(file) = sources.load_opt(&d.primary.file) {
        for line in &lines {
            if let Some(text) = file.line(*line) {
                texts.insert(*line, expand_tabs(text, 4));
            }
        }
    }

    out.push_str(&format!("{gutter} |\n"));

    let mut previous: Option<u32> = None;
    for line in &lines {
        if let Some(prev) = previous {
            if *line > prev + 1 {
                out.push_str(&format!("{:>width$} |\n", "...", width = width));
            }
        }
        previous = Some(*line);

        let text = texts.get(line).map(|s| s.as_str()).unwrap_or("");
        out.push_str(&format!("{:>width$} | {}\n", line, text, width = width));

        let line_len = text.chars().count() as u32;
        for (start, end, label) in underlines(d, *line, line_len) {
            let pad = " ".repeat(start.saturating_sub(1) as usize);
            let carets = "^".repeat(end.saturating_sub(start).max(1) as usize);
            let caption = label.map(|l| format!(" {l}")).unwrap_or_default();
            out.push_str(&format!("{gutter} | {pad}{carets}{caption}\n"));
        }
    }

    out.push_str(&format!("{gutter} |\n"));

    for note in &d.notes {
        out.push_str(&format!("{gutter} = note: {note}\n"));
    }
    for help in &d.help {
        out.push_str(&format!("{gutter} = help: {help}\n"));
    }
    for suggestion in &d.suggestions {
        if suggestion.edits.is_empty() {
            out.push_str(&format!("{gutter} = help: {}\n", suggestion.message));
            continue;
        }
        out.push_str(&format!(
            "{gutter} = help: {} [{}]\n",
            suggestion.message,
            applicability_str(suggestion.applicability)
        ));
        let diff = render_suggestion_diff(d, sources, opts.context_lines);
        for diff_line in diff.lines() {
            out.push_str(&format!("{gutter} | {diff_line}\n"));
        }
    }
    for related in &d.related {
        out.push_str(&format!(
            "{gutter} = note: {}: {}\n",
            related.primary.location(),
            related.message
        ));
    }

    out.push('\n');
}

/// Caret spans to draw under one source line: the primary span, plus any label
/// whose span is not the primary one.
fn underlines(d: &Diagnostic, line: u32, line_len: u32) -> Vec<(u32, u32, Option<String>)> {
    let mut spans = Vec::new();
    let primary = &d.primary;

    if line >= primary.line_start && line <= primary.line_end {
        let start = if line == primary.line_start {
            primary.col_start
        } else {
            1
        };
        let end = if line == primary.line_end {
            primary.col_end
        } else {
            line_len + 1
        };
        let caption = d
            .labels
            .iter()
            .find(|l| l.span == *primary)
            .map(|l| l.text.clone());
        spans.push((start, end.max(start + 1), caption));
    }

    for label in &d.labels {
        if label.span == *primary {
            continue;
        }
        if line >= label.span.line_start && line <= label.span.line_end {
            let start = if line == label.span.line_start {
                label.span.col_start
            } else {
                1
            };
            let end = if line == label.span.line_end {
                label.span.col_end
            } else {
                line_len + 1
            };
            spans.push((start, end.max(start + 1), Some(label.text.clone())));
        }
    }

    spans
}
