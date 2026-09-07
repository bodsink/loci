//! Visualization subgraphs for the local web UI.
//!
//! MCP tools return paginated search rows, not a renderable adjacency list.
//! This module walks the same store those tools use and returns a capped
//! node/edge snapshot. Large projects are never dumped whole: a seed walks
//! outward, and an unseeded overview starts from source files rather than
//! every symbol.

use loci_core::{LociError, Result};
use loci_graph::{
    query, CoverageStatus, EdgeType, GraphReader, Node, NodeLabel, UNRESOLVED_NODE_ID,
};
use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};

const DEFAULT_LIMIT: usize = 220;
const MAX_LIMIT: usize = 600;
const LARGE_GRAPH_NODES: u64 = 8_000;

type GraphParts = (Vec<Value>, Vec<Value>, bool, Option<&'static str>);

const OVERVIEW_EDGES: &[EdgeType] = &[
    EdgeType::Imports,
    EdgeType::Defines,
    EdgeType::Contains,
    EdgeType::Calls,
    EdgeType::RoutesTo,
    EdgeType::Inherits,
    EdgeType::Implements,
];
const IMPORT_EDGES: &[EdgeType] = &[EdgeType::Imports];
const CALL_EDGES: &[EdgeType] = &[EdgeType::Calls];
const ROUTE_EDGES: &[EdgeType] = &[EdgeType::RoutesTo, EdgeType::Calls];
const NEIGHBOR_EDGES: &[EdgeType] = &[
    EdgeType::Calls,
    EdgeType::Imports,
    EdgeType::Inherits,
    EdgeType::Implements,
    EdgeType::RoutesTo,
    EdgeType::Defines,
    EdgeType::HasField,
];

const OVERVIEW_LABELS: &[NodeLabel] = &[
    NodeLabel::File,
    NodeLabel::Package,
    NodeLabel::Module,
    NodeLabel::Route,
    NodeLabel::Class,
    NodeLabel::Struct,
    NodeLabel::Function,
    NodeLabel::Method,
];
const IMPORT_LABELS: &[NodeLabel] = &[NodeLabel::File, NodeLabel::Package, NodeLabel::Module];
const CALL_LABELS: &[NodeLabel] = &[NodeLabel::Function, NodeLabel::Method];
const ROUTE_LABELS: &[NodeLabel] = &[NodeLabel::Route, NodeLabel::Function, NodeLabel::Method];

pub fn visualization_graph(
    project: &str,
    view: &str,
    seed: Option<&str>,
    depth: u32,
    limit: usize,
) -> Result<Value> {
    let (entry, store) = loci_index::open_project(project)?;
    let reader = store.read()?;
    let limit = if limit == 0 {
        DEFAULT_LIMIT
    } else {
        limit.clamp(20, MAX_LIMIT)
    };
    let depth = depth.clamp(1, 6);
    let view = if view.is_empty() { "overview" } else { view };

    if let Some(seed) = seed.filter(|s| !s.trim().is_empty()) {
        match query::resolve_symbol(&reader, seed) {
            Ok(start) => {
                let (nodes, edges, truncated) = walk(
                    &reader,
                    &[start],
                    edges_for(view),
                    labels_for(view),
                    depth,
                    limit,
                )?;
                return Ok(graph_payload(
                    project,
                    &entry.root,
                    view,
                    Some(seed),
                    depth,
                    nodes,
                    edges,
                    truncated,
                    None,
                ));
            }
            Err(LociError::AmbiguousSymbol { name, count }) => {
                return Ok(json!({
                    "project": project,
                    "error": "ambiguous_symbol",
                    "message": format!("'{name}' matches {count} symbols; retry with an exact qualified_name"),
                    "candidates": query::symbol_candidates(&reader, seed)?,
                }));
            }
            Err(e) => return Err(e),
        }
    }

    let meta = reader
        .meta()?
        .ok_or_else(|| LociError::IndexMissing(project.to_string()))?;

    let (nodes, edges, truncated, note) = match view {
        "routes" => routes_graph(&reader, limit)?,
        "calls" if meta.node_count > LARGE_GRAPH_NODES => {
            let seeds = seeds_from_files(&reader, CALL_LABELS, 40)?;
            let (nodes, edges, truncated) =
                walk(&reader, &seeds, CALL_EDGES, Some(CALL_LABELS), 2, limit)?;
            (
                nodes,
                edges,
                truncated,
                Some(
                    "Unseeded call graphs on large indexes start from source files, not every callable.",
                ),
            )
        }
        "imports" => file_atlas(&reader, "imports", limit)?,
        "overview" => file_atlas(&reader, "overview", limit)?,
        "neighborhood" => {
            let seeds = file_nodes(&reader, 40)?;
            let (nodes, edges, truncated) = walk(&reader, &seeds, NEIGHBOR_EDGES, None, 1, limit)?;
            (
                nodes,
                edges,
                truncated,
                Some("Pick a symbol to walk its neighbourhood. This page is a sample."),
            )
        }
        _ => ranked(
            &reader,
            labels_for(view).unwrap_or(OVERVIEW_LABELS),
            edges_for(view),
            limit,
        )?,
    };

    Ok(graph_payload(
        project,
        &entry.root,
        view,
        None,
        depth,
        nodes,
        edges,
        truncated,
        note,
    ))
}

fn edges_for(view: &str) -> &'static [EdgeType] {
    match view {
        "imports" => IMPORT_EDGES,
        "calls" => CALL_EDGES,
        "routes" => ROUTE_EDGES,
        "neighborhood" => NEIGHBOR_EDGES,
        _ => OVERVIEW_EDGES,
    }
}

fn labels_for(view: &str) -> Option<&'static [NodeLabel]> {
    match view {
        "imports" => Some(IMPORT_LABELS),
        "calls" => Some(CALL_LABELS),
        "routes" => Some(ROUTE_LABELS),
        "neighborhood" => None,
        _ => Some(OVERVIEW_LABELS),
    }
}

#[allow(clippy::too_many_arguments)]
fn graph_payload(
    project: &str,
    root: &str,
    view: &str,
    seed: Option<&str>,
    depth: u32,
    nodes: Vec<Value>,
    edges: Vec<Value>,
    truncated: bool,
    note: Option<&str>,
) -> Value {
    json!({
        "project": project,
        "root": root,
        "view": view,
        "seed": seed,
        "depth": depth,
        "nodes": nodes,
        "edges": edges,
        "node_count": nodes.len(),
        "edge_count": edges.len(),
        "truncated": truncated,
        "note": note,
    })
}

fn file_atlas(reader: &GraphReader, view: &str, limit: usize) -> Result<GraphParts> {
    let mut seeds = file_nodes(reader, 80)?;
    if view == "overview" {
        seeds.extend(nodes_with_label(reader, NodeLabel::Route, 40)?);
    }
    let (edge_types, labels, note) = if view == "imports" {
        (
            IMPORT_EDGES,
            IMPORT_LABELS,
            Some("Source files and the imports that connect them."),
        )
    } else {
        (
            OVERVIEW_EDGES,
            OVERVIEW_LABELS,
            Some("Files, packages, types and routes. Search a symbol to walk calls."),
        )
    };
    if seeds.is_empty() {
        return Ok((
            Vec::new(),
            Vec::new(),
            false,
            Some("No file nodes in this index."),
        ));
    }
    let (nodes, edges, truncated) = walk(reader, &seeds, edge_types, Some(labels), 2, limit)?;
    Ok((nodes, edges, truncated, note))
}

fn routes_graph(reader: &GraphReader, limit: usize) -> Result<GraphParts> {
    let routes = nodes_with_label(reader, NodeLabel::Route, limit)?;
    if routes.is_empty() {
        return Ok((
            Vec::new(),
            Vec::new(),
            false,
            Some("No Route nodes in this index."),
        ));
    }
    let (nodes, edges, truncated) =
        walk(reader, &routes, ROUTE_EDGES, Some(ROUTE_LABELS), 2, limit)?;
    Ok((nodes, edges, truncated, None))
}

fn ranked(
    reader: &GraphReader,
    labels: &[NodeLabel],
    edge_types: &[EdgeType],
    limit: usize,
) -> Result<GraphParts> {
    let mut scored = Vec::new();
    for node in reader.all_nodes()? {
        if !labels.contains(&node.label) {
            continue;
        }
        let outbound = reader.neighbours(node.id, true, Some(edge_types))?.len();
        let inbound = reader.neighbours(node.id, false, Some(edge_types))?.len();
        scored.push((outbound + inbound, node));
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.qualified_name.cmp(&b.1.qualified_name))
    });
    let truncated = scored.len() > limit;
    // A bag of the highest-degree nodes is often disconnected. Start from a
    // smaller hub set and walk outward so the atlas has edges to draw.
    let seed_cap = (limit / 5).max(12).min(scored.len());
    let kept: Vec<Node> = scored
        .into_iter()
        .take(seed_cap)
        .map(|(_, node)| node)
        .collect();
    if kept.is_empty() {
        return Ok((Vec::new(), Vec::new(), false, None));
    }
    let (nodes, edges, walk_truncated) = walk(reader, &kept, edge_types, Some(labels), 2, limit)?;
    Ok((
        nodes,
        edges,
        truncated || walk_truncated,
        truncated.then_some("Showing the most connected nodes in this view."),
    ))
}

fn walk(
    reader: &GraphReader,
    starts: &[Node],
    edge_types: &[EdgeType],
    labels: Option<&[NodeLabel]>,
    depth: u32,
    limit: usize,
) -> Result<(Vec<Value>, Vec<Value>, bool)> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut queue: VecDeque<(u64, u32)> = VecDeque::new();
    let mut ordered = Vec::new();

    for start in starts {
        if seen.insert(start.id) {
            ordered.push(start.clone());
            queue.push_back((start.id, 0));
        }
    }

    let mut edge_rows: Vec<(u64, u64, EdgeType, String, Option<String>)> = Vec::new();
    let mut edge_seen: HashSet<(u64, u64, &'static str)> = HashSet::new();
    let mut truncated = false;

    while let Some((node_id, hop)) = queue.pop_front() {
        if hop >= depth {
            continue;
        }
        for outbound in [true, false] {
            for (edge_type, other, edge_id) in
                reader.neighbours(node_id, outbound, Some(edge_types))?
            {
                if other == UNRESOLVED_NODE_ID {
                    continue;
                }
                let Some(other_node) = reader.node(other)? else {
                    continue;
                };
                if let Some(allowed) = labels {
                    if !allowed.contains(&other_node.label) && !seen.contains(&other) {
                        continue;
                    }
                }

                let (src, dst) = if outbound {
                    (node_id, other)
                } else {
                    (other, node_id)
                };
                if edge_seen.insert((src, dst, edge_type.as_str())) {
                    let stored = reader.edge(edge_id)?;
                    let evidence = stored
                        .as_ref()
                        .map(|edge| edge.source.as_str().to_string())
                        .unwrap_or_else(|| "ast".to_string());
                    let detail = stored.and_then(|edge| edge.detail);
                    edge_rows.push((src, dst, edge_type, evidence, detail));
                }

                if seen.contains(&other) {
                    continue;
                }
                if ordered.len() >= limit {
                    truncated = true;
                    continue;
                }
                seen.insert(other);
                ordered.push(other_node);
                queue.push_back((other, hop + 1));
            }
        }
    }

    let keep: HashSet<u64> = ordered.iter().map(|n| n.id).collect();
    let mut nodes = Vec::with_capacity(ordered.len());
    for node in &ordered {
        nodes.push(node_value(reader, node)?);
    }

    let mut edges = Vec::new();
    for (src, dst, edge_type, evidence, detail) in edge_rows {
        if keep.contains(&src) && keep.contains(&dst) {
            edges.push(json!({
                "src": src,
                "dst": dst,
                "edge_type": edge_type.as_str(),
                "source": evidence,
                "detail": detail,
            }));
        }
    }

    Ok((nodes, edges, truncated))
}

fn node_value(reader: &GraphReader, node: &Node) -> Result<Value> {
    let out_degree = reader.neighbours(node.id, true, None)?.len();
    let in_degree = reader.neighbours(node.id, false, None)?.len();
    let extra: Map<String, Value> = node
        .extra
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    Ok(json!({
        "id": node.id,
        "name": node.name,
        "qualified_name": node.qualified_name,
        "label": node.label.as_str(),
        "file_path": node.file_path,
        "start_line": node.start_line,
        "end_line": node.end_line,
        "language": node.language.map(|l| l.as_str()),
        "source": node.source.as_str(),
        "signature": node.signature,
        "extra": extra,
        "in_degree": in_degree,
        "out_degree": out_degree,
    }))
}

fn nodes_with_label(reader: &GraphReader, label: NodeLabel, cap: usize) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    for file in reader.all_files()? {
        if file.status == CoverageStatus::Skipped {
            continue;
        }
        for node in reader.nodes_in_file(&file.path)? {
            if node.label == label {
                out.push(node);
                if out.len() >= cap {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

fn file_nodes(reader: &GraphReader, cap: usize) -> Result<Vec<Node>> {
    let mut files = reader.all_files()?;
    files.retain(|file| file.status != CoverageStatus::Skipped);
    files.sort_by(|a, b| {
        source_score(&b.path)
            .cmp(&source_score(&a.path))
            .then(a.path.cmp(&b.path))
    });
    let mut out = Vec::new();
    for file in files.into_iter().take(cap.saturating_mul(2)) {
        if let Some(node) = reader
            .nodes_in_file(&file.path)?
            .into_iter()
            .find(|n| n.label == NodeLabel::File)
        {
            out.push(node);
            if out.len() >= cap {
                break;
            }
        }
    }
    Ok(out)
}

fn seeds_from_files(reader: &GraphReader, labels: &[NodeLabel], cap: usize) -> Result<Vec<Node>> {
    let mut files = reader.all_files()?;
    files.retain(|file| file.status != CoverageStatus::Skipped);
    files.sort_by(|a, b| {
        source_score(&b.path)
            .cmp(&source_score(&a.path))
            .then(a.path.cmp(&b.path))
    });
    let mut out = Vec::new();
    for file in files {
        for node in reader.nodes_in_file(&file.path)? {
            if labels.contains(&node.label) {
                out.push(node);
                if out.len() >= cap {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

fn source_score(path: &str) -> i32 {
    let lower = path.to_ascii_lowercase();
    let mut score = 0;
    for part in ["src/", "crates/", "lib/", "services/", "app/", "internal/"] {
        if lower.contains(part) {
            score += 2;
        }
    }
    for ext in [".rs", ".go", ".ts", ".tsx", ".py", ".js", ".jsx"] {
        if lower.ends_with(ext) {
            score += 1;
        }
    }
    if lower.contains("test") || lower.contains("fixture") {
        score -= 1;
    }
    score
}
