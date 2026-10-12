//! End-to-end tests for `fpcheck mcp` over its real stdio transport.
//!
//! These drive the built binary, so they cover framing, the guarantee that
//! stdout carries protocol messages and nothing else, and — the important one —
//! that the MCP `check` tool returns byte-identical text to
//! `fpcheck check --message-format=llm`. `--no-fpc` is used throughout, as in
//! `tests/cli.rs`, because this repository is developed without Free Pascal.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const DEMO: &str = r#"{$mode objfpc}
unit demo;

interface

procedure Run(x: Double);

implementation

procedure Run(x: Double);
begin
  {$R-}
  with Foo do Bar;
  if x = 0.1 then Writeln('hi');
end;

end.
"#;

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, contents) in files {
            std::fs::write(dir.path().join(name), contents).unwrap();
        }
        Project { dir }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path(relative)).expect("read fixture")
    }

    /// Run a normal CLI command in the project directory.
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_fpcheck"))
            .args(args)
            .current_dir(self.dir.path())
            .output()
            .expect("run fpcheck");
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.code().unwrap_or(-1), text)
    }

    /// Speak newline-delimited JSON to `fpcheck mcp` and collect the replies.
    ///
    /// Every stdout line must parse as JSON: a stray `println!` anywhere in the
    /// request path would break the transport, so it fails the test here.
    fn speak(&self, lines: &[String]) -> (i32, Vec<serde_json::Value>, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fpcheck"))
            .arg("mcp")
            .current_dir(self.dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fpcheck mcp");

        {
            let stdin = child.stdin.as_mut().expect("stdin");
            for line in lines {
                writeln!(stdin, "{line}").expect("write request");
            }
        }
        // Closing stdin is the shutdown signal; the server drains and exits 0.
        drop(child.stdin.take());

        let output = child.wait_with_output().expect("wait for fpcheck mcp");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        let mut responses = Vec::new();
        for (index, line) in stdout.lines().enumerate() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("stdout line {index} is not JSON ({e}): {line:?}\nfull stdout:\n{stdout}")
            });
            responses.push(value);
        }
        (output.status.code().unwrap_or(-1), responses, stderr)
    }
}

fn request(id: i64, method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
}

fn initialize(id: i64) -> String {
    request(
        id,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "fpcheck-tests", "version": "1"},
        }),
    )
}

fn by_id(responses: &[serde_json::Value], id: i64) -> &serde_json::Value {
    responses
        .iter()
        .find(|r| r["id"] == serde_json::json!(id))
        .unwrap_or_else(|| panic!("no response with id {id}: {responses:?}"))
}

fn tool_call(id: i64, name: &str, arguments: serde_json::Value) -> String {
    request(
        id,
        "tools/call",
        serde_json::json!({ "name": name, "arguments": arguments }),
    )
}

fn demo() -> Project {
    Project::new(&[("demo.pas", DEMO)])
}

#[test]
fn initialize_then_list_tools_over_stdio() {
    let project = demo();
    let (code, responses, stderr) = project.speak(&[
        initialize(1),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        request(2, "tools/list", serde_json::json!({})),
        request(3, "ping", serde_json::json!({})),
    ]);

    assert_eq!(code, 0, "server must exit 0 at EOF");
    assert!(stderr.is_empty(), "unexpected stderr:\n{stderr}");
    // The notification is not answered: three requests, three replies.
    assert_eq!(responses.len(), 3, "{responses:?}");

    let init = by_id(&responses, 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "fpcheck");
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("check"));

    let tools = by_id(&responses, 2)["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["check", "fix", "explain"]);

    assert_eq!(by_id(&responses, 3)["result"], serde_json::json!({}));
}

#[test]
fn check_tool_text_is_identical_to_the_cli_llm_output() {
    let project = demo();

    let (cli_code, cli_text) =
        project.cli(&["check", "--no-fpc", "--message-format=llm", "demo.pas"]);
    assert_eq!(cli_code, 1, "{cli_text}");

    let (code, responses, _) = project.speak(&[
        initialize(1),
        tool_call(
            2,
            "check",
            serde_json::json!({ "paths": ["demo.pas"], "no_fpc": true }),
        ),
    ]);
    assert_eq!(code, 0);

    let result = &by_id(&responses, 2)["result"];
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text, cli_text,
        "the MCP check tool and the CLI must render identically"
    );
    assert_eq!(result["isError"], true, "exit code 1 maps to isError");

    let structured = &result["structuredContent"];
    assert_eq!(structured["exit_code"], cli_code);
    assert_eq!(structured["clean"], false);
    assert_eq!(
        structured["summary"]["errors"].as_u64().unwrap(),
        2,
        "{structured}"
    );
    assert_eq!(structured["summary"]["warnings"], 1);
    assert_eq!(structured["summary"]["total"], 3);
    let codes: Vec<&str> = structured["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"P0702"), "{codes:?}");
}

#[test]
fn fix_tool_writes_and_a_recheck_is_clean() {
    let project = demo();
    assert!(project.read("demo.pas").contains("{$R-}"));

    let (code, responses, _) = project.speak(&[
        initialize(1),
        tool_call(2, "fix", serde_json::json!({ "paths": ["demo.pas"] })),
        tool_call(
            3,
            "check",
            serde_json::json!({ "paths": ["demo.pas"], "no_fpc": true }),
        ),
    ]);
    assert_eq!(code, 0);

    let fixed = &by_id(&responses, 2)["result"];
    assert_eq!(fixed["isError"], false, "{fixed}");
    assert!(fixed["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("applied 1 edit(s)"));
    assert!(!project.read("demo.pas").contains("{$R-}"));

    let after = &by_id(&responses, 3)["result"];
    let text = after["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("P0304"), "P0304 should be fixed:\n{text}");
    assert!(text.contains("P0702"), "other lints must survive:\n{text}");
}

#[test]
fn resources_are_reachable_over_stdio() {
    let project = demo();
    let (code, responses, _) = project.speak(&[
        initialize(1),
        request(2, "resources/list", serde_json::json!({})),
        request(
            3,
            "resources/read",
            serde_json::json!({ "uri": "fpcheck://explain/P0702" }),
        ),
        request(
            4,
            "resources/read",
            serde_json::json!({ "uri": "fpcheck://catalog/lints" }),
        ),
    ]);
    assert_eq!(code, 0);

    let uris: Vec<String> = by_id(&responses, 2)["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_string())
        .collect();
    assert!(uris.contains(&"fpcheck://explain/P0702".to_string()));
    assert!(uris.contains(&"fpcheck://catalog/lints".to_string()));

    let explain = by_id(&responses, 3)["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    assert!(explain.contains("P0702: with-statement"), "{explain}");

    let lints = by_id(&responses, 4)["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    assert!(lints.contains("P0702"), "lint catalog must carry P0702");
}

#[test]
fn a_malformed_line_is_reported_and_the_server_keeps_serving() {
    let project = demo();
    let (code, responses, _) = project.speak(&[
        "{ this is not json".to_string(),
        request(2, "ping", serde_json::json!({})),
    ]);
    assert_eq!(code, 0);
    assert_eq!(responses.len(), 2);

    let parse_error = &responses[0];
    assert_eq!(parse_error["id"], serde_json::Value::Null);
    assert_eq!(parse_error["error"]["code"], -32700);
    assert_eq!(by_id(&responses, 2)["result"], serde_json::json!({}));
}

#[test]
fn a_missing_compiler_is_a_tool_error_not_a_transport_failure() {
    if std::env::var_os("FPCHECK_FPC").is_some() {
        // The environment override would win over the bogus binary.
        return;
    }
    let project = Project::new(&[
        ("demo.pas", DEMO),
        (
            "broken.toml",
            "[package]\n\n[fpc]\nbinary = \"/nonexistent/fpc\"\n",
        ),
    ]);

    let (code, responses, _) = project.speak(&[
        initialize(1),
        tool_call(
            2,
            "check",
            serde_json::json!({ "paths": ["demo.pas"], "config": "broken.toml" }),
        ),
    ]);
    assert_eq!(code, 0, "the server itself must still exit cleanly");

    let reply = by_id(&responses, 2);
    assert!(reply.get("error").is_none(), "must not be a JSON-RPC error");
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("no_fpc: true"));
}

#[test]
fn unknown_tools_and_methods_are_protocol_errors() {
    let project = demo();
    let (code, responses, _) = project.speak(&[
        initialize(1),
        tool_call(2, "nope", serde_json::json!({})),
        request(3, "completion/complete", serde_json::json!({})),
    ]);
    assert_eq!(code, 0);
    assert_eq!(by_id(&responses, 2)["error"]["code"], -32602);
    assert_eq!(by_id(&responses, 3)["error"]["code"], -32601);
}

#[test]
fn the_subcommand_is_discoverable() {
    let project = Project::new(&[]);
    let (code, out) = project.cli(&["--help"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("mcp"), "{out}");
    let (code, out) = project.cli(&["mcp", "--help"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Model Context Protocol"), "{out}");
}
