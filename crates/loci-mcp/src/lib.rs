//! The loci MCP surface: 15 tools and the stdio server Cursor talks to.
//!
//! The transport is JSON-RPC 2.0 over newline-delimited stdin/stdout, which is
//! exactly what the MCP stdio specification defines. It is implemented directly
//! rather than through an SDK so the handshake stays under our control and the
//! binary keeps no async runtime.

pub mod handlers;
pub mod journal;
pub mod protocol;
pub mod tools;

use protocol::{ErrorObject, Request};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub use protocol::{SERVER_INSTRUCTIONS, SERVER_NAME, SERVER_VERSION};

/// Execute one tool call, journalling it. Returns the payload and whether the
/// call failed, so the caller can set `isError`.
pub fn call_tool(name: &str, args: &Value) -> (Value, bool) {
    let started = std::time::Instant::now();
    let outcome = handlers::dispatch(name, args);
    let duration_ms = started.elapsed().as_millis() as u64;

    // Journalling must never break a tool call.
    let _ = journal::record(&journal::entry_for(name, args, duration_ms, &outcome));

    match outcome {
        Ok(mut value) => {
            let soft_error = value.get("error").is_some();
            if let Some(object) = value.as_object_mut() {
                object.insert("duration_ms".to_string(), json!(duration_ms));
            }
            (value, soft_error)
        }
        Err(e) => (
            json!({
                "error": e.code(),
                "message": e.to_string(),
                "duration_ms": duration_ms,
                "recovery": recovery_hint(e.code()),
            }),
            true,
        ),
    }
}

/// What the agent should do next after a given failure. Agents follow concrete
/// instructions far more reliably than they infer them from an error string.
fn recovery_hint(code: &str) -> &'static str {
    match code {
        "project_not_found" => {
            "Call list_projects for the exact ids, or index_repository if the repository is not indexed yet."
        }
        "index_missing" => "Call index_repository with the project's root path to build the graph.",
        "ambiguous_symbol" => "Retry with the full qualified_name from search_graph.",
        "symbol_not_found" => {
            "Widen the search with search_graph name_pattern, then check_index_coverage before \
             concluding the symbol does not exist."
        }
        "path_out_of_root" => "Use a path relative to the project root; loci reads nothing outside it.",
        "language_unsupported" => "Call get_graph_schema for the list of bundled languages, then use search_code.",
        "lsp_unavailable" => "This build is AST-only; the graph is still usable.",
        "bad_cursor" => "Re-run the query without a cursor and page from the start.",
        "invalid_argument" => "Re-read the tool's inputSchema and correct the arguments.",
        _ => "Retry once; if it persists, fall back to reading the source directly.",
    }
}

/// Handle one JSON-RPC request, returning the response value (or `Value::Null`
/// for notifications, which must not be answered).
pub fn handle_request(request: &Request) -> Value {
    let id = request.id.clone().unwrap_or(Value::Null);

    match request.method.as_str() {
        "initialize" => {
            let version = request
                .params
                .get("protocolVersion")
                .and_then(Value::as_str);
            protocol::success(id, protocol::initialize_result(version))
        }
        // Notifications carry no id and expect no reply.
        method if method.starts_with("notifications/") => Value::Null,
        "ping" => protocol::success(id, json!({})),
        "tools/list" => protocol::success(id, tools::tool_list()),
        "resources/list" => protocol::success(id, json!({ "resources": [] })),
        "prompts/list" => protocol::success(id, json!({ "prompts": [] })),
        "tools/call" => {
            let name = request
                .params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();

            if tools::find(name).is_none() {
                return protocol::failure(
                    id,
                    ErrorObject {
                        code: protocol::INVALID_PARAMS,
                        message: format!("unknown tool '{name}'"),
                        data: Some(json!({
                            "available": tools::TOOLS.iter().map(|t| t.name).collect::<Vec<_>>()
                        })),
                    },
                );
            }

            let args = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (payload, is_error) = call_tool(name, &args);
            protocol::success(id, protocol::tool_result(&payload, is_error))
        }
        other => protocol::failure(
            id,
            ErrorObject {
                code: protocol::METHOD_NOT_FOUND,
                message: format!("method '{other}' is not implemented"),
                data: None,
            },
        ),
    }
}

/// Run the stdio server until the client closes stdin.
pub fn serve_stdio<R: BufRead, W: Write>(input: R, output: W) -> std::io::Result<()> {
    protocol::serve(input, output, handle_request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Request {
        serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .unwrap()
    }

    #[test]
    fn initialize_negotiates_and_returns_instructions() {
        let response = handle_request(&request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "clientInfo": { "name": "cursor", "version": "1.0" },
                "capabilities": {}
            }),
        ));
        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(response["result"]["serverInfo"]["name"], "loci");
        assert!(response["result"]["instructions"].is_string());
    }

    #[test]
    fn tools_list_exposes_all_fifteen_with_schemas() {
        let response = handle_request(&request("tools/list", json!({})));
        let listed = response["result"]["tools"].as_array().unwrap();
        assert_eq!(listed.len(), 15);
        for tool in listed {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn unknown_tools_are_rejected_with_the_available_list() {
        let response = handle_request(&request(
            "tools/call",
            json!({ "name": "definitely_not_a_tool", "arguments": {} }),
        ));
        assert_eq!(response["error"]["code"], protocol::INVALID_PARAMS);
        assert!(response["error"]["data"]["available"].is_array());
    }

    #[test]
    fn unknown_methods_return_method_not_found() {
        let response = handle_request(&request("does/not/exist", json!({})));
        assert_eq!(response["error"]["code"], protocol::METHOD_NOT_FOUND);
    }

    #[test]
    fn tool_failures_come_back_as_readable_results_with_recovery_advice() {
        let response = handle_request(&request(
            "tools/call",
            json!({
                "name": "index_status",
                "arguments": { "project": "no-such-project-xyz" }
            }),
        ));

        assert_eq!(response["result"]["isError"], true);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["error"], "project_not_found");
        assert!(payload["recovery"]
            .as_str()
            .unwrap()
            .contains("list_projects"));
    }

    #[test]
    fn get_graph_schema_reports_only_grammars_that_are_linked_in() {
        let response = handle_request(&request(
            "tools/call",
            json!({ "name": "get_graph_schema", "arguments": {} }),
        ));
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();

        let languages: Vec<&str> = payload["bundled_languages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(languages.contains(&"python"));
        assert!(languages.contains(&"go"));
        for late_addition in ["csharp", "kotlin", "perl"] {
            assert!(
                languages.contains(&late_addition),
                "{late_addition} has a grammar and must be listed"
            );
        }
        assert!(!languages.contains(&"php"));

        // Hybrid LSP exists but must never be described as always-on, or an
        // agent will read source="ast" edges as type-checked.
        assert_eq!(payload["hybrid_lsp"]["implemented"], true);
        assert_eq!(payload["hybrid_lsp"]["default"], "off");
    }

    #[test]
    fn notifications_produce_no_response_value() {
        let notification: Request = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap();
        assert!(handle_request(&notification).is_null());
    }
}
