use loci_core::{LociError, Result, Sandbox};
use loci_graph::{
    catalog::{Catalog, ProjectEntry},
    coverage::{self, COVERAGE_NOTE},
    query::{self, Direction, PatternQuery, SearchRequest},
    Edge, EdgeType, Evidence, GraphStore, NodeLabel,
};
use loci_index::{changes, walk::IgnoreMatcher, IndexOptions};
use serde_json::{json, Value};
use std::path::Path;

/// Parse a `cursor` string back into an offset.
fn offset_from(args: &Value) -> Result<usize> {
    match args.get("cursor") {
        None | Some(Value::Null) => Ok(0),
        Some(Value::String(s)) => s.parse().map_err(|_| LociError::BadCursor),
        Some(_) => Err(LociError::BadCursor),
    }
}

fn limit_from(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(default)
        .clamp(1, 1000)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| LociError::InvalidArgument(format!("'{key}' is required")))
}

fn first_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        args.get(*key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    })
}

fn looks_like_regex(value: &str) -> bool {
    value.chars().any(|c| {
        matches!(
            c,
            '^' | '$' | '*' | '+' | '?' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '\\'
        )
    })
}

/// Map the names agents actually send onto the SearchRequest fields.
fn apply_query_alias(
    args: &Value,
    name: Option<String>,
    name_pattern: Option<String>,
) -> (Option<String>, Option<String>) {
    if name.is_some() || name_pattern.is_some() {
        return (name, name_pattern);
    }
    match first_str(args, &["query"]) {
        Some(query) if looks_like_regex(query) => (None, Some(query.to_string())),
        Some(query) => (Some(query.to_string()), None),
        None => (name, name_pattern),
    }
}

fn normalize_start_selector(mut start: Value) -> Value {
    let Some(object) = start.as_object_mut() else {
        return start;
    };
    if !object.contains_key("name")
        && !object.contains_key("name_pattern")
        && !object.contains_key("qualified_name")
    {
        if let Some(query) = object.remove("query") {
            if query.as_str().is_some_and(looks_like_regex) {
                object.insert("name_pattern".into(), query);
            } else {
                object.insert("name".into(), query);
            }
        }
    }
    start
}

fn normalize_hops(hops: Value) -> Value {
    let Some(items) = hops.as_array() else {
        return hops;
    };
    let mapped: Vec<Value> = items
        .iter()
        .map(|hop| {
            if let Some(edge) = hop.as_str() {
                return json!({ "edge": edge });
            }
            let mut hop = hop.clone();
            if let Some(object) = hop.as_object_mut() {
                if !object.contains_key("edge") {
                    if let Some(edge_type) = object.remove("edge_type") {
                        object.insert("edge".into(), edge_type);
                    }
                }
                if let Some(Value::String(direction)) = object.get("direction") {
                    let mapped = match direction.as_str() {
                        "inbound" | "callers" => "in",
                        "outbound" | "callees" => "out",
                        other => other,
                    };
                    object.insert("direction".into(), json!(mapped));
                }
            }
            hop
        })
        .collect();
    json!(mapped)
}

/// Open a project's graph and a sandbox over its root.
///
/// The sandbox is built from the recorded root, which may have been deleted
/// since indexing; that surfaces as an explicit error rather than a panic.
fn open(project: &str) -> Result<(GraphStore, Sandbox, ProjectEntry)> {
    open_with(project, loci_index::open_project)
}

fn open_write(project: &str) -> Result<(GraphStore, Sandbox, ProjectEntry)> {
    open_with(project, loci_index::open_project_write)
}

fn open_with(
    project: &str,
    open_store: fn(&str) -> Result<(ProjectEntry, GraphStore)>,
) -> Result<(GraphStore, Sandbox, ProjectEntry)> {
    let (entry, store) = open_store(project)?;
    let root = Path::new(&entry.root);
    let sandbox = Sandbox::new(root).map_err(|_| {
        LociError::InvalidArgument(format!(
            "project '{project}' was indexed at '{}', which is no longer readable; re-index it",
            entry.root
        ))
    })?;
    Ok((store, sandbox, entry))
}

pub fn list_projects(args: &Value) -> Result<Value> {
    let catalog = Catalog::load()?;
    let offset = offset_from(args)?;
    let limit = limit_from(args, 50);

    let total = catalog.projects.len();
    let page: Vec<Value> = catalog
        .projects
        .iter()
        .skip(offset)
        .take(limit)
        .map(|entry| {
            json!({
                "project": entry.id,
                "name": entry.name,
                "root": entry.root,
                "store_path": entry.store_path,
                "indexed_at_unix": entry.indexed_at_unix,
                "graph_present": Path::new(&entry.store_path).exists(),
            })
        })
        .collect();
    let has_more = offset + page.len() < total;

    Ok(json!({
        "projects": page,
        "total": total,
        "has_more": has_more,
        "cursor": has_more.then(|| (offset + limit).to_string()),
        "next_step": if total == 0 {
            "No repository is indexed yet. Call index_repository with an absolute repo_path."
        } else {
            "Pass one of these `project` ids to the other tools."
        },
    }))
}

pub fn index_repository(args: &Value) -> Result<Value> {
    let repo_path = required_str(args, "repo_path")?;
    let options = IndexOptions {
        name: args.get("name").and_then(Value::as_str).map(str::to_string),
        full: args.get("full").and_then(Value::as_bool).unwrap_or(false),
        hybrid_lsp: args
            .get("hybrid_lsp")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };

    let report = loci_index::index_repository(Path::new(repo_path), &options)?;
    serde_json::to_value(report).map_err(|e| LociError::Storage(e.to_string()))
}

pub fn delete_project(args: &Value) -> Result<Value> {
    let project = required_str(args, "project")?;
    let entry = loci_index::delete_project(project)?;
    Ok(json!({
        "deleted": entry.id,
        "root": entry.root,
        "store_path_removed": entry.store_path,
        "note": "The source repository was not touched. Re-index to recreate the graph.",
    }))
}

pub fn index_status(args: &Value) -> Result<Value> {
    let requested = required_str(args, "project")?;
    let (store, _, entry) = open(requested)?;
    let project = entry.id.as_str();
    let root = entry.root.as_str();
    let reader = store.read()?;

    let meta = reader
        .meta()?
        .ok_or_else(|| LociError::IndexMissing(project.to_string()))?;
    let summary = coverage::summarise(&reader)?;
    let labels = reader.label_counts()?;

    let parse_partial: Vec<Value> = reader
        .files_with_status(loci_graph::CoverageStatus::ParsePartial)?
        .iter()
        .take(25)
        .map(|f| json!({ "path": f.path, "detail": f.detail }))
        .collect();
    let skipped_files = reader.files_with_status(loci_graph::CoverageStatus::Skipped)?;
    let skipped: Vec<Value> = coverage::sample_files(&skipped_files, 25)
        .into_iter()
        .map(|s| {
            json!({
                "path": s.file.path,
                "reason": s.file.reason.map(|r| r.as_str()),
                "detail": s.file.detail,
                "others_like_it": s.others_like_it,
            })
        })
        .collect();

    Ok(json!({
        "project": project,
        "root": root,
        "schema_version": meta.schema_version,
        "indexed_at_unix": meta.indexed_at_unix,
        "last_run_duration_ms": meta.duration_ms,
        "nodes": meta.node_count,
        "edges": meta.edge_count,
        "files": meta.file_count,
        "languages": meta.languages,
        "labels": labels,
        "bundled_languages": meta.bundled_languages,
        "coverage": {
            "indexed": summary.indexed,
            "parse_partial": summary.parse_partial,
            "skipped": summary.skipped,
            "parse_partial_examples": parse_partial,
            "skipped_examples": skipped,
            "note": COVERAGE_NOTE,
        },
        "next_step": "Run detect_changes to see whether the working tree has drifted from this index.",
    }))
}

pub fn check_index_coverage(args: &Value) -> Result<Value> {
    let project = required_str(args, "project")?;
    let paths = args.get("paths").and_then(Value::as_array);
    let scopes = args.get("scopes").and_then(Value::as_array);

    let default_scope = json!(["."]);
    let scopes = match scopes {
        Some(s) if !s.is_empty() => Some(s),
        _ if paths.is_none_or(|p| p.is_empty()) => Some(default_scope.as_array().expect("array")),
        _ => scopes,
    };

    let (store, sandbox, entry) = open(project)?;
    let project = entry.id.as_str();
    let reader = store.read()?;
    let matcher = IgnoreMatcher::build(sandbox.root());

    let mut path_results = Vec::new();
    for entry in paths.unwrap_or(&Vec::new()) {
        let Some(path) = entry.as_str() else { continue };
        let excluded = matcher.is_excluded(path, false);
        let exists = sandbox
            .resolve(path)
            .map(|resolved| resolved.exists())
            .unwrap_or(false);
        path_results.push(serde_json::to_value(coverage::classify_path(
            &reader, path, excluded, exists,
        )?)?);
    }

    let limit = limit_from(args, 200);
    let offset = offset_from(args)?;
    let mut scope_results = Vec::new();
    let mut scope_total = 0usize;
    let mut scope_has_more = false;
    for entry in scopes.unwrap_or(&Vec::new()) {
        let Some(prefix) = entry.as_str() else {
            continue;
        };
        let (files, total, has_more) = coverage::classify_scope(&reader, prefix, limit, offset)?;
        scope_total += total;
        scope_has_more |= has_more;
        scope_results.push(json!({
            "scope": prefix,
            "files": files,
            "total": total,
            "has_more": has_more,
        }));
    }

    Ok(json!({
        "project": project,
        "paths": path_results,
        "scopes": scope_results,
        "total": scope_total,
        "has_more": scope_has_more,
        "cursor": scope_has_more.then(|| (offset + limit).to_string()),
        "note": COVERAGE_NOTE,
    }))
}

pub fn detect_changes(args: &Value) -> Result<Value> {
    let requested = required_str(args, "project")?;
    let (store, sandbox, entry) = open(requested)?;
    let project = entry.id.as_str();
    let report = changes::detect_changes(
        &store,
        &sandbox,
        project,
        limit_from(args, 200),
        offset_from(args)?,
    )?;
    serde_json::to_value(report).map_err(|e| LociError::Storage(e.to_string()))
}

fn search_request_from(args: &Value, limit: usize, offset: usize) -> SearchRequest {
    let (name, name_pattern) = apply_query_alias(
        args,
        args.get("name").and_then(Value::as_str).map(str::to_string),
        args.get("name_pattern")
            .and_then(Value::as_str)
            .map(str::to_string),
    );
    SearchRequest {
        name,
        qualified_name: args
            .get("qualified_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        name_pattern,
        label: args
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string),
        file_pattern: args
            .get("file_pattern")
            .and_then(Value::as_str)
            .map(str::to_string),
        limit: Some(limit),
        offset: Some(offset),
    }
}

pub fn search_graph(args: &Value) -> Result<Value> {
    let requested = required_str(args, "project")?;
    let (store, _, entry) = open(requested)?;
    let project = entry.id.as_str();
    let reader = store.read()?;

    let limit = limit_from(args, 50);
    let offset = offset_from(args)?;
    let request = search_request_from(args, limit, offset);
    let response = query::search(&reader, &request)?;

    let mut payload = serde_json::to_value(&response)?;
    payload["project"] = json!(project);
    if response.total == 0 {
        payload["note"] = json!(
            "No symbol matched. This means the graph holds no such symbol, not that the code \
             lacks one: check_index_coverage will say whether the relevant files were indexed."
        );
    }
    Ok(payload)
}

pub fn query_graph(args: &Value) -> Result<Value> {
    let requested = required_str(args, "project")?;
    let (store, _, entry) = open(requested)?;
    let project = entry.id.as_str();
    let reader = store.read()?;

    let start = match args.get("start") {
        None | Some(Value::Null) => {
            return Err(LociError::InvalidArgument(
                if args.get("edge_type").is_some() {
                    "'start' is required. To walk an edge type, pass start (for example \
                     {\"label\":\"Route\"}) and hops=[{\"edge\":\"ROUTES_TO\"}]. A top-level \
                     'edge_type' is not a query."
                        .to_string()
                } else {
                    "'start' is required".to_string()
                },
            ));
        }
        Some(Value::String(qualified_name)) => json!({ "qualified_name": qualified_name }),
        Some(other) => other.clone(),
    };
    let start = normalize_start_selector(start);
    let start: SearchRequest = serde_json::from_value(start)
        .map_err(|e| LociError::InvalidArgument(format!("bad 'start' selector: {e}")))?;

    let hops = normalize_hops(args.get("hops").cloned().unwrap_or_else(|| json!([])));
    let pattern = PatternQuery {
        start,
        hops: serde_json::from_value(hops)
            .map_err(|e| LociError::InvalidArgument(format!("bad 'hops': {e}")))?,
        limit: Some(limit_from(args, 50)),
        offset: Some(offset_from(args)?),
    };

    let response = query::run_pattern(&reader, &pattern)?;
    let mut payload = serde_json::to_value(&response)?;
    payload["project"] = json!(project);
    if response.total == 0 {
        // An empty walk is the shape of a false negative, so say what it does
        // and does not prove before the agent concludes anything from it.
        payload["note"] = json!(
            "No path matched this pattern. Unresolved calls are stored as CALL_UNRESOLVED rather \
             than CALLS, so a missing path is not proof that no such relationship exists in the \
             code: check_index_coverage for the paths involved."
        );
    }
    Ok(payload)
}

pub fn trace_path(args: &Value) -> Result<Value> {
    let requested = required_str(args, "project")?;
    let (store, _, entry) = open(requested)?;
    let project = entry.id.as_str();
    let reader = store.read()?;

    let reference = first_str(args, &["qualified_name", "name", "from"]).ok_or_else(|| {
        LociError::InvalidArgument("pass 'qualified_name' (preferred) or 'name'".to_string())
    })?;

    let node = match query::resolve_symbol(&reader, reference) {
        Ok(node) => node,
        Err(LociError::AmbiguousSymbol { name, count }) => {
            return Ok(json!({
                "project": project,
                "error": "ambiguous_symbol",
                "message": format!("'{name}' matches {count} symbols; retry with an exact qualified_name"),
                "candidates": query::symbol_candidates(&reader, reference)?,
            }));
        }
        Err(e) => return Err(e),
    };

    let direction = Direction::parse(
        args.get("direction")
            .and_then(Value::as_str)
            .unwrap_or("both"),
    )?;
    let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(3) as u32;

    let response = query::trace(
        &reader,
        &node,
        direction,
        depth,
        limit_from(args, 100),
        offset_from(args)?,
    )?;

    let mut payload = serde_json::to_value(&response)?;
    payload["project"] = json!(project);
    payload["file_path"] = json!(node.file_path);
    payload["start_line"] = json!(node.start_line);
    payload["end_line"] = json!(node.end_line);
    // "Nothing calls this" is the most expensive wrong answer this tool can
    // give, so every zero-caller result is caveated, not just the ones that
    // happen to have unresolved edges attached.
    if response.callers_total == 0 && direction.includes_inbound() {
        payload["note"] = json!(if response.unresolved.is_empty() {
            "No resolved callers were found. Calls that could not be pinned to a definition are \
             stored as CALL_UNRESOLVED and are not counted here, and dynamic or reflective calls \
             are invisible to AST analysis. Check check_index_coverage before concluding that \
             nothing calls this."
        } else {
            "No resolved callers, but this symbol has unresolved calls attached. Unresolved edges \
             elsewhere may also point here; absence of CALLS is not proof of no caller."
        });
    }
    Ok(payload)
}

pub fn get_code_snippet(args: &Value) -> Result<Value> {
    let project = required_str(args, "project")?;
    let reference = first_str(args, &["qualified_name", "name"])
        .ok_or_else(|| LociError::InvalidArgument("'qualified_name' is required".to_string()))?;
    let context = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(50) as u32;

    let (store, sandbox, entry) = open(project)?;
    let project = entry.id.as_str();
    let reader = store.read()?;

    let node = match query::resolve_symbol(&reader, reference) {
        Ok(node) => node,
        Err(LociError::AmbiguousSymbol { name, count }) => {
            return Ok(json!({
                "project": project,
                "error": "ambiguous_symbol",
                "message": format!("'{name}' matches {count} symbols; retry with an exact qualified_name"),
                "candidates": query::symbol_candidates(&reader, reference)?,
            }));
        }
        Err(e) => return Err(e),
    };

    let source = sandbox.read_to_string(&node.file_path)?;
    let lines: Vec<&str> = source.lines().collect();

    let start = node.start_line.saturating_sub(1).saturating_sub(context) as usize;
    let end = ((node.end_line + context) as usize).min(lines.len());
    let snippet = lines
        .get(start..end)
        .map(|slice| slice.join("\n"))
        .unwrap_or_default();

    let coverage_note = reader
        .file_record(&node.file_path)?
        .filter(|record| record.status == loci_graph::CoverageStatus::ParsePartial)
        .map(|record| {
            format!(
                "{} was only partially parsed ({}); the source below is ground truth, the graph may be incomplete here.",
                record.path,
                record.detail.unwrap_or_default()
            )
        });

    Ok(json!({
        "project": project,
        "qualified_name": node.qualified_name,
        "name": node.name,
        "label": node.label.as_str(),
        "file_path": node.file_path,
        "start_line": start as u32 + 1,
        "end_line": end as u32,
        "symbol_start_line": node.start_line,
        "symbol_end_line": node.end_line,
        "language": node.language.map(|l| l.as_str()),
        "signature": node.signature,
        "source": snippet,
        "coverage_note": coverage_note,
    }))
}

pub fn get_graph_schema(_args: &Value) -> Result<Value> {
    let labels: Vec<Value> = NodeLabel::ALL
        .iter()
        .map(|label| {
            json!({
                "label": label.as_str(),
                "callable": label.is_callable(),
            })
        })
        .collect();

    let edges: Vec<Value> = EdgeType::ALL
        .iter()
        .map(|edge| {
            json!({
                "edge_type": edge.as_str(),
                "description": edge_description(*edge),
            })
        })
        .collect();

    let grammars: Vec<Value> = loci_parse::BUNDLED
        .iter()
        .map(|g| {
            json!({
                "language": g.language.as_str(),
                "grammar_crate": g.grammar_crate,
            })
        })
        .collect();

    let lsp: Vec<Value> = loci_lsp::detect()
        .into_iter()
        .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
        .collect();

    Ok(json!({
        "schema_version": loci_core::SCHEMA_VERSION,
        "node_labels": labels,
        "edge_types": edges,
        "evidence_values": ["ast", "lsp", "hybrid", "trace"],
        "qualified_name_format":
            "Dot-separated for every language: <module path from the file, without extension> \
             followed by each enclosing definition, then the symbol name. Example: \
             app.service.OrderService.create_order.",
        "bundled_languages": loci_parse::bundled_language_ids(),
        "bundled_grammars": grammars,
        "hybrid_lsp": {
            "implemented": true,
            "default": "off",
            "note": "Indexing is AST-first: every node, and every edge the parser resolves, carries \
                     source=\"ast\". Pass hybrid_lsp=true to index_repository to additionally ask an \
                     installed language server about the calls the AST could not settle, such as a \
                     method reached through a variable. Those upgraded edges carry source=\"lsp\", \
                     so evidence is always distinguishable. It is off by default because starting a \
                     server costs seconds; with it off, unresolvable calls stay CALL_UNRESOLVED \
                     rather than being guessed. A missing or slow server never fails the index, it \
                     is reported in hybrid_lsp.languages with a skipped_reason.",
            "servers": lsp,
            "servers_note": "on_path means the executable exists, which is not proof it runs: a \
                             rustup shim for an uninstalled component is on PATH and fails on first \
                             use. The index report says what each server actually resolved.",
        },
        "not_supported": {
            "languages": ["php"],
            "note": "PHP is out of scope by design: no grammar, no language server, no fixtures.",
        },
    }))
}

fn edge_description(edge: EdgeType) -> &'static str {
    match edge {
        EdgeType::Contains => "A definition lexically contains another definition.",
        EdgeType::Defines => "A file declares a top-level symbol or route.",
        EdgeType::Calls => "A resolved call. `detail` records how it was resolved.",
        EdgeType::CallUnresolved => {
            "A call whose target could not be pinned to one definition. Carries the callee name \
             and the reason; the destination is deliberately empty."
        }
        EdgeType::Imports => "A file imports another indexed file.",
        EdgeType::Inherits => "A type extends another type.",
        EdgeType::Implements => "A type implements an interface or trait.",
        EdgeType::HasField => "A type owns a field.",
        EdgeType::RoutesTo => "An HTTP route dispatches to a handler.",
        EdgeType::ConfigRef => "A symbol references a configuration key found in the repository.",
        EdgeType::ProtoRef => "A symbol references a protobuf definition in the repository.",
        EdgeType::OpenApiRef => "A symbol references an OpenAPI operation in the repository.",
        EdgeType::Impacts => "Observed at runtime through ingest_traces, not from static analysis.",
    }
}

pub fn get_architecture(args: &Value) -> Result<Value> {
    let requested = required_str(args, "project")?;
    let scope = args.get("path").and_then(Value::as_str).unwrap_or("");
    let (store, _, entry) = open(requested)?;
    let project = entry.id.as_str();
    let root = entry.root.as_str();
    let reader = store.read()?;

    let in_scope =
        |path: &str| scope.is_empty() || path == scope || path.starts_with(&format!("{scope}/"));

    let nodes: Vec<_> = reader
        .all_nodes()?
        .into_iter()
        .filter(|n| in_scope(&n.file_path))
        .collect();

    let mut languages: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut labels: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut symbols_per_file: std::collections::BTreeMap<String, usize> = Default::default();

    for node in &nodes {
        *labels.entry(node.label.as_str()).or_insert(0) += 1;
        if node.label == NodeLabel::File {
            if let Some(language) = node.language {
                *languages.entry(language.as_str()).or_insert(0) += 1;
            }
        } else {
            *symbols_per_file.entry(node.file_path.clone()).or_insert(0) += 1;
        }
    }

    // Routes with the handler each dispatches to, straight from ROUTES_TO edges.
    let mut routes = Vec::new();
    for node in nodes.iter().filter(|n| n.label == NodeLabel::Route) {
        let handler = reader
            .neighbours(node.id, true, Some(&[EdgeType::RoutesTo]))?
            .first()
            .and_then(|(_, dst, _)| reader.node(*dst).ok().flatten())
            .map(|n| json!({ "qualified_name": n.qualified_name, "file_path": n.file_path, "start_line": n.start_line }));
        routes.push(json!({
            "method": node.extra.get("method"),
            "path": node.extra.get("path"),
            "framework": node.extra.get("framework"),
            "file_path": node.file_path,
            "line": node.start_line,
            "handler": handler,
        }));
    }
    routes.sort_by(|a, b| a["path"].to_string().cmp(&b["path"].to_string()));
    let route_total = routes.len();
    let routes_truncated = routes.len() > 50;
    routes.truncate(50);

    // Entry points: callables nothing in the graph calls.
    let mut entry_points = Vec::new();
    for node in nodes.iter().filter(|n| n.label.is_callable()) {
        if reader
            .neighbours(node.id, false, Some(&[EdgeType::Calls]))?
            .is_empty()
        {
            entry_points.push(json!({
                "qualified_name": node.qualified_name,
                "file_path": node.file_path,
                "start_line": node.start_line,
            }));
        }
    }
    entry_points.sort_by(|a, b| {
        a["qualified_name"]
            .to_string()
            .cmp(&b["qualified_name"].to_string())
    });
    let entry_point_total = entry_points.len();
    entry_points.truncate(50);

    let mut largest: Vec<Value> = symbols_per_file
        .into_iter()
        .map(|(path, count)| json!({ "file_path": path, "symbols": count }))
        .collect();
    largest.sort_by(|a, b| b["symbols"].as_u64().cmp(&a["symbols"].as_u64()));
    largest.truncate(15);

    Ok(json!({
        "project": project,
        "root": root,
        "scope": if scope.is_empty() { Value::Null } else { json!(scope) },
        "languages": languages,
        "labels": labels,
        "routes": routes,
        "route_total": route_total,
        "routes_has_more": routes_truncated,
        "entry_points": entry_points,
        "entry_point_total": entry_point_total,
        "entry_points_has_more": entry_point_total > 50,
        "largest_files_by_symbol_count": largest,
        "note": "Counted from the graph. Entry points are callables with no inbound CALLS edge in \
                 this index, which includes symbols called only from unindexed or unresolved code.",
    }))
}

pub fn search_code(args: &Value) -> Result<Value> {
    let project = required_str(args, "project")?;
    let pattern = first_str(args, &["pattern", "query"])
        .ok_or_else(|| LociError::InvalidArgument("'pattern' is required".to_string()))?;
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let (store, sandbox, entry) = open(project)?;
    let project = entry.id.as_str();
    let reader = store.read()?;

    let file_filter = match first_str(args, &["file_pattern", "path"]) {
        Some(p) => Some(
            regex::Regex::new(p)
                .map_err(|e| LociError::InvalidArgument(format!("bad file_pattern: {e}")))?,
        ),
        None => None,
    };

    let matcher = if use_regex {
        let mut builder = regex::RegexBuilder::new(pattern);
        builder.case_insensitive(!case_sensitive);
        Some(
            builder
                .build()
                .map_err(|e| LociError::InvalidArgument(format!("bad pattern: {e}")))?,
        )
    } else {
        None
    };

    let needle = if case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };

    let limit = limit_from(args, 50);
    let offset = offset_from(args)?;

    let mut hits = Vec::new();
    for record in reader.all_files()? {
        if record.status == loci_graph::CoverageStatus::Skipped {
            continue;
        }
        if let Some(filter) = &file_filter {
            if !filter.is_match(&record.path) {
                continue;
            }
        }
        let Ok(content) = sandbox.read_to_string(&record.path) else {
            continue;
        };

        for (index, line) in content.lines().enumerate() {
            let matched = match &matcher {
                Some(re) => re.is_match(line),
                None => {
                    if case_sensitive {
                        line.contains(&needle)
                    } else {
                        line.to_lowercase().contains(&needle)
                    }
                }
            };
            if !matched {
                continue;
            }

            let line_number = index as u32 + 1;
            let containing = reader
                .nodes_in_file(&record.path)?
                .into_iter()
                .filter(|n| {
                    n.label != NodeLabel::File
                        && n.start_line <= line_number
                        && line_number <= n.end_line
                })
                .min_by_key(|n| n.end_line - n.start_line);

            hits.push(json!({
                "file_path": record.path,
                "line": line_number,
                "text": line.trim_end().chars().take(400).collect::<String>(),
                "in_symbol": containing.map(|n| json!({
                    "qualified_name": n.qualified_name,
                    "label": n.label.as_str(),
                })),
            }));
        }
    }

    hits.sort_by(|a, b| {
        a["file_path"]
            .as_str()
            .cmp(&b["file_path"].as_str())
            .then(a["line"].as_u64().cmp(&b["line"].as_u64()))
    });

    let total = hits.len();
    let page: Vec<Value> = hits.into_iter().skip(offset).take(limit).collect();
    let has_more = offset + page.len() < total;

    Ok(json!({
        "project": project,
        "pattern": pattern,
        "regex": use_regex,
        "matches": page,
        "total": total,
        "has_more": has_more,
        "cursor": has_more.then(|| (offset + limit).to_string()),
        "note": "Text scan over indexed files only. Files listed as skipped by check_index_coverage \
                 were not searched.",
    }))
}

pub fn manage_adr(args: &Value) -> Result<Value> {
    let project = required_str(args, "project")?;
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("list");
    let (store, _, entry) = match mode {
        "upsert" | "delete" => open_write(project)?,
        _ => open(project)?,
    };
    let project = entry.id.as_str();

    match mode {
        "list" => {
            let reader = store.read()?;
            let mut adrs = reader.adrs()?;
            adrs.sort_by_key(|a| a["id"].as_str().unwrap_or("").to_string());
            Ok(json!({ "project": project, "adrs": adrs, "total": adrs.len(), "has_more": false }))
        }
        "get" => {
            let id = required_str(args, "id")?;
            let reader = store.read()?;
            match reader.adr(id)? {
                Some(adr) => Ok(json!({ "project": project, "adr": adr })),
                None => Err(LociError::InvalidArgument(format!(
                    "no ADR with id '{id}'; call manage_adr with mode=list"
                ))),
            }
        }
        "upsert" => {
            let id = required_str(args, "id")?;
            let title = required_str(args, "title")?;

            // Validate symbol links against the graph so an ADR cannot point at
            // something that does not exist.
            let reader = store.read()?;
            let mut related = Vec::new();
            let mut unresolved = Vec::new();
            if let Some(names) = args
                .get("related_qualified_names")
                .and_then(Value::as_array)
            {
                for entry in names {
                    let Some(name) = entry.as_str() else { continue };
                    match query::resolve_symbol(&reader, name) {
                        Ok(node) => related.push(json!({
                            "qualified_name": node.qualified_name,
                            "file_path": node.file_path,
                            "start_line": node.start_line,
                        })),
                        Err(e) => unresolved.push(json!({ "name": name, "reason": e.code() })),
                    }
                }
            }
            drop(reader);

            let adr = json!({
                "id": id,
                "title": title,
                "status": args.get("status").and_then(Value::as_str).unwrap_or("proposed"),
                "body": args.get("body").and_then(Value::as_str).unwrap_or(""),
                "related": related,
                "updated_at_unix": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });

            let writer = store.write()?;
            writer.put_adr(id, &adr)?;
            writer.commit()?;

            Ok(json!({
                "project": project,
                "saved": adr,
                "unresolved": unresolved,
                "note": if unresolved.is_empty() {
                    Value::Null
                } else {
                    json!("Some related names did not resolve to indexed symbols and were not linked.")
                },
            }))
        }
        "delete" => {
            let id = required_str(args, "id")?;
            let writer = store.write()?;
            let existed = writer.remove_adr(id)?;
            writer.commit()?;
            Ok(json!({ "project": project, "deleted": existed, "id": id }))
        }
        other => Err(LociError::InvalidArgument(format!(
            "mode must be list, get, upsert or delete; got '{other}'"
        ))),
    }
}

pub fn ingest_traces(args: &Value) -> Result<Value> {
    let project = required_str(args, "project")?;
    let traces = args
        .get("traces")
        .and_then(Value::as_array)
        .ok_or_else(|| LociError::InvalidArgument("'traces' must be an array".to_string()))?;

    let (store, _, entry) = open_write(project)?;
    let project = entry.id.as_str();

    let mut linked = Vec::new();
    let mut unmatched = Vec::new();
    let mut edges = Vec::new();

    {
        let reader = store.read()?;
        let mut next_edge_id = reader
            .meta()?
            .map(|m| m.next_edge_id.max(1))
            .unwrap_or(1)
            .saturating_add(1_000_000);

        for trace in traces {
            let caller = trace.get("caller").and_then(Value::as_str).unwrap_or("");
            let callee = trace.get("callee").and_then(Value::as_str).unwrap_or("");
            let count = trace.get("count").and_then(Value::as_u64).unwrap_or(1);

            let src = query::resolve_symbol(&reader, caller);
            let dst = query::resolve_symbol(&reader, callee);

            match (src, dst) {
                (Ok(src), Ok(dst)) => {
                    let mut edge = Edge::new(src.id, dst.id, EdgeType::Impacts);
                    edge.id = next_edge_id;
                    next_edge_id += 1;
                    edge.source = Evidence::Trace;
                    edge.detail = Some(format!("observed {count} time(s)"));
                    edges.push(edge);
                    linked.push(json!({
                        "caller": src.qualified_name,
                        "callee": dst.qualified_name,
                        "count": count,
                    }));
                }
                (src, dst) => unmatched.push(json!({
                    "caller": caller,
                    "callee": callee,
                    "caller_resolved": src.is_ok(),
                    "callee_resolved": dst.is_ok(),
                    "reason": "both endpoints must resolve to indexed symbols; no placeholder nodes are created",
                })),
            }
        }
    }

    let linked_count = edges.len();
    if !edges.is_empty() {
        let writer = store.write()?;
        for edge in &edges {
            writer.put_edge(edge)?;
        }
        writer.commit()?;
    }

    Ok(json!({
        "project": project,
        "linked": linked,
        "linked_count": linked_count,
        "unmatched": unmatched,
        "unmatched_count": unmatched.len(),
        "note": "Runtime edges are stored as IMPACTS with source=\"trace\" so they are never \
                 confused with statically resolved CALLS edges.",
    }))
}

/// Route a tool call to its handler.
pub fn dispatch(name: &str, args: &Value) -> Result<Value> {
    match name {
        "list_projects" => list_projects(args),
        "index_repository" => index_repository(args),
        "delete_project" => delete_project(args),
        "index_status" => index_status(args),
        "check_index_coverage" => check_index_coverage(args),
        "detect_changes" => detect_changes(args),
        "search_graph" => search_graph(args),
        "query_graph" => query_graph(args),
        "trace_path" => trace_path(args),
        "get_code_snippet" => get_code_snippet(args),
        "get_graph_schema" => get_graph_schema(args),
        "get_architecture" => get_architecture(args),
        "search_code" => search_code(args),
        "manage_adr" => manage_adr(args),
        "ingest_traces" => ingest_traces(args),
        other => Err(LociError::InvalidArgument(format!(
            "unknown tool '{other}'"
        ))),
    }
}
