//! The MCP (Model Context Protocol) server (`SPEC.md` 12.1).
//!
//! This is a thin adapter, not a second implementation. It speaks newline-delimited
//! JSON-RPC 2.0 on stdio and calls the *same* core functions the CLI calls —
//! [`crate::cli::run_check`], [`crate::cli::run_fix`], and
//! [`crate::explain::render`] — so the edit → check → fix loop cannot drift
//! between the two front ends. The `check` tool returns exactly the `llm`
//! renderer's text plus the existing diagnostic model, uncapped.
//!
//! Two rules keep the transport usable:
//!
//! * **stdout carries protocol messages only.** Nothing in the request path
//!   prints; human-facing text belongs to the tool results.
//! * **A tool execution failure is not a protocol error.** A missing compiler or
//!   an unknown diagnostic code comes back as a normal result with
//!   `isError: true`, which is what lets the agent read the message and adapt.
//!
//! The server holds no cache yet (milestone M4); each call resolves config and
//! the catalog afresh, which is correct, if not yet fast.

pub mod protocol;
mod resources;
mod tools;

use crate::cli::Cli;
use crate::error::{Error, Result};
use protocol::Request;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// The revision agreed when the client asks for an unknown one.
pub use protocol::PROTOCOL_VERSION;

/// Guidance returned by `initialize` — the MCP rendering of the `AGENTS.md`
/// snippet in spec section 12.
pub const INSTRUCTIONS: &str = "\
fpcheck is a Rust-style feedback layer for Free Pascal. Call `check` after every \
edit and fix root causes first; its text content is `fpcheck check \
--message-format=llm` and its structuredContent carries every diagnostic, \
uncapped. Call `explain` when a code is unclear. Never silence a diagnostic \
without a real reason string.";

/// Per-connection protocol state.
#[derive(Debug, Default)]
pub struct Server {
    /// The revision agreed in `initialize`, if the client sent it.
    pub protocol_version: Option<String>,
    pub initialized: bool,
}

impl Server {
    pub fn new() -> Self {
        Server::default()
    }

    /// Whether tool results may carry `structuredContent`, which only exists
    /// from revision [`protocol::STRUCTURED_CONTENT_VERSION`] on.
    pub(crate) fn supports_structured(&self) -> bool {
        self.protocol_version.as_deref() == Some(protocol::STRUCTURED_CONTENT_VERSION)
    }
}

/// Serve MCP on stdin/stdout until the client closes stdin.
///
/// `cli` carries the connection-level context (`--config`, positional paths),
/// which per-call arguments can override.
pub fn serve_stdio(cli: &Cli) -> Result<i32> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = Server::new();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line.map_err(|e| Error::io("stdin", e))?;
        let Some(response) = handle_line(&mut server, cli, &line) else {
            continue;
        };
        let text = serde_json::to_string(&response)
            .map_err(|e| Error::internal(format!("could not serialize a response: {e}")))?;
        writeln!(out, "{text}").map_err(|e| Error::io("stdout", e))?;
        out.flush().map_err(|e| Error::io("stdout", e))?;
    }

    Ok(0)
}

/// Handle one newline-delimited message.
///
/// `None` means the message was a notification (or blank) and must not be
/// answered.
pub fn handle_line(server: &mut Server, cli: &Cli, line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }

    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => {
            return Some(protocol::failure(
                Value::Null,
                protocol::PARSE_ERROR,
                format!("invalid JSON: {e}"),
            ));
        }
    };

    if request.is_notification() {
        handle_notification(server, &request);
        return None;
    }

    let id = request.id.clone().unwrap_or(Value::Null);
    if let Some(version) = &request.jsonrpc {
        if version != "2.0" {
            return Some(protocol::failure(
                id,
                protocol::INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
    }

    Some(dispatch(
        server,
        cli,
        &id,
        &request.method,
        request.params.as_ref(),
    ))
}

fn dispatch(
    server: &mut Server,
    cli: &Cli,
    id: &Value,
    method: &str,
    params: Option<&Value>,
) -> Value {
    let id = id.clone();
    match method {
        "initialize" => protocol::success(id, initialize(server, params)),
        "ping" => protocol::success(id, json!({})),
        "tools/list" => protocol::success(id, tools::list()),
        "tools/call" => match tools::call(server, cli, params) {
            Ok(result) => protocol::success(id, result),
            Err(error) => protocol::failure(id, error.code, error.message),
        },
        "resources/list" => match resources::list(cli) {
            Ok(result) => protocol::success(id, result),
            Err(error) => protocol::failure(id, protocol::INTERNAL_ERROR, error.to_string()),
        },
        "resources/templates/list" => protocol::success(id, resources::templates()),
        "resources/read" => match resources::read(cli, params) {
            Ok(result) => protocol::success(id, result),
            Err(error) => protocol::failure(id, error.code, error.message),
        },
        other => protocol::failure(
            id,
            protocol::METHOD_NOT_FOUND,
            format!("unknown method `{other}`"),
        ),
    }
}

/// `initialize`: agree a revision and advertise what the server can do.
fn initialize(server: &mut Server, params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let agreed = match requested {
        Some(version) if protocol::SUPPORTED_VERSIONS.contains(&version) => version.to_string(),
        // An unknown revision: answer with the newest we speak and let the
        // client decide whether to continue, as the protocol requires.
        _ => protocol::PROTOCOL_VERSION.to_string(),
    };
    server.protocol_version = Some(agreed.clone());
    server.initialized = true;

    json!({
        "protocolVersion": agreed,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
        },
        "serverInfo": {
            "name": "fpcheck",
            "title": "fpcheck",
            "version": crate::version(),
        },
        "instructions": INSTRUCTIONS,
    })
}

fn handle_notification(server: &mut Server, request: &Request) {
    if request.method == "notifications/initialized" {
        server.initialized = true;
    }
    // `notifications/cancelled`, `notifications/roots/list_changed`, and unknown
    // notifications are ignored: there is no long-running work to cancel.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Command;

    const DEMO: &str = "{$mode objfpc}\nunit demo;\ninterface\nimplementation\nbegin\n  {$R-}\n  with Foo do Bar;\nend.\nend.\n";

    fn server_cli() -> Cli {
        Cli::with_command(Command::Mcp)
    }

    fn respond(server: &mut Server, cli: &Cli, message: &str) -> Value {
        handle_line(server, cli, message).expect("expected a response")
    }

    fn call(server: &mut Server, cli: &Cli, name: &str, arguments: Value) -> Value {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        })
        .to_string();
        respond(server, cli, &message)
    }

    /// A temporary project with one offending file and an explicit config.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo.pas"), DEMO).unwrap();
        std::fs::write(dir.path().join("fpcheck.toml"), "[package]\n").unwrap();
        dir
    }

    fn project_cli(dir: &tempfile::TempDir) -> Cli {
        let mut cli = server_cli();
        cli.config = Some(dir.path().join("fpcheck.toml"));
        cli
    }

    fn demo_path(dir: &tempfile::TempDir) -> String {
        dir.path().join("demo.pas").display().to_string()
    }

    #[test]
    fn initialize_advertises_tools_and_resources() {
        let mut server = Server::new();
        let cli = server_cli();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "fpcheck");
        assert!(reply["result"]["capabilities"]["tools"].is_object());
        assert!(reply["result"]["capabilities"]["resources"].is_object());
        assert!(reply["result"]["instructions"].as_str().unwrap().len() > 40);
        assert!(server.initialized);
    }

    #[test]
    fn an_unknown_protocol_version_falls_back_to_the_newest_supported() {
        let mut server = Server::new();
        let cli = server_cli();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn notifications_are_never_answered() {
        let mut server = Server::new();
        let cli = server_cli();
        for method in [
            "notifications/initialized",
            "notifications/cancelled",
            "notifications/roots/list_changed",
        ] {
            let message = json!({ "jsonrpc": "2.0", "method": method }).to_string();
            assert!(
                handle_line(&mut server, &cli, &message).is_none(),
                "{method}"
            );
        }
        assert!(handle_line(&mut server, &cli, "   ").is_none());
    }

    #[test]
    fn initialized_notification_sets_the_flag() {
        let mut server = Server::new();
        let cli = server_cli();
        handle_line(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert!(server.initialized);
    }

    #[test]
    fn malformed_json_is_a_parse_error_with_a_null_id() {
        let mut server = Server::new();
        let cli = server_cli();
        let reply = respond(&mut server, &cli, "{not json");
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["error"]["code"], protocol::PARSE_ERROR);
    }

    #[test]
    fn a_bad_jsonrpc_version_is_an_invalid_request() {
        let mut server = Server::new();
        let cli = server_cli();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
        );
        assert_eq!(reply["error"]["code"], protocol::INVALID_REQUEST);
        assert_eq!(reply["id"], 1);
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let mut server = Server::new();
        let cli = server_cli();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":"x","method":"completion/complete"}"#,
        );
        assert_eq!(reply["error"]["code"], protocol::METHOD_NOT_FOUND);
        assert_eq!(reply["id"], "x");
    }

    #[test]
    fn ping_returns_an_empty_result() {
        let mut server = Server::new();
        let cli = server_cli();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
        );
        assert_eq!(reply["result"], json!({}));
    }

    #[test]
    fn tools_list_round_trips_through_the_protocol() {
        let mut server = Server::new();
        let cli = server_cli();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        );
        let tools = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn check_reports_diagnostics_in_text_and_structured_content() {
        let dir = project();
        let cli = project_cli(&dir);
        let mut server = Server::new();
        // Negotiate the newest revision so structuredContent is included.
        respond(
            &mut server,
            &cli,
            &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION}}).to_string(),
        );

        let reply = call(
            &mut server,
            &cli,
            "check",
            json!({"paths": [demo_path(&dir)], "no_fpc": true}),
        );
        let result = &reply["result"];
        assert_eq!(result["isError"], true, "errors must set isError: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("error[P0304]"), "{text}");
        assert!(text.contains("error[P0702]"), "{text}");
        assert!(text.contains("fpcheck: "), "{text}");

        let structured = &result["structuredContent"];
        assert_eq!(structured["summary"]["schema"], crate::JSON_SCHEMA_VERSION);
        assert!(structured["summary"]["errors"].as_u64().unwrap() >= 2);
        assert_eq!(structured["clean"], false);
        assert_eq!(structured["exit_code"], 1);
        let codes: Vec<&str> = structured["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["code"].as_str().unwrap())
            .collect();
        assert!(codes.contains(&"P0702"), "{codes:?}");
    }

    #[test]
    fn structured_content_is_omitted_for_an_older_client() {
        let dir = project();
        let cli = project_cli(&dir);
        let mut server = Server::new();
        respond(
            &mut server,
            &cli,
            &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}).to_string(),
        );

        let reply = call(
            &mut server,
            &cli,
            "check",
            json!({"paths": [demo_path(&dir)], "no_fpc": true}),
        );
        let result = &reply["result"];
        assert!(result["structuredContent"].is_null(), "{result}");
        // The text is still the full llm output.
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("P0702"));
    }

    #[test]
    fn check_on_clean_code_is_not_an_error() {
        let dir = project();
        std::fs::write(
            dir.path().join("clean.pas"),
            "{$mode objfpc}\nunit clean;\ninterface\nimplementation\nend.\n",
        )
        .unwrap();
        let cli = project_cli(&dir);
        let mut server = Server::new();
        let reply = call(
            &mut server,
            &cli,
            "check",
            json!({"paths": [dir.path().join("clean.pas").display().to_string()], "no_fpc": true}),
        );
        let result = &reply["result"];
        assert_eq!(result["isError"], false, "{result}");
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("0 errors"));
    }

    #[test]
    fn check_max_diagnostics_caps_the_text_but_not_structured_content() {
        let dir = project();
        let cli = project_cli(&dir);
        let mut server = Server::new();
        respond(
            &mut server,
            &cli,
            &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION}}).to_string(),
        );
        let reply = call(
            &mut server,
            &cli,
            "check",
            json!({"paths": [demo_path(&dir)], "no_fpc": true, "max_diagnostics": 1}),
        );
        let result = &reply["result"];
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("more diagnostic(s) omitted"));
        // The machine view is never truncated.
        assert!(
            result["structuredContent"]["diagnostics"]
                .as_array()
                .unwrap()
                .len()
                > 1
        );
    }

    #[test]
    fn a_missing_compiler_is_a_tool_error_with_a_hint() {
        if std::env::var_os("FPCHECK_FPC").is_some() {
            // The environment override would win over the bogus binary.
            return;
        }
        let dir = project();
        std::fs::write(
            dir.path().join("fpcheck.toml"),
            "[package]\n\n[fpc]\nbinary = \"/nonexistent/fpc\"\n",
        )
        .unwrap();
        let cli = project_cli(&dir);
        let mut server = Server::new();
        let reply = call(
            &mut server,
            &cli,
            "check",
            json!({"paths": [demo_path(&dir)]}),
        );
        let result = &reply["result"];
        assert_eq!(result["isError"], true, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("no_fpc: true"), "{text}");
        // A tool failure is not a JSON-RPC failure.
        assert!(reply.get("error").is_none(), "{reply}");
    }

    #[test]
    fn fix_honours_dry_run_then_writes() {
        let dir = project();
        let cli = project_cli(&dir);

        let mut server = Server::new();
        let reply = call(
            &mut server,
            &cli,
            "fix",
            json!({"paths": [demo_path(&dir)], "dry_run": true}),
        );
        let result = &reply["result"];
        assert_eq!(result["isError"], false, "{result}");
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("would apply 1 edit(s)"));
        assert!(
            std::fs::read_to_string(dir.path().join("demo.pas"))
                .unwrap()
                .contains("{$R-}"),
            "dry run must not write"
        );

        let reply = call(
            &mut server,
            &cli,
            "fix",
            json!({"paths": [demo_path(&dir)]}),
        );
        let result = &reply["result"];
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("applied 1 edit(s)"));
        assert!(
            !std::fs::read_to_string(dir.path().join("demo.pas"))
                .unwrap()
                .contains("{$R-}"),
            "fix must write"
        );
    }

    #[test]
    fn explain_returns_the_catalog_summary_and_the_project_docs() {
        let dir = project();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/P0702.md"), "# project docs\n").unwrap();
        let cli = project_cli(&dir);
        let mut server = Server::new();
        respond(
            &mut server,
            &cli,
            &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION}}).to_string(),
        );

        let reply = call(&mut server, &cli, "explain", json!({"code": "p0702"}));
        let result = &reply["result"];
        assert_eq!(result["isError"], false, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("P0702: with-statement"), "{text}");
        assert!(text.contains("`with` is banned"), "{text}");
        assert!(text.contains("# project docs"), "{text}");
        // The code is normalized to upper case and echoed in structuredContent.
        assert_eq!(result["structuredContent"]["code"], "P0702");
        assert_eq!(result["structuredContent"]["markdown"], text);
    }

    #[test]
    fn explain_rejects_an_empty_code_as_invalid_params() {
        let cli = server_cli();
        let mut server = Server::new();
        let reply = call(&mut server, &cli, "explain", json!({"code": "  "}));
        assert_eq!(reply["error"]["code"], protocol::INVALID_PARAMS);
    }

    #[test]
    fn explain_of_an_unknown_code_is_a_tool_error() {
        let cli = server_cli();
        let mut server = Server::new();
        let reply = call(&mut server, &cli, "explain", json!({"code": "P9999"}));
        let result = &reply["result"];
        assert_eq!(result["isError"], true, "{result}");
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown diagnostic code"));
    }

    #[test]
    fn resources_list_and_read_round_trip() {
        let dir = project();
        let cli = project_cli(&dir);
        let mut server = Server::new();

        let list = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
        );
        let uris: Vec<&str> = list["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&resources::CATALOG_LINTS_URI));
        assert!(uris.contains(&"fpcheck://explain/P0702"));

        let read = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"fpcheck://explain/P0702"}}"#,
        );
        assert!(read["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("with-statement"));

        let templates = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":3,"method":"resources/templates/list"}"#,
        );
        assert_eq!(
            templates["result"]["resourceTemplates"][0]["uriTemplate"],
            "fpcheck://explain/{code}"
        );
    }

    #[test]
    fn reading_a_missing_resource_is_a_jsonrpc_error() {
        let cli = server_cli();
        let mut server = Server::new();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"fpcheck://nope"}}"#,
        );
        assert_eq!(reply["error"]["code"], protocol::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn tools_call_without_params_is_invalid_params() {
        let cli = server_cli();
        let mut server = Server::new();
        let reply = respond(
            &mut server,
            &cli,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#,
        );
        assert_eq!(reply["error"]["code"], protocol::INVALID_PARAMS);
    }
}
