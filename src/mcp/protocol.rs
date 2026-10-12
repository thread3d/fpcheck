//! JSON-RPC 2.0 envelopes and MCP constants for the stdio transport.
//!
//! The wire format is newline-delimited JSON, one message per line, which is
//! what the MCP stdio transport specifies. Only the shapes this server actually
//! sends or receives are modelled; unknown fields from a newer client are
//! ignored rather than rejected.

use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

/// The newest MCP revision this server implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Revisions this server can speak, newest first.
///
/// `initialize` echoes the client's choice when it is one of these and answers
/// with [`PROTOCOL_VERSION`] otherwise, as the protocol requires.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The revision that introduced `structuredContent` on tool results. Older
/// clients get text content only.
pub const STRUCTURED_CONTENT_VERSION: &str = "2025-06-18";

// JSON-RPC 2.0 error codes.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// MCP-defined "resource not found" code.
pub const RESOURCE_NOT_FOUND: i32 = -32002;

/// An incoming JSON-RPC message.
#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    /// Absent for a notification; a JSON `null` id is still a request.
    #[serde(default, deserialize_with = "deserialize_id")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// Deserialize an `id` member while preserving a JSON `null`.
///
/// `Option<Value>` alone folds `null` into `None`, which would make an
/// `"id": null` request indistinguishable from a notification and leave it
/// unanswered. JSON-RPC discourages a null id but does not forbid it, so only a
/// *missing* member means "notification".
fn deserialize_id<'de, D>(deserializer: D) -> std::result::Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Some(Value::deserialize(deserializer)?))
}

impl Request {
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A JSON-RPC error to return to the client.
///
/// Protocol-level failures (a malformed call, an unknown tool, a missing
/// resource) use this. A *tool execution* failure is different: it travels in a
/// normal result with `isError: true`, and the MCP server code builds that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        RpcError::new(INVALID_PARAMS, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        RpcError::new(RESOURCE_NOT_FOUND, message)
    }
}

pub fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn failure(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_an_id_is_a_notification() {
        let r: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();
        assert!(r.is_notification());
        assert!(r.id.is_none());
    }

    #[test]
    fn a_null_id_is_a_request_not_a_notification() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();
        assert!(!r.is_notification());
        assert_eq!(r.id, Some(Value::Null));
    }

    #[test]
    fn string_and_numeric_ids_both_parse() {
        let a: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#).unwrap();
        assert_eq!(a.id, Some(json!("abc")));
        let b: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).unwrap();
        assert_eq!(b.id, Some(json!(7)));
    }

    #[test]
    fn success_and_failure_shapes() {
        let ok = success(json!(1), json!({"a": 1}));
        assert_eq!(ok["jsonrpc"], "2.0");
        assert_eq!(ok["result"]["a"], 1);
        assert!(ok.get("error").is_none());

        let err = failure(Value::Null, PARSE_ERROR, "bad");
        assert_eq!(err["id"], Value::Null);
        assert_eq!(err["error"]["code"], PARSE_ERROR);
        assert_eq!(err["error"]["message"], "bad");
    }

    #[test]
    fn the_newest_version_is_the_first_supported_one() {
        assert_eq!(SUPPORTED_VERSIONS[0], PROTOCOL_VERSION);
        assert!(SUPPORTED_VERSIONS.contains(&STRUCTURED_CONTENT_VERSION));
    }
}
