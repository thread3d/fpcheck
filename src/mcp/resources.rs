//! MCP resources: the project's diagnostic reference, as data (`SPEC.md` 12.1).
//!
//! Reference material — the two catalogs and the per-code write-ups — is what an
//! agent reaches for *while* fixing a diagnostic, so it is served as MCP
//! resources rather than pushed through the check tool. Everything here reads
//! the catalog the analyzer uses and calls the same [`crate::explain::render`]
//! the `explain` command does, so a resource can never say something `fpcheck
//! explain` would not.

use crate::catalog::{self, Catalog};
use crate::cli::{self, Cli};
use crate::error::Result;
use crate::mcp::protocol::{self, RpcError};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

pub const CATALOG_LINTS_URI: &str = "fpcheck://catalog/lints";
pub const CATALOG_FPC_URI: &str = "fpcheck://catalog/fpc";
/// Prefix of the per-code explanation resources.
pub const EXPLAIN_PREFIX: &str = "fpcheck://explain/";

/// Every code `explain` can serve: the catalog's lints plus every `docs/<CODE>.md`
/// in the project (the docs directory also covers codes whose lint is not
/// implemented yet, which is intentional — codes are stable before they ship).
fn explain_codes(catalog: &Catalog, docs_dir: &Path) -> Vec<String> {
    let mut codes: BTreeSet<String> = catalog.lint_codes().cloned().collect();
    if let Ok(entries) = std::fs::read_dir(docs_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(code) = name.strip_suffix(".md") {
                if !code.is_empty() {
                    codes.insert(code.to_ascii_uppercase());
                }
            }
        }
    }
    codes.into_iter().collect()
}

fn describe_code(catalog: &Catalog, code: &str) -> String {
    match catalog.lint(code) {
        Some(entry) => format!("{code}: {}", entry.name),
        None => format!("{code}: diagnostic"),
    }
}

/// `resources/list`.
pub fn list(cli: &Cli) -> Result<Value> {
    let context = cli::session_context(cli)?;
    let mut resources = vec![
        json!({
            "uri": CATALOG_LINTS_URI,
            "name": "Analyzer lint catalog",
            "description": "Every analyzer lint code, its name, and its help text (catalog/lints.toml).",
            "mimeType": "application/toml",
        }),
        json!({
            "uri": CATALOG_FPC_URI,
            "name": "FPC compiler message catalog",
            "description": "Every Free Pascal compiler message, keyed by the compiler's own message number (catalog/fpc.toml).",
            "mimeType": "application/toml",
        }),
    ];

    for code in explain_codes(&context.catalog, &context.root.join("docs")) {
        resources.push(json!({
            "uri": format!("{EXPLAIN_PREFIX}{code}"),
            "name": describe_code(&context.catalog, &code),
            "description": "Long-form explanation with a failing and a fixed example; the same text as `fpcheck explain`.",
            "mimeType": "text/markdown",
        }));
    }

    Ok(json!({ "resources": resources }))
}

/// `resources/templates/list`.
///
/// The full compiler catalog is ~955 codes; listing every one would drown the
/// lint write-ups, so any code — `P` or `F` — is reachable through this
/// template instead.
pub fn templates() -> Value {
    json!({
        "resourceTemplates": [{
            "uriTemplate": "fpcheck://explain/{code}",
            "name": "Diagnostic explanation",
            "description": "The same text as `fpcheck explain <code>`, for any stable code (analyzer `P` codes and compiler `F` codes).",
            "mimeType": "text/markdown",
        }]
    })
}

/// `resources/read`.
pub fn read(cli: &Cli, params: Option<&Value>) -> std::result::Result<Value, RpcError> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("resources/read requires a string `uri`"))?;

    let context = cli::session_context(cli)
        .map_err(|e| RpcError::new(protocol::INTERNAL_ERROR, e.to_string()))?;

    match uri {
        CATALOG_LINTS_URI => Ok(contents(
            uri,
            "application/toml",
            catalog_text(&context.root, "lints.toml", catalog::embedded_lint_text),
        )),
        CATALOG_FPC_URI => Ok(contents(
            uri,
            "application/toml",
            catalog_text(&context.root, "fpc.toml", catalog::embedded_fpc_text),
        )),
        _ => {
            let Some(code) = uri.strip_prefix(EXPLAIN_PREFIX) else {
                return Err(RpcError::not_found(format!("unknown resource `{uri}`")));
            };
            let code = code.trim().to_ascii_uppercase();
            if code.is_empty() {
                return Err(RpcError::invalid_params(
                    "an explanation resource needs a diagnostic code in its URI",
                ));
            }
            let markdown =
                crate::explain::render(&code, &context.catalog, &context.root.join("docs"))
                    .map_err(|e| RpcError::not_found(e.to_string()))?;
            Ok(contents(uri, "text/markdown", markdown))
        }
    }
}

fn contents(uri: &str, mime: &str, text: String) -> Value {
    json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] })
}

/// A project's catalog file when it has one, else the copy embedded in the
/// binary. An unreadable override falls back rather than failing the read.
fn catalog_text(root: &Path, file: &str, embedded: fn() -> &'static str) -> String {
    let path = root.join("catalog").join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => embedded().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};

    fn server_cli() -> Cli {
        Cli::with_command(Command::Mcp)
    }

    #[test]
    fn list_offers_the_catalogs_and_a_with_statement_explanation() {
        let value = list(&server_cli()).unwrap();
        let uris: Vec<&str> = value["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&CATALOG_LINTS_URI), "{uris:?}");
        assert!(uris.contains(&CATALOG_FPC_URI), "{uris:?}");
        assert!(
            uris.contains(&"fpcheck://explain/P0702"),
            "P0702 must be listed: {uris:?}"
        );
    }

    #[test]
    fn a_project_docs_file_is_listed_even_without_a_lint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/P0401.md"), "# P0401\n").unwrap();
        std::fs::write(dir.path().join("fpcheck.toml"), "[package]\n").unwrap();

        // An explicit config makes the temp directory the project root.
        let mut cli = server_cli();
        cli.config = Some(dir.path().join("fpcheck.toml"));
        let value = list(&cli).unwrap();
        let uris: Vec<&str> = value["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"fpcheck://explain/P0401"), "{uris:?}");
    }

    #[test]
    fn reading_an_explanation_matches_the_explain_command() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/P0702.md"), "# project override\n").unwrap();
        std::fs::write(dir.path().join("fpcheck.toml"), "[package]\n").unwrap();
        let mut cli = server_cli();
        cli.config = Some(dir.path().join("fpcheck.toml"));

        let value = read(&cli, Some(&json!({"uri": "fpcheck://explain/P0702"}))).unwrap();
        let text = value["contents"][0]["text"].as_str().unwrap();
        let expected = crate::explain::render(
            "P0702",
            &crate::catalog::Catalog::builtin(),
            &dir.path().join("docs"),
        )
        .unwrap();
        assert_eq!(text, expected);
        // The catalog summary comes first, then the project's own write-up.
        assert!(text.contains("P0702: with-statement"), "{text}");
        assert!(text.contains("# project override"), "{text}");
        assert_eq!(value["contents"][0]["mimeType"], "text/markdown");
    }

    #[test]
    fn reading_the_compiler_catalog_serves_the_embedded_data() {
        let value = read(
            &server_cli(),
            Some(&json!({"uri": "fpcheck://catalog/fpc"})),
        )
        .unwrap();
        let text = value["contents"][0]["text"].as_str().unwrap();
        assert!(text.contains("[[message]]"), "not the fpc catalog");
        assert_eq!(value["contents"][0]["mimeType"], "application/toml");
    }

    #[test]
    fn a_project_catalog_override_is_served_instead_of_the_embedded_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("catalog")).unwrap();
        std::fs::write(
            dir.path().join("catalog/lints.toml"),
            "[P0702]\nname = \"x\"\nheadline = \"overridden\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("fpcheck.toml"), "[package]\n").unwrap();
        let mut cli = server_cli();
        cli.config = Some(dir.path().join("fpcheck.toml"));

        let value = read(&cli, Some(&json!({"uri": CATALOG_LINTS_URI}))).unwrap();
        let text = value["contents"][0]["text"].as_str().unwrap();
        assert!(text.contains("overridden"), "{text}");
    }

    #[test]
    fn an_unknown_uri_is_not_found() {
        let err = read(&server_cli(), Some(&json!({"uri": "fpcheck://nope"}))).unwrap_err();
        assert_eq!(err.code, protocol::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn an_unknown_code_is_not_found() {
        let err = read(
            &server_cli(),
            Some(&json!({"uri": "fpcheck://explain/P9999"})),
        )
        .unwrap_err();
        assert_eq!(err.code, protocol::RESOURCE_NOT_FOUND);
        assert!(err.message.contains("unknown diagnostic code"), "{err:?}");
    }

    #[test]
    fn a_missing_uri_is_invalid_params() {
        let err = read(&server_cli(), None).unwrap_err();
        assert_eq!(err.code, protocol::INVALID_PARAMS);
    }

    #[test]
    fn the_template_covers_any_code() {
        let value = templates();
        assert_eq!(
            value["resourceTemplates"][0]["uriTemplate"],
            "fpcheck://explain/{code}"
        );
    }
}
