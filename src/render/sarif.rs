//! SARIF 2.1.0 output (spec section 3).
//!
//! Machine format: never truncated, and every diagnostic carries a physical
//! region so CI annotations land on the offending span.

use crate::diagnostic::{Diagnostic, Level};
use crate::render::RenderOptions;
use crate::source::SourceManager;
use serde_json::json;
use std::collections::BTreeSet;

pub fn render(diags: &[Diagnostic], _sources: &mut SourceManager, _opts: &RenderOptions) -> String {
    let rule_ids: BTreeSet<&str> = diags.iter().map(|d| d.code.as_str()).collect();
    let rules: Vec<serde_json::Value> = rule_ids
        .iter()
        .map(|id| json!({ "id": id, "name": id }))
        .collect();

    let results: Vec<serde_json::Value> = diags
        .iter()
        .map(|d| {
            json!({
                "ruleId": d.code,
                "level": sarif_level(d.level),
                "message": { "text": d.message },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": d.primary.file.to_string_lossy() },
                        "region": {
                            "startLine": d.primary.line_start,
                            "startColumn": d.primary.col_start,
                            "endLine": d.primary.line_end,
                            "endColumn": d.primary.col_end,
                        }
                    }
                }],
                "partialFingerprints": {
                    "fpcheck/v1": format!("{}:{}:{}", d.code, d.primary.line_start, d.primary.col_start)
                }
            })
        })
        .collect();

    let document = json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "fpcheck",
                    "version": crate::version(),
                    "informationUri": "https://github.com/thread3d/fpcheck",
                    "rules": rules,
                }
            },
            "results": results,
        }]
    });

    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_string())
}

fn sarif_level(level: Level) -> &'static str {
    match level {
        Level::Error => "error",
        Level::Warning => "warning",
        // SARIF has no "help" level; the closest is a note.
        Level::Note | Level::Help => "note",
    }
}
