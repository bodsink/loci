//! A minimal synchronous LSP client over stdio.
//!
//! Scope is deliberately narrow: start a server, open a file, ask
//! `textDocument/definition`, shut down. That is the single question the
//! indexer cannot answer from the AST, and answering only it keeps the client
//! small enough to reason about.
//!
//! Two framing details differ from the MCP server in `loci-mcp`:
//! LSP uses `Content-Length` headers rather than one JSON object per line, and
//! the server may send requests *to* us mid-conversation, which must be
//! answered or servers such as gopls stall waiting for a reply.

use loci_core::{LociError, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// A resolved definition site, in the terms the indexer needs: a
/// repository-relative path and a zero-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub relative_path: String,
    pub line: u32,
}

pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    incoming: Receiver<Value>,
    root: PathBuf,
    next_id: i64,
    timeout: Duration,
    /// Files already sent with didOpen, so each is opened at most once.
    opened: std::collections::HashSet<String>,
}

fn lsp_error(message: impl Into<String>) -> LociError {
    LociError::LspUnavailable(message.into())
}

impl LspClient {
    /// Spawn `executable` rooted at `root` and complete the LSP handshake.
    ///
    /// Returns `LspUnavailable` rather than panicking when the server is
    /// missing or refuses to initialise, so the caller can fall back to AST.
    pub fn start(executable: &str, args: &[&str], root: &Path, timeout: Duration) -> Result<Self> {
        let mut child = Command::new(executable)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| lsp_error(format!("{executable} failed to start: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| lsp_error("server stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| lsp_error("server stdout unavailable"))?;

        // stderr must be drained or a chatty server fills the pipe and blocks.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = BufReader::new(stderr).read_to_end(&mut sink);
            });
        }

        let (sender, incoming) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = read_message(&mut reader) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });

        let mut client = Self {
            child,
            stdin,
            incoming,
            root: root.to_path_buf(),
            next_id: 0,
            timeout,
            opened: std::collections::HashSet::new(),
        };

        client.initialize()?;
        Ok(client)
    }

    fn initialize(&mut self) -> Result<()> {
        let root_uri = path_to_uri(&self.root);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "workspaceFolders": [{ "uri": root_uri, "name": "root" }],
            "capabilities": {
                "textDocument": {
                    "definition": { "linkSupport": true },
                },
                "workspace": { "workspaceFolders": true },
            },
        });

        self.request("initialize", params)?;
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    /// Tell the server about a file. Sending the text we already read avoids a
    /// second disk read and keeps the server's view identical to the indexer's.
    pub fn did_open(&mut self, relative_path: &str, language_id: &str, text: &str) -> Result<()> {
        if !self.opened.insert(relative_path.to_string()) {
            return Ok(());
        }
        let uri = path_to_uri(&self.root.join(relative_path));
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                },
            }),
        )
    }

    /// Ask where the symbol at this position is defined.
    ///
    /// `line` and `character` are zero-based, as LSP requires. An empty result
    /// means the server had no answer, which is reported as such rather than
    /// treated as an error.
    pub fn definition(
        &mut self,
        relative_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        let uri = path_to_uri(&self.root.join(relative_path));
        let response = self.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )?;

        Ok(self.parse_locations(&response))
    }

    fn parse_locations(&self, result: &Value) -> Vec<Location> {
        // The spec allows Location, Location[], or LocationLink[].
        let items: Vec<&Value> = match result {
            Value::Array(items) => items.iter().collect(),
            Value::Object(_) => vec![result],
            _ => return Vec::new(),
        };

        items
            .into_iter()
            .filter_map(|item| {
                let uri = item
                    .get("uri")
                    .or_else(|| item.get("targetUri"))
                    .and_then(Value::as_str)?;
                let range = item
                    .get("range")
                    .or_else(|| item.get("targetSelectionRange"))
                    .or_else(|| item.get("targetRange"))?;
                let line = range.get("start")?.get("line")?.as_u64()? as u32;
                let path = uri_to_path(uri)?;
                let relative = path.strip_prefix(&self.root).ok()?;
                Some(Location {
                    relative_path: relative.to_string_lossy().replace('\\', "/"),
                    line,
                })
            })
            .collect()
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        self.wait_for(id, method)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let body = serde_json::to_vec(message)?;
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .and_then(|_| self.stdin.write_all(&body))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| lsp_error(format!("writing to language server failed: {e}")))
    }

    /// Read until the response with `id` arrives, servicing anything the server
    /// asks of us on the way. The deadline is absolute so a stream of unrelated
    /// notifications cannot extend the wait indefinitely.
    fn wait_for(&mut self, id: i64, method: &str) -> Result<Value> {
        let deadline = Instant::now() + self.timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(lsp_error(format!("{method} timed out")));
            }

            let message = match self.incoming.recv_timeout(remaining) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(lsp_error(format!("{method} timed out")))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(lsp_error(format!("language server exited during {method}")))
                }
            };

            let message_id = message.get("id").and_then(Value::as_i64);
            let is_request_from_server = message.get("method").is_some();

            match (message_id, is_request_from_server) {
                // Our response.
                (Some(received), false) if received == id => {
                    if let Some(error) = message.get("error") {
                        return Err(lsp_error(format!("{method} failed: {error}")));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                // A request from the server. Ignoring it deadlocks gopls.
                (Some(received), true) => {
                    let reply = self.reply_to_server_request(&message);
                    self.send(&json!({
                        "jsonrpc": "2.0",
                        "id": received,
                        "result": reply,
                    }))?;
                }
                // A notification, or a response we are not waiting on.
                _ => continue,
            }
        }
    }

    /// Minimal answers to the requests servers make during startup. Anything we
    /// do not model gets `null`, which every server treats as "not supported".
    fn reply_to_server_request(&self, message: &Value) -> Value {
        match message.get("method").and_then(Value::as_str) {
            // Must be an array the same length as the requested items.
            Some("workspace/configuration") => {
                let count = message
                    .get("params")
                    .and_then(|p| p.get("items"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                Value::Array(vec![Value::Null; count])
            }
            _ => Value::Null,
        }
    }

    /// Ask the server to exit, then make sure it actually did.
    pub fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);

        // A server that ignores `exit` must not outlive the index run.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
    }
}

/// Read one `Content-Length` framed message. `None` means the stream ended.
fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length: Option<usize> = None;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }

    let mut body = vec![0u8; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn path_to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_through_percent_encoding() {
        for original in [
            "/home/user/project/src/main.rs",
            "/tmp/dir with spaces/a.go",
            "/tmp/tanda+plus/ünïcode.py",
        ] {
            let uri = path_to_uri(Path::new(original));
            assert!(uri.starts_with("file:///"), "{uri}");
            assert!(!uri.contains(' '), "spaces must be encoded: {uri}");
            assert_eq!(uri_to_path(&uri).as_deref(), Some(Path::new(original)));
        }
    }

    #[test]
    fn a_message_is_read_from_its_content_length_frame() {
        let raw = "Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}";
        let mut reader = BufReader::new(raw.as_bytes());
        let message = read_message(&mut reader).expect("a framed message");
        assert_eq!(message["jsonrpc"], "2.0");
    }

    #[test]
    fn a_truncated_frame_ends_the_stream_instead_of_hanging() {
        let raw = "Content-Length: 999\r\n\r\n{\"jsonrpc\":\"2.0\"}";
        let mut reader = BufReader::new(raw.as_bytes());
        assert!(read_message(&mut reader).is_none());
    }

    #[test]
    fn a_missing_server_is_reported_as_unavailable_not_a_panic() {
        let result = LspClient::start(
            "loci-definitely-not-a-language-server",
            &[],
            Path::new("/tmp"),
            Duration::from_secs(1),
        );
        let error = match result {
            Ok(_) => panic!("a nonexistent executable must not start"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "lsp_unavailable");
    }
}
