use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// MCP revision loci answers with when a client does not name one it supports.
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// Revisions whose stdio wire format loci is known to speak.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

pub const SERVER_NAME: &str = "loci";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Operational guidance injected into the agent's context alongside the tool
/// list. Kept short and sequenced, because that is what agents actually follow.
pub const SERVER_INSTRUCTIONS: &str = "\
loci answers structural questions about code from a persistent local graph.

Order of operations:
1. list_projects — get the exact `project` id. Never guess it from a directory name.
2. index_repository — only when the repository you need is absent, or index_status shows it is stale.
3. search_graph / query_graph / trace_path — structural discovery. Prefer these over grep.
4. get_code_snippet — read exact source once you have a qualified_name from search_graph.
5. search_code — literal or regex text only, or when graph coverage is insufficient.

Rules:
- Every tool needs `project`. Take it from list_projects.
- Responses are paginated. When has_more is true, page with the returned cursor before concluding.
- Before any negative or exhaustive claim (\"X does not exist\", \"nothing calls Y\"), call
  check_index_coverage for the paths or scopes involved. Coverage is best-effort and never proves
  completeness.
- CALL_UNRESOLVED edges mean a call was seen but its target could not be pinned down. Absence of a
  CALLS edge is not absence of a call.
- Empty results are reported as empty. Do not fill them in from assumption.";

#[derive(Debug, Deserialize)]
pub struct Request {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    /// Absent for notifications, which must not be answered.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub const INVALID_PARAMS: i32 = -32602;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INTERNAL_ERROR: i32 = -32603;
pub const PARSE_ERROR: i32 = -32700;

pub fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn failure(id: Value, error: ErrorObject) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": error.code,
            "message": error.message,
            "data": error.data,
        }
    })
}

/// Negotiate a protocol revision: echo the client's if we speak it, otherwise
/// answer with our default and let the client decide.
pub fn negotiate_version(requested: Option<&str>) -> String {
    match requested {
        Some(version) if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) => version.to_string(),
        _ => DEFAULT_PROTOCOL_VERSION.to_string(),
    }
}

pub fn initialize_result(requested_version: Option<&str>) -> Value {
    json!({
        "protocolVersion": negotiate_version(requested_version),
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
        },
        "instructions": SERVER_INSTRUCTIONS,
    })
}

/// A tool result. `is_error` marks a failure the model should read and react to,
/// as opposed to a transport-level JSON-RPC error.
pub fn tool_result(payload: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(payload)
        .unwrap_or_else(|_| "{\"error\":\"result could not be serialised\"}".to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// Read newline-delimited JSON-RPC from `input` and write responses to `output`.
///
/// This is the MCP stdio framing: one JSON object per line, no headers.
/// Notifications (no `id`) are handled without a reply, as the spec requires.
pub fn serve<R, W>(
    input: R,
    mut output: W,
    mut dispatch: impl FnMut(&Request) -> Value,
) -> std::io::Result<()>
where
    R: BufRead,
    W: Write,
{
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(trimmed) {
            Ok(request) => request,
            Err(e) => {
                let response = failure(
                    Value::Null,
                    ErrorObject {
                        code: PARSE_ERROR,
                        message: format!("malformed JSON-RPC message: {e}"),
                        data: None,
                    },
                );
                writeln!(output, "{response}")?;
                output.flush()?;
                continue;
            }
        };

        let is_notification = request.id.is_none();
        let response = dispatch(&request);

        if is_notification || response.is_null() {
            continue;
        }
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_echoes_a_version_we_speak() {
        assert_eq!(negotiate_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_version(Some("2024-11-05")), "2024-11-05");
    }

    #[test]
    fn negotiation_falls_back_for_unknown_versions() {
        assert_eq!(
            negotiate_version(Some("1999-01-01")),
            DEFAULT_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_version(None), DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn initialize_advertises_tools_and_instructions() {
        let result = initialize_result(Some("2025-06-18"));
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "loci");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["instructions"]
            .as_str()
            .unwrap()
            .contains("list_projects"));
    }

    #[test]
    fn notifications_get_no_response() {
        let input = std::io::Cursor::new(
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        );
        let mut output = Vec::new();
        serve(input, &mut output, |_| json!({"unused": true})).unwrap();
        assert!(output.is_empty(), "a notification must not be answered");
    }

    #[test]
    fn requests_get_exactly_one_line_of_response() {
        let input = std::io::Cursor::new(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
        );
        let mut output = Vec::new();
        serve(input, &mut output, |request| {
            success(request.id.clone().unwrap(), json!({}))
        })
        .unwrap();

        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text
            .lines()
            .all(|l| serde_json::from_str::<Value>(l).is_ok()));
    }

    #[test]
    fn malformed_input_produces_a_parse_error_not_a_crash() {
        let input = std::io::Cursor::new("not json\n");
        let mut output = Vec::new();
        serve(input, &mut output, |_| Value::Null).unwrap();

        let response: Value =
            serde_json::from_str(String::from_utf8(output).unwrap().trim()).unwrap();
        assert_eq!(response["error"]["code"], PARSE_ERROR);
    }
}
