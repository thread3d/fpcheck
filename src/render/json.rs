//! The `--message-format=json` renderer (spec section 5.2).
//!
//! JSON Lines: one `Diagnostic` per line, then a final summary line carrying the
//! schema version. Machine formats are never truncated — `--max-diagnostics`
//! applies to the human and llm renderers only.

use crate::diagnostic::Diagnostic;
use crate::render::{summarize, RenderOptions};
use crate::source::SourceManager;

/// The versioned summary object, shared by this renderer's last line and the
/// MCP `check` result. Keeping one builder means the two cannot drift.
pub fn summary_value(diags: &[Diagnostic]) -> serde_json::Value {
    let summary = summarize(diags);
    serde_json::json!({
        "schema": crate::JSON_SCHEMA_VERSION,
        "errors": summary.errors,
        "warnings": summary.warnings,
        "notes": summary.notes,
        "help": summary.help,
        "total": diags.len(),
    })
}

pub fn render(diags: &[Diagnostic], _sources: &mut SourceManager, _opts: &RenderOptions) -> String {
    let mut out = String::new();
    for d in diags {
        if let Ok(line) = serde_json::to_string(d) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    let line = serde_json::json!({ "summary": summary_value(diags) });
    out.push_str(&line.to_string());
    out.push('\n');
    out
}
