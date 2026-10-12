//! The MCP tools: `check`, `fix`, and `explain` (`SPEC.md` 12.1).
//!
//! Every tool is a *form* over an existing core function, never a new code path:
//! `check` calls [`crate::cli::run_check`], `fix` calls [`crate::cli::run_fix`],
//! and `explain` calls [`crate::explain::render`]. Output reuses the existing
//! diagnostic model, the `llm` renderer, and the JSON summary, so the MCP
//! surface cannot report something the CLI would not.
//!
//! The `check` loop keeps exactly one mode-of-operation choice (`no_fpc`, which
//! only exists because a machine may have no compiler); everything else is
//! parameter, not a separate tool.

use crate::cli::{self, CheckRun, Cli, Command, SessionContext};
use crate::error::Error;
use crate::fpc::FpcMode;
use crate::mcp::protocol::{self, RpcError};
use crate::mcp::Server;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

pub fn list() -> Value {
    json!({
        "tools": [
            check_tool(),
            fix_tool(),
            explain_tool(),
        ]
    })
}

fn check_tool() -> Value {
    json!({
        "name": "check",
        "title": "Check Pascal sources",
        "description": "Run the fpcheck analyzer and (unless `no_fpc`) the Free Pascal compiler \
                        over a project, returning normalized, root-cause-first diagnostics. \
                        The text content is exactly `fpcheck check --message-format=llm`; \
                        `structuredContent` carries every diagnostic, uncapped, with the \
                        versioned summary the JSON renderer uses. `isError` mirrors the CLI's \
                        exit code 1 (diagnostics at error level); tool failures are reported \
                        the same way with `isError: true`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files or directories to check. Defaults to the configured `[package] sources`.",
                },
                "config": {
                    "type": "string",
                    "description": "Path to fpcheck.toml. Defaults to searching upwards from the first path.",
                },
                "no_fpc": {
                    "type": "boolean",
                    "default": false,
                    "description": "Skip the Free Pascal compiler and run the analyzer alone. Required on a machine with no `fpc`.",
                },
                "max_diagnostics": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 15,
                    "description": "Cap the text content (0 = unlimited). `structuredContent` is never capped.",
                },
                "profile": {
                    "type": "string",
                    "enum": ["dev", "release"],
                    "default": "dev",
                    "description": "Compiler profile from fpcheck.toml.",
                },
                "deny": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Diagnostic codes to force to error level.",
                },
                "warn": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Diagnostic codes to force to warning level.",
                },
                "allow": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Diagnostic codes to silence.",
                },
                "deny_warnings": {
                    "type": "boolean",
                    "default": false,
                    "description": "Treat warnings as errors.",
                }
            },
            "additionalProperties": false
        },
        "annotations": {
            "title": "Check Pascal sources",
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        }
    })
}

fn fix_tool() -> Value {
    json!({
        "name": "fix",
        "title": "Apply machine-applicable fixes",
        "description": "Apply every MachineApplicable suggestion from the analyzer, in the same \
                        order `fpcheck fix` uses. Use `dry_run: true` first to see what would \
                        change: it reports the files and counts without writing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files or directories to fix. Defaults to the configured `[package] sources`.",
                },
                "config": {
                    "type": "string",
                    "description": "Path to fpcheck.toml. Defaults to searching upwards from the first path.",
                },
                "dry_run": {
                    "type": "boolean",
                    "default": false,
                    "description": "Report what would change without writing anything.",
                }
            },
            "additionalProperties": false
        },
        "annotations": {
            "title": "Apply machine-applicable fixes",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": true,
            "openWorldHint": false,
        }
    })
}

fn explain_tool() -> Value {
    json!({
        "name": "explain",
        "title": "Explain a diagnostic code",
        "description": "Long-form explanation of a stable code — what it means, why it matters, \
                        and a failing and fixed example. Works for analyzer `P` codes and the \
                        compiler's `F` codes. The same text is available as the resource \
                        `fpcheck://explain/{code}`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "A stable diagnostic code, e.g. `P0702` or `F5000`.",
                },
                "config": {
                    "type": "string",
                    "description": "Path to fpcheck.toml, for a project that overrides the catalog or ships docs/.",
                }
            },
            "required": ["code"],
            "additionalProperties": false
        },
        "annotations": {
            "title": "Explain a diagnostic code",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        }
    })
}

// ---------------------------------------------------------------------------
// tools/call
// ---------------------------------------------------------------------------

pub fn call(server: &Server, cli: &Cli, params: Option<&Value>) -> Result<Value, RpcError> {
    let params = params
        .ok_or_else(|| RpcError::invalid_params("tools/call requires `name` and `arguments`"))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("tools/call requires a string `name`"))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "check" => Ok(check(server, cli, parse(name, &arguments)?)),
        "fix" => Ok(fix(server, cli, parse(name, &arguments)?)),
        "explain" => explain(server, cli, parse(name, &arguments)?),
        other => Err(RpcError::invalid_params(format!(
            "unknown tool `{other}` (expected check, fix, or explain)"
        ))),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(tool: &str, arguments: &Value) -> Result<T, RpcError> {
    serde_json::from_value(arguments.clone())
        .map_err(|e| RpcError::invalid_params(format!("invalid arguments for `{tool}`: {e}")))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CheckArgs {
    paths: Vec<String>,
    config: Option<String>,
    no_fpc: bool,
    max_diagnostics: Option<usize>,
    profile: Option<String>,
    deny: Vec<String>,
    warn: Vec<String>,
    allow: Vec<String>,
    deny_warnings: bool,
}

fn check(server: &Server, server_cli: &Cli, args: CheckArgs) -> Value {
    let no_fpc = args.no_fpc;
    let mut cli = Cli::with_command(Command::Check {
        no_fpc,
        report_allows: false,
    });
    apply_common(&mut cli, server_cli, args.config, args.paths);
    // The agent-facing format; `structuredContent` carries the full set.
    cli.message_format = "llm".to_string();
    cli.max_diagnostics = args.max_diagnostics.unwrap_or(server_cli.max_diagnostics);
    cli.profile = args.profile.unwrap_or_else(|| server_cli.profile.clone());
    cli.deny = args.deny;
    cli.warn = args.warn;
    cli.allow = args.allow;
    cli.deny_warnings = args.deny_warnings;

    match cli::run_check(&cli, FpcMode::CheckOnly, no_fpc) {
        Ok(run) => {
            let CheckRun {
                diagnostics,
                fatal,
                rendered,
                exit_code,
                ..
            } = run;
            let structured = json!({
                "summary": crate::render::json::summary_value(&diagnostics),
                "fatal": fatal,
                "clean": exit_code == 0,
                "exit_code": exit_code,
                "diagnostics": diagnostics,
            });
            tool_result(server, rendered, Some(structured), exit_code != 0)
        }
        Err(error) => tool_error(describe_error(&error)),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FixArgs {
    paths: Vec<String>,
    config: Option<String>,
    dry_run: Option<bool>,
}

fn fix(server: &Server, server_cli: &Cli, args: FixArgs) -> Value {
    let dry_run = args.dry_run.unwrap_or(false);
    let mut cli = Cli::with_command(Command::Fix { dry_run });
    apply_common(&mut cli, server_cli, args.config, args.paths);

    match cli::run_fix(&cli, dry_run) {
        Ok(run) => {
            let applied = run.applied();
            let skipped = run.skipped();
            let changed: Vec<String> = run
                .changed_files()
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            let mut text = run.summary.clone();
            if dry_run {
                for path in &changed {
                    text.push_str(&format!("\n--- {path}"));
                }
            }
            let structured = json!({
                "dry_run": dry_run,
                "applied": applied,
                "skipped": skipped,
                "changed_files": changed,
                "summary": run.summary,
            });
            tool_result(server, text, Some(structured), false)
        }
        Err(error) => tool_error(describe_error(&error)),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ExplainArgs {
    code: String,
    config: Option<String>,
}

fn explain(server: &Server, server_cli: &Cli, args: ExplainArgs) -> Result<Value, RpcError> {
    let code = args.code.trim().to_ascii_uppercase();
    if code.is_empty() {
        return Err(RpcError::invalid_params(
            "`explain` requires a non-empty `code`",
        ));
    }

    let mut cli = Cli::with_command(Command::Mcp);
    apply_common(&mut cli, server_cli, args.config, Vec::new());

    let SessionContext { root, catalog } = cli::session_context(&cli)
        .map_err(|e| RpcError::new(protocol::INTERNAL_ERROR, describe_error(&e)))?;

    match crate::explain::render(&code, &catalog, &root.join("docs")) {
        Ok(markdown) => {
            let structured = json!({ "code": code, "markdown": markdown });
            Ok(tool_result(
                server,
                markdown.clone(),
                Some(structured),
                false,
            ))
        }
        // An unknown code is feedback for the caller, not a protocol failure.
        Err(error) => Ok(tool_error(describe_error(&error))),
    }
}

/// Copy the connection-level context (`--config`, positional paths) and the
/// per-call overrides onto a freshly built `Cli`.
fn apply_common(cli: &mut Cli, server_cli: &Cli, config: Option<String>, paths: Vec<String>) {
    cli.config = config
        .map(PathBuf::from)
        .or_else(|| server_cli.config.clone());
    cli.paths = if paths.is_empty() {
        server_cli.paths.clone()
    } else {
        paths.into_iter().map(PathBuf::from).collect()
    };
}

/// A tool *execution* result: failure travels here with `isError`, per the MCP
/// split between protocol errors and tool errors.
fn tool_result(server: &Server, text: String, structured: Option<Value>, is_error: bool) -> Value {
    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    // `structuredContent` only exists from 2025-06-18 on; older clients get text.
    if server.supports_structured() {
        if let Some(structured) = structured {
            result["structuredContent"] = structured;
        }
    }
    result
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
    })
}

/// Make a core error actionable where an agent will first meet it: a missing
/// compiler is the one failure with an obvious next step.
fn describe_error(error: &Error) -> String {
    let mut text = format!("fpcheck: {error}");
    if matches!(error, Error::FpcNotFound(_)) {
        text.push_str(
            "\nhelp: install Free Pascal, set FPCHECK_FPC or `[fpc] binary`, or call `check` \
             with `no_fpc: true` for analyzer-only diagnostics",
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_exposes_the_three_tools_with_object_schemas() {
        let value = list();
        let tools = value["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["check", "fix", "explain"]);
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            assert!(tool["description"].as_str().unwrap().len() > 40, "{tool}");
            assert!(tool["annotations"]["title"].is_string(), "{tool}");
        }
    }

    #[test]
    fn explain_requires_a_code_and_rejects_unknown_arguments() {
        let server = Server::new();
        let cli = Cli::with_command(Command::Mcp);
        assert_eq!(
            call(
                &server,
                &cli,
                Some(&json!({"name": "explain", "arguments": {}}))
            )
            .unwrap_err()
            .code,
            protocol::INVALID_PARAMS
        );
        assert_eq!(
            call(
                &server,
                &cli,
                Some(&json!({"name": "explain", "arguments": {"code": "P0702", "typo": 1}})),
            )
            .unwrap_err()
            .code,
            protocol::INVALID_PARAMS
        );
    }

    #[test]
    fn an_unknown_tool_is_a_protocol_error() {
        let server = Server::new();
        let cli = Cli::with_command(Command::Mcp);
        let err = call(
            &server,
            &cli,
            Some(&json!({"name": "nope", "arguments": {}})),
        )
        .unwrap_err();
        assert_eq!(err.code, protocol::INVALID_PARAMS);
        assert!(err.message.contains("unknown tool"), "{err:?}");
    }

    #[test]
    fn describe_error_points_at_no_fpc_for_a_missing_compiler() {
        let error = Error::FpcNotFound("`fpc` was not found".into());
        let text = describe_error(&error);
        assert!(text.contains("no_fpc: true"), "{text}");
        // Other errors are not given a compiler hint.
        let other = describe_error(&Error::Usage("bad code".into()));
        assert!(!other.contains("no_fpc"), "{other}");
    }
}
