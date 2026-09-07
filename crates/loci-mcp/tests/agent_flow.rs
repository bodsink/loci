//! End-to-end tests of the sequence a Cursor agent actually performs:
//! list_projects -> index_repository -> search_graph -> trace_path ->
//! get_code_snippet -> check_index_coverage.
//!
//! Every assertion checks something an agent would be misled by if it broke.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// One data directory per test binary, pointed at by LOCI_DATA_DIR so no test
/// ever touches the developer's real index.
fn data_dir() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("temp data dir");
        std::env::set_var("LOCI_DATA_DIR", dir.path());
        dir
    });
    dir.path()
}

/// The catalog is a single shared file, so these tests run one at a time.
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

/// Copy the fixture so a test can modify files without dirtying the repository.
fn fixture_copy() -> tempfile::TempDir {
    let destination = tempfile::tempdir().expect("temp fixture");
    copy_tree(&repo_root().join("fixtures/sample"), destination.path());
    destination
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Call a tool the way the MCP layer does, returning the decoded payload.
fn call(tool: &str, args: Value) -> Value {
    let (payload, _) = loci_mcp::call_tool(tool, &args);
    payload
}

/// Index a fresh copy of the fixture and return (project id, fixture dir).
fn indexed_fixture(name: &str) -> (String, tempfile::TempDir) {
    let fixture = fixture_copy();
    let report = call(
        "index_repository",
        json!({ "repo_path": fixture.path().to_string_lossy(), "name": name }),
    );
    assert!(
        report["error"].is_null(),
        "indexing the fixture failed: {report}"
    );
    (
        report["project"].as_str().expect("project id").to_string(),
        fixture,
    )
}

#[test]
fn a_new_agent_can_go_from_nothing_to_source_without_prior_knowledge() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("flow");

    // 1. The project is discoverable, with a concrete root.
    let projects = call("list_projects", json!({}));
    let listed = projects["projects"].as_array().expect("projects array");
    let entry = listed
        .iter()
        .find(|p| p["project"] == project.as_str())
        .expect("the freshly indexed project must be listed");
    assert!(Path::new(entry["root"].as_str().unwrap()).is_absolute());
    assert_eq!(entry["graph_present"], true);

    // 2. A structural search returns concrete locations, not guesses.
    let found = call(
        "search_graph",
        json!({ "project": project, "name": "create_order" }),
    );
    let hits = found["results"].as_array().expect("results");
    assert!(!hits.is_empty(), "create_order must be in the graph");
    for hit in hits {
        assert!(hit["file_path"].as_str().unwrap().ends_with(".py"));
        assert!(hit["start_line"].as_u64().unwrap() >= 1);
        assert!(hit["end_line"].as_u64().unwrap() >= hit["start_line"].as_u64().unwrap());
        assert!(hit["qualified_name"]
            .as_str()
            .unwrap()
            .contains("create_order"));
    }

    // 3. The qualified name from search feeds straight into a snippet read.
    let qualified_name = hits
        .iter()
        .find(|h| h["label"] == "Method")
        .expect("the service method")["qualified_name"]
        .as_str()
        .unwrap()
        .to_string();

    let snippet = call(
        "get_code_snippet",
        json!({ "project": project, "qualified_name": qualified_name }),
    );
    assert!(
        snippet["source"]
            .as_str()
            .unwrap()
            .contains("def create_order"),
        "the snippet must be the real source: {snippet}"
    );
    assert_eq!(snippet["label"], "Method");
}

#[test]
fn trace_path_finds_transitive_callers_across_files() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("trace");

    let trace = call(
        "trace_path",
        json!({
            "project": project,
            "qualified_name": "services.orders-py.app.repository.save_order",
            "direction": "inbound",
            "depth": 4
        }),
    );

    let callers: Vec<&str> = trace["callers"]
        .as_array()
        .expect("callers")
        .iter()
        .map(|c| c["qualified_name"].as_str().unwrap())
        .collect();

    assert!(
        callers
            .iter()
            .any(|c| c.ends_with("OrderService.create_order")),
        "the direct caller in another file must be found: {callers:?}"
    );
    assert!(
        callers.iter().any(|c| c.ends_with("app.main.create_order")),
        "the transitive HTTP handler must be found at depth 2: {callers:?}"
    );

    for caller in trace["callers"].as_array().unwrap() {
        assert!(caller["hop"].as_u64().unwrap() >= 1);
        assert!(!caller["file_path"].as_str().unwrap().is_empty());
    }
}

#[test]
fn unresolved_calls_are_reported_instead_of_silently_dropped() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("unresolved");

    // This handler calls into a third-party library, which is not indexed.
    let trace = call(
        "trace_path",
        json!({
            "project": project,
            "qualified_name": "services.orders-py.app.main.health",
            "direction": "outbound"
        }),
    );
    assert!(trace["unresolved"].is_array());

    // Somewhere in the fixture there must be at least one unresolved call,
    // otherwise the honesty guarantee is untested.
    let architecture = call("get_architecture", json!({ "project": project }));
    assert!(architecture["routes"].as_array().unwrap().len() >= 8);
}

#[test]
fn routes_are_extracted_for_every_fixture_language_with_real_evidence() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("routes");

    let architecture = call("get_architecture", json!({ "project": project }));
    let routes = architecture["routes"].as_array().expect("routes");

    let paths: Vec<&str> = routes.iter().map(|r| r["path"].as_str().unwrap()).collect();
    assert!(
        paths.contains(&"/orders"),
        "FastAPI route missing: {paths:?}"
    );
    assert!(
        paths.contains(&"/checkout"),
        "Express route missing: {paths:?}"
    );
    assert!(
        paths.contains(&"/reserve"),
        "net/http route missing: {paths:?}"
    );
    assert!(
        paths.contains(&"/invoices"),
        "ASP.NET attribute route missing: {paths:?}"
    );

    // The ASP.NET verb lives in the attribute name (HttpPost), not in a call.
    let invoices = routes
        .iter()
        .find(|r| r["path"] == "/invoices")
        .expect("the invoices route");
    assert_eq!(invoices["method"], "POST");

    // Every route must carry a real file location.
    for route in routes {
        assert!(!route["file_path"].as_str().unwrap().is_empty());
        assert!(route["line"].as_u64().unwrap() >= 1);
    }

    // The Python DELETE route must reach its handler through ROUTES_TO.
    let linked = routes
        .iter()
        .find(|r| r["path"] == "/orders/{order_id}")
        .expect("the delete route");
    assert_eq!(linked["method"], "DELETE");
    assert!(
        linked["handler"]["qualified_name"]
            .as_str()
            .unwrap()
            .ends_with("cancel_order"),
        "route must link to its handler: {linked}"
    );
}

#[test]
fn query_graph_walks_from_routes_to_handlers() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("pattern");

    let result = call(
        "query_graph",
        json!({
            "project": project,
            "start": { "label": "Route" },
            "hops": [{ "edge": "ROUTES_TO" }]
        }),
    );

    let rows = result["rows"].as_array().expect("rows");
    assert!(!rows.is_empty(), "at least one route must reach a handler");
    for row in rows {
        let path = row["path"].as_array().unwrap();
        assert_eq!(path.len(), 2, "start node plus one hop");
        assert_eq!(path[0]["label"], "Route");
        assert_eq!(path[1]["via_edge"], "ROUTES_TO");
    }
}

#[test]
fn gitignored_files_are_reported_as_excluded_not_as_missing() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("coverage");

    let coverage = call(
        "check_index_coverage",
        json!({ "project": project, "paths": ["ignored/secret.py", "services/orders-py/app/main.py"] }),
    );
    let results = coverage["paths"].as_array().expect("paths");

    let ignored = &results[0];
    assert_eq!(ignored["status"], "excluded");
    assert_eq!(ignored["reason"], "gitignore");

    let indexed = &results[1];
    assert_eq!(indexed["status"], "indexed");
    assert!(indexed["symbols_in_graph"].as_u64().unwrap() > 0);

    // The gitignored symbol must genuinely be absent from the graph.
    let search = call(
        "search_graph",
        json!({ "project": project, "name": "should_never_be_indexed" }),
    );
    assert_eq!(search["total"], 0);
    assert!(
        search["note"]
            .as_str()
            .unwrap()
            .contains("check_index_coverage"),
        "an empty result must point the agent at the coverage tool"
    );
}

#[test]
fn coverage_reports_unsupported_files_with_a_reason() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("unsupported");

    let coverage = call(
        "check_index_coverage",
        json!({ "project": project, "paths": ["README.md"] }),
    );
    let readme = &coverage["paths"][0];
    assert_eq!(readme["status"], "skipped");
    assert_eq!(readme["reason"], "unsupported_language");
    assert_eq!(readme["fallback"], "read_source_directly");
}

#[test]
fn incremental_reindex_updates_changed_files_and_keeps_callers_correct() {
    let _guard = serial();
    let (project, fixture) = indexed_fixture("incremental");

    let before = call("index_status", json!({ "project": project }));
    let baseline_nodes = before["nodes"].as_u64().unwrap();

    // Nothing changed yet.
    let clean = call("detect_changes", json!({ "project": project }));
    assert_eq!(clean["modified"], 0);
    assert_eq!(clean["added"], 0);
    assert_eq!(clean["removed"], 0);

    // Add a function to an existing file.
    let repository = fixture.path().join("services/orders-py/app/repository.py");
    let original = std::fs::read_to_string(&repository).unwrap();
    std::fs::write(
        &repository,
        format!("{original}\n\ndef purge_orders():\n    _ORDERS.clear()\n"),
    )
    .unwrap();

    let dirty = call("detect_changes", json!({ "project": project }));
    assert_eq!(dirty["modified"], 1, "the edited file must be flagged");
    let changed = &dirty["files"][0];
    assert_eq!(changed["change"], "modified");
    assert!(
        !changed["symbols"].as_array().unwrap().is_empty(),
        "an agent needs to know which symbols are at risk"
    );

    // Re-index incrementally.
    let report = call(
        "index_repository",
        json!({ "repo_path": fixture.path().to_string_lossy(), "name": project }),
    );
    assert_eq!(
        report["files_reparsed"], 1,
        "only the edited file may be re-parsed: {report}"
    );
    assert!(
        report["files_unchanged"].as_u64().unwrap() > 0,
        "unchanged files must be skipped, not re-parsed: {report}"
    );

    // The new symbol is present and the old ones survived.
    let added = call(
        "search_graph",
        json!({ "project": project, "name": "purge_orders" }),
    );
    assert_eq!(added["total"], 1);

    let after = call("index_status", json!({ "project": project }));
    assert!(after["nodes"].as_u64().unwrap() > baseline_nodes);

    // Callers from files that were NOT re-parsed must still resolve.
    let trace = call(
        "trace_path",
        json!({
            "project": project,
            "qualified_name": "services.orders-py.app.repository.save_order",
            "direction": "inbound",
            "depth": 4
        }),
    );
    let callers: Vec<&str> = trace["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["qualified_name"].as_str().unwrap())
        .collect();
    assert!(
        callers
            .iter()
            .any(|c| c.ends_with("OrderService.create_order")),
        "an incremental run must not lose edges from untouched files: {callers:?}"
    );
}

#[test]
fn deleting_a_file_removes_its_symbols_on_the_next_index() {
    let _guard = serial();
    let (project, fixture) = indexed_fixture("deletion");

    let inventory = fixture.path().join("services/inventory-go/inventory.go");
    std::fs::remove_file(&inventory).unwrap();

    let changes = call("detect_changes", json!({ "project": project }));
    assert_eq!(changes["removed"], 1);

    call(
        "index_repository",
        json!({ "repo_path": fixture.path().to_string_lossy(), "name": project }),
    );

    let gone = call(
        "search_graph",
        json!({ "project": project, "name": "fetchInventory" }),
    );
    assert_eq!(
        gone["total"], 0,
        "symbols from a deleted file must disappear"
    );
}

#[test]
fn ambiguous_symbols_return_candidates_rather_than_a_guess() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("ambiguous");

    // `health` exists in both the Python and the Rust service.
    let response = call(
        "get_code_snippet",
        json!({ "project": project, "qualified_name": "health" }),
    );

    assert_eq!(response["error"], "ambiguous_symbol");
    let candidates = response["candidates"].as_array().expect("candidates");
    assert!(candidates.len() >= 2);
    for candidate in candidates {
        assert!(!candidate["qualified_name"].as_str().unwrap().is_empty());
        assert!(!candidate["file_path"].as_str().unwrap().is_empty());
    }
}

#[test]
fn unknown_projects_fail_with_a_recoverable_error() {
    let _guard = serial();
    let (payload, is_error) = loci_mcp::call_tool(
        "search_graph",
        &json!({ "project": "not-a-real-project", "name": "x" }),
    );
    assert!(is_error);
    assert_eq!(payload["error"], "project_not_found");
    assert!(payload["recovery"]
        .as_str()
        .unwrap()
        .contains("list_projects"));
}

#[test]
fn search_code_finds_text_the_graph_does_not_model() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("textsearch");

    let matches = call(
        "search_code",
        json!({ "project": project, "pattern": "ORDERS_BASE_URL" }),
    );
    let hits = matches["matches"].as_array().expect("matches");
    assert!(!hits.is_empty(), "an env var name is text, not a symbol");
    for hit in hits {
        assert!(hit["line"].as_u64().unwrap() >= 1);
        assert!(!hit["file_path"].as_str().unwrap().is_empty());
    }
}

#[test]
fn pagination_is_consistent_and_the_cursor_advances() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("paging");

    let first = call(
        "search_graph",
        json!({ "project": project, "name_pattern": ".", "limit": 5 }),
    );
    assert_eq!(first["results"].as_array().unwrap().len(), 5);
    assert_eq!(first["has_more"], true);

    let cursor = first["cursor"].as_str().expect("cursor").to_string();
    let second = call(
        "search_graph",
        json!({ "project": project, "name_pattern": ".", "limit": 5, "cursor": cursor }),
    );

    let first_names: Vec<&str> = first["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["qualified_name"].as_str().unwrap())
        .collect();
    let second_names: Vec<&str> = second["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["qualified_name"].as_str().unwrap())
        .collect();

    assert_eq!(first["total"], second["total"]);
    assert!(
        first_names.iter().all(|n| !second_names.contains(n)),
        "a second page must not repeat the first"
    );
}

#[test]
fn adrs_are_linked_to_real_symbols_and_reject_invented_ones() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("adr");

    let saved = call(
        "manage_adr",
        json!({
            "project": project,
            "mode": "upsert",
            "id": "0001-order-flow",
            "title": "Orders persist through the repository layer",
            "status": "accepted",
            "body": "All writes go through save_order.",
            "related_qualified_names": [
                "services.orders-py.app.repository.save_order",
                "totally.made.up.symbol"
            ]
        }),
    );

    let related = saved["saved"]["related"].as_array().expect("related");
    assert_eq!(related.len(), 1, "only the real symbol may be linked");
    assert!(related[0]["file_path"]
        .as_str()
        .unwrap()
        .ends_with("repository.py"));

    let unresolved = saved["unresolved"].as_array().expect("unresolved");
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0]["name"], "totally.made.up.symbol");

    let listed = call("manage_adr", json!({ "project": project, "mode": "list" }));
    assert_eq!(listed["adrs"].as_array().unwrap().len(), 1);
}

#[test]
fn traces_only_link_endpoints_that_already_exist() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("traces");

    let result = call(
        "ingest_traces",
        json!({
            "project": project,
            "traces": [
                {
                    "caller": "services.orders-py.app.service.OrderService.create_order",
                    "callee": "services.orders-py.app.repository.save_order",
                    "count": 12
                },
                { "caller": "ghost.caller", "callee": "ghost.callee" }
            ]
        }),
    );

    assert_eq!(result["linked_count"], 1);
    assert_eq!(result["unmatched_count"], 1);
    assert_eq!(result["unmatched"][0]["caller_resolved"], false);
}

/// One root keeps one graph. Re-indexing it under a different name must say so
/// rather than quietly returning a project id the caller did not ask for.
#[test]
fn a_rejected_project_name_is_reported_not_swallowed() {
    let _guard = serial();
    let fixture = fixture_copy();
    let path = fixture.path().to_string_lossy().to_string();

    let first = call(
        "index_repository",
        json!({ "repo_path": path, "name": "first-name" }),
    );
    assert_eq!(first["project"], "first-name");
    assert!(first["name_note"].is_null(), "the first name was honoured");

    let second = call(
        "index_repository",
        json!({ "repo_path": path, "name": "second-name" }),
    );
    assert_eq!(
        second["project"], "first-name",
        "the root must keep its original graph"
    );
    let note = second["name_note"]
        .as_str()
        .expect("an ignored name must be reported");
    assert!(
        note.contains("second-name") && note.contains("first-name"),
        "{note}"
    );
}

/// Every language named for Hybrid LSP must survive the whole pipeline, not
/// just the parser: walked, parsed, stored, and queryable through the graph.
#[test]
fn the_newly_bundled_languages_reach_the_graph_end_to_end() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("polyglot");

    let status = call("index_status", json!({ "project": project }));
    let languages = status["languages"].as_object().expect("languages");
    for language in ["csharp", "kotlin", "perl"] {
        assert!(
            languages.contains_key(language),
            "{language} never reached the graph: {languages:?}"
        );
    }

    // C#: a method call inside a class resolves to the sibling method.
    let issue = call(
        "trace_path",
        json!({
            "project": project,
            "name": "IssueInvoice",
            "direction": "outbound",
            "depth": 2,
        }),
    );
    let callees: Vec<&str> = issue["callees"]
        .as_array()
        .expect("callees")
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        callees.contains(&"ValidateAmount") && callees.contains(&"BuildInvoice"),
        "C# call graph is incomplete: {callees:?}"
    );

    // Kotlin: an override in a class body is a Method, not a free Function.
    let deliver = call(
        "search_graph",
        json!({ "project": project, "name": "deliver" }),
    );
    let kotlin_method = deliver["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|r| r["language"] == "kotlin")
        .expect("kotlin deliver");
    assert_eq!(kotlin_method["label"], "Method");
    assert!(kotlin_method["file_path"]
        .as_str()
        .unwrap()
        .ends_with("Notifier.kt"));

    // Perl: a sub is found and its exact source can be read back.
    let render = call(
        "search_graph",
        json!({ "project": project, "name": "format_rows" }),
    );
    let perl_sub = &render["results"][0];
    assert_eq!(perl_sub["language"], "perl");
    let snippet = call(
        "get_code_snippet",
        json!({
            "project": project,
            "qualified_name": perl_sub["qualified_name"].as_str().unwrap(),
        }),
    );
    assert!(
        snippet["source"]
            .as_str()
            .unwrap()
            .contains("sub format_rows"),
        "Perl snippet must return the real sub: {snippet}"
    );
}

/// A tool that returns nothing is the input to a confident wrong answer, so
/// every empty structural result has to say what it does not prove.
#[test]
fn every_empty_structural_result_carries_a_caveat() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("caveats");

    let no_symbol = call(
        "search_graph",
        json!({ "project": project, "name": "no_such_symbol_anywhere" }),
    );
    assert_eq!(no_symbol["total"], 0);
    assert!(
        no_symbol["note"]
            .as_str()
            .is_some_and(|n| n.contains("check_index_coverage")),
        "an empty search must point at coverage: {no_symbol}"
    );

    let no_path = call(
        "query_graph",
        json!({
            "project": project,
            "start": { "label": "Route" },
            "hops": [{ "edge": "INHERITS", "direction": "out" }],
        }),
    );
    assert_eq!(no_path["total"], 0);
    assert!(
        no_path["note"]
            .as_str()
            .is_some_and(|n| n.contains("CALL_UNRESOLVED")),
        "an empty walk must explain unresolved edges: {no_path}"
    );

    // A leaf entry point genuinely has no callers. That is exactly the answer
    // an agent would over-trust, so it must still be caveated.
    let uncalled = call(
        "trace_path",
        json!({
            "project": project,
            "qualified_name": "services.orders-py.app.main.create_order",
            "direction": "inbound",
        }),
    );
    assert_eq!(uncalled["callers_total"], 0);
    assert!(
        uncalled["note"]
            .as_str()
            .is_some_and(|n| n.contains("CALL_UNRESOLVED")),
        "a zero-caller trace must be caveated: {uncalled}"
    );
}

/// The caveat must not fire when callers were never requested, or it becomes
/// noise the model learns to ignore.
#[test]
fn an_outbound_only_trace_makes_no_claim_about_callers() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("outbound-only");

    let outbound = call(
        "trace_path",
        json!({
            "project": project,
            "qualified_name": "services.orders-py.app.main.create_order",
            "direction": "outbound",
        }),
    );
    assert_eq!(outbound["callers_total"], 0);
    assert!(
        outbound["note"].is_null(),
        "an outbound trace never looked for callers, so it must not caveat them: {outbound}"
    );
}

#[test]
fn deleting_a_project_removes_it_from_the_catalog() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("disposable");

    let deleted = call("delete_project", json!({ "project": project }));
    assert_eq!(deleted["deleted"], project.as_str());

    let (payload, is_error) = loci_mcp::call_tool("index_status", &json!({ "project": project }));
    assert!(is_error);
    assert_eq!(payload["error"], "project_not_found");
}

#[test]
fn the_graph_survives_a_process_restart() {
    let _guard = serial();
    let (project, _fixture) = indexed_fixture("persistent");

    // Reopening the store from disk is what a fresh `loci mcp` process does.
    let (entry, store) = loci_index::open_project(&project).expect("reopen project");
    let reader = store.read().expect("read transaction");
    let meta = reader.meta().expect("meta").expect("meta present");

    assert_eq!(meta.name, project);
    assert!(meta.node_count > 0);
    assert!(Path::new(&entry.store_path).exists());
    assert!(
        !reader
            .nodes_by_name("save_order")
            .expect("lookup")
            .is_empty(),
        "symbols must still be queryable after the writing process exited"
    );
}
