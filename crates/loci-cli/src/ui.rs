//! Local web UI for inspecting indexed projects.
//!
//! The page is served from this process and talks to the same handlers the
//! MCP tools use. It binds loopback by default: the graph never leaves the
//! machine.

mod ui_graph;

use loci_core::{LociError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");

pub struct ServeOptions {
    pub bind: String,
    pub port: u16,
    pub open_browser: bool,
}

pub fn run(options: &ServeOptions) -> Result<()> {
    let addr = format!("{}:{}", options.bind, options.port);
    let server = bind(&addr)?;
    let bound = display_addr(&server);
    eprintln!("loci ui listening on {bound}");
    eprintln!("The graph stays on this machine. Press Ctrl-C to stop.");
    if options.open_browser {
        let _ = open_url(&format!("{bound}/"));
    }
    serve(server)
}

pub fn bind(addr: &str) -> Result<Server> {
    Server::http(addr).map_err(|e| LociError::InvalidArgument(format!("cannot bind {addr}: {e}")))
}

pub fn serve(server: Server) -> Result<()> {
    for request in server.incoming_requests() {
        std::thread::spawn(move || {
            if let Err(error) = respond(request) {
                eprintln!("ui request failed: {error}");
            }
        });
    }
    Ok(())
}

pub fn display_addr(server: &Server) -> String {
    match server.server_addr().to_ip() {
        Some(addr) => format!("http://{addr}"),
        None => "http://127.0.0.1".to_string(),
    }
}

#[cfg(test)]
pub fn bound_socket(server: &Server) -> Option<std::net::SocketAddr> {
    server.server_addr().to_ip()
}

fn respond(mut request: Request) -> Result<()> {
    let method = request.method().clone();
    let url = request.url().to_string();
    let mut body = Vec::new();
    std::io::Read::read_to_end(request.as_reader(), &mut body)
        .map_err(|e| LociError::io("<request>", e))?;

    let (path, query) = split_url(&url);
    let out = dispatch(method_str(&method), path, query, &body);
    let response = Response::from_data(out.body)
        .with_status_code(StatusCode(out.status))
        .with_header(header("Content-Type", out.content_type))
        .with_header(header("Cache-Control", "no-store"));
    request
        .respond(response)
        .map_err(|e| LociError::io("<response>", e))?;
    Ok(())
}

fn method_str(method: &Method) -> &'static str {
    match method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Delete => "DELETE",
        Method::Put => "PUT",
        Method::Head => "HEAD",
        Method::Options => "OPTIONS",
        Method::Patch => "PATCH",
        _ => "GET",
    }
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header")
}

pub struct HttpOut {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

pub fn dispatch(method: &str, path: &str, query: &str, body: &[u8]) -> HttpOut {
    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => text(200, "text/html; charset=utf-8", INDEX_HTML),
        ("GET", "/app.css") => text(200, "text/css; charset=utf-8", APP_CSS),
        ("GET", "/app.js") => text(200, "text/javascript; charset=utf-8", APP_JS),
        ("GET", "/api/health") => json_ok(json!({
            "ok": true,
            "server": "loci-ui",
            "version": env!("CARGO_PKG_VERSION"),
        })),
        ("GET", "/api/tools") => json_ok(loci_mcp::tools::tool_list()),
        ("GET", "/api/usage") => match loci_mcp::journal::summarise() {
            Ok(summary) => json_ok(serde_json::to_value(summary).unwrap_or(json!({}))),
            Err(error) => json_err(&error),
        },
        ("GET", path) if project_graph_id(path).is_some() => {
            let project = project_graph_id(path).expect("checked");
            let params = query_map(query);
            let view = params.get("view").map(String::as_str).unwrap_or("overview");
            let seed = params.get("seed").map(String::as_str);
            let depth = params
                .get("depth")
                .and_then(|s| s.parse().ok())
                .unwrap_or(2);
            let limit = params
                .get("limit")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            match ui_graph::visualization_graph(project, view, seed, depth, limit) {
                Ok(value) => json_ok(value),
                Err(error) => json_err(&error),
            }
        }
        ("POST", path) if path.starts_with("/api/tools/") => {
            let name = &path["/api/tools/".len()..];
            if name.is_empty() || loci_mcp::tools::find(name).is_none() {
                return json_status(
                    404,
                    json!({
                        "error": "unknown_tool",
                        "message": format!("unknown tool '{name}'"),
                        "available": loci_mcp::tools::TOOLS.iter().map(|t| t.name).collect::<Vec<_>>(),
                    }),
                );
            }
            let args: Value = if body.is_empty() {
                json!({})
            } else {
                match serde_json::from_slice(body) {
                    Ok(value) => value,
                    Err(error) => {
                        return json_status(
                            400,
                            json!({
                                "error": "invalid_argument",
                                "message": format!("request body is not JSON: {error}"),
                            }),
                        )
                    }
                }
            };
            match loci_mcp::handlers::dispatch(name, &args) {
                Ok(value) => json_ok(value),
                Err(error) => json_err(&error),
            }
        }
        _ => json_status(
            404,
            json!({
                "error": "not_found",
                "message": format!("no route for {method} {path}"),
            }),
        ),
    }
}

fn project_graph_id(path: &str) -> Option<&str> {
    path.strip_prefix("/api/projects/")?
        .strip_suffix("/graph")
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

fn split_url(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((path, query)) => (path, query),
        None => (url, ""),
    }
}

fn query_map(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(key), percent_decode(value));
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Some(hex) = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
            {
                out.push(hex);
                index += 3;
                continue;
            }
        } else if bytes[index] == b'+' {
            out.push(b' ');
            index += 1;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn text(status: u16, content_type: &'static str, body: &str) -> HttpOut {
    HttpOut {
        status,
        content_type,
        body: body.as_bytes().to_vec(),
    }
}

fn json_ok(value: Value) -> HttpOut {
    json_status(200, value)
}

fn json_err(error: &LociError) -> HttpOut {
    let status = match error.code() {
        "project_not_found" | "index_missing" | "symbol_not_found" => 404,
        "invalid_argument" | "bad_cursor" | "ambiguous_symbol" => 400,
        _ => 500,
    };
    json_status(
        status,
        json!({
            "error": error.code(),
            "message": error.to_string(),
        }),
    )
}

fn json_status(status: u16, value: Value) -> HttpOut {
    HttpOut {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

fn open_url(url: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn data_dir() -> &'static Path {
        static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
        let dir = DIR.get_or_init(|| {
            let dir = tempfile::tempdir().expect("temp data dir");
            std::env::set_var("LOCI_DATA_DIR", dir.path());
            dir
        });
        dir.path()
    }

    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        data_dir();
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("create dir");
        for entry in std::fs::read_dir(from).expect("read fixture") {
            let entry = entry.expect("dir entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("file type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy file");
            }
        }
    }

    fn fixture_copy() -> tempfile::TempDir {
        let destination = tempfile::tempdir().expect("temp fixture");
        copy_tree(&repo_root().join("fixtures/sample"), destination.path());
        destination
    }

    fn json_body(out: &HttpOut) -> Value {
        serde_json::from_slice(&out.body).expect("json body")
    }

    fn indexed(name: &str) -> (String, tempfile::TempDir) {
        let fixture = fixture_copy();
        let out = dispatch(
            "POST",
            "/api/tools/index_repository",
            "",
            serde_json::to_vec(&json!({
                "repo_path": fixture.path().to_string_lossy(),
                "name": name,
            }))
            .unwrap()
            .as_slice(),
        );
        let report = json_body(&out);
        assert_eq!(out.status, 200, "{report}");
        assert!(report["error"].is_null(), "{report}");
        (
            report["project"].as_str().expect("project").to_string(),
            fixture,
        )
    }

    #[test]
    fn the_page_and_assets_are_served() {
        let _lock = serial();
        let page = dispatch("GET", "/", "", b"");
        assert_eq!(page.status, 200);
        let html = String::from_utf8_lossy(&page.body);
        assert!(html.contains("Loci"));
        assert!(html.contains("Add project"));

        let css = dispatch("GET", "/app.css", "", b"");
        assert!(String::from_utf8_lossy(&css.body).contains("--teal"));

        let js = dispatch("GET", "/app.js", "", b"");
        let js = String::from_utf8_lossy(&js.body);
        assert!(js.contains("index_repository"));
        assert!(
            js.contains("data-delete") && js.contains("removeProject"),
            "the project list must expose a remove control for an added project"
        );
        assert!(html.contains("Remove project"));
    }

    #[test]
    fn tools_list_exposes_the_same_fifteen() {
        let _lock = serial();
        let out = dispatch("GET", "/api/tools", "", b"");
        let body = json_body(&out);
        let tools = body["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 15);
        assert!(tools.iter().any(|t| t["name"] == "search_graph"));
        assert!(tools.iter().any(|t| t["name"] == "index_repository"));
    }

    #[test]
    fn add_project_indexes_and_list_projects_sees_it() {
        let _lock = serial();
        let (project, _fixture) = indexed("ui-added");

        let listed = dispatch("POST", "/api/tools/list_projects", "", b"{}");
        let body = json_body(&listed);
        let ids: Vec<&str> = body["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["project"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&project.as_str()), "{body}");
    }

    #[test]
    fn removing_an_added_project_drops_it_from_the_list_and_leaves_source() {
        let _lock = serial();
        let (project, fixture) = indexed("ui-removed");
        let source = fixture.path().to_path_buf();
        assert!(
            source.exists(),
            "fixture must exist before delete so we can prove it survives"
        );

        let deleted = dispatch(
            "POST",
            "/api/tools/delete_project",
            "",
            serde_json::to_vec(&json!({ "project": project }))
                .unwrap()
                .as_slice(),
        );
        let body = json_body(&deleted);
        assert_eq!(deleted.status, 200, "{body}");
        assert_eq!(body["deleted"], project.as_str(), "{body}");

        let listed = dispatch("POST", "/api/tools/list_projects", "", b"{}");
        let listed = json_body(&listed);
        let ids: Vec<&str> = listed["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["project"].as_str().unwrap())
            .collect();
        assert!(
            !ids.contains(&project.as_str()),
            "deleted project must leave the catalog: {listed}"
        );
        assert!(
            source.exists(),
            "delete_project must not touch the source repository"
        );
    }

    #[test]
    fn atlas_overview_shows_files_and_structural_edges() {
        let _lock = serial();
        let (project, _fixture) = indexed("ui-overview");
        let out = dispatch(
            "GET",
            &format!("/api/projects/{project}/graph"),
            "view=overview",
            b"",
        );
        let body = json_body(&out);
        assert_eq!(out.status, 200, "{body}");
        let nodes = body["nodes"].as_array().expect("nodes");
        assert!(
            nodes.iter().any(|n| n["label"] == "File"),
            "overview must include File nodes: {body}"
        );
        let edges = body["edges"].as_array().expect("edges");
        assert!(
            edges.iter().any(|e| matches!(
                e["edge_type"].as_str(),
                Some("IMPORTS" | "CONTAINS" | "ROUTES_TO" | "DEFINES" | "CALLS")
            )),
            "overview must include structural edges: {body}"
        );
    }

    #[test]
    fn atlas_routes_view_contains_route_nodes_and_edges() {
        let _lock = serial();
        let (project, _fixture) = indexed("ui-routes");
        let out = dispatch(
            "GET",
            &format!("/api/projects/{project}/graph"),
            "view=routes",
            b"",
        );
        let body = json_body(&out);
        assert_eq!(out.status, 200, "{body}");
        let nodes = body["nodes"].as_array().expect("nodes");
        assert!(
            nodes.iter().any(|n| n["label"] == "Route"),
            "expected Route nodes: {body}"
        );
        let edges = body["edges"].as_array().expect("edges");
        assert!(
            edges.iter().any(|e| e["edge_type"] == "ROUTES_TO"),
            "expected ROUTES_TO edges: {body}"
        );
    }

    #[test]
    fn atlas_seed_walks_the_create_order_neighbourhood() {
        let _lock = serial();
        let (project, _fixture) = indexed("ui-seed");
        let out = dispatch(
            "GET",
            &format!("/api/projects/{project}/graph"),
            "view=neighborhood&seed=services.orders-py.app.main.create_order&depth=2",
            b"",
        );
        let body = json_body(&out);
        assert_eq!(out.status, 200, "{body}");
        assert!(body["error"].is_null(), "{body}");
        let names: Vec<&str> = body["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["qualified_name"].as_str().unwrap())
            .collect();
        assert!(
            names.iter().any(|n| n.ends_with("create_order")),
            "{names:?}"
        );
        assert!(
            !body["edges"].as_array().unwrap().is_empty(),
            "seeded walk must return edges"
        );
    }

    #[test]
    fn overlapping_reads_do_not_fight_for_the_graph_lock() {
        let _lock = serial();
        let (project, _fixture) = indexed("ui-lock");
        let body = serde_json::to_vec(&json!({ "project": project })).unwrap();

        let first = std::thread::spawn({
            let body = body.clone();
            move || dispatch("POST", "/api/tools/index_status", "", &body)
        });
        let second = dispatch("POST", "/api/tools/index_status", "", &body);
        let first = first.join().expect("thread");

        let a = json_body(&first);
        let b = json_body(&second);
        assert_eq!(first.status, 200, "{a}");
        assert_eq!(second.status, 200, "{b}");
        assert!(a["error"].is_null(), "{a}");
        assert!(b["error"].is_null(), "{b}");
        assert_eq!(a["nodes"], b["nodes"]);
    }

    #[test]
    fn unknown_project_is_an_explicit_error() {
        let _lock = serial();
        let out = dispatch("GET", "/api/projects/no-such-ui/graph", "", b"");
        let body = json_body(&out);
        assert_eq!(out.status, 404);
        assert_eq!(body["error"], "project_not_found");
    }

    #[test]
    fn the_http_server_serves_the_page_on_a_real_socket() {
        let _lock = serial();
        let server = bind("127.0.0.1:0").expect("bind");
        let addr = bound_socket(&server).expect("tcp addr");
        std::thread::spawn(move || {
            let _ = serve(server);
        });

        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .expect("write");
        let mut buf = String::new();
        stream.read_to_string(&mut buf).expect("read");
        assert!(buf.contains("Loci"), "{buf}");
        assert!(buf.contains("200 OK"), "{buf}");
    }
}
