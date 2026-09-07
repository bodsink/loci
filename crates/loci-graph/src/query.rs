use crate::schema::{Edge, EdgeType, Node, NodeLabel, UNRESOLVED_NODE_ID};
use crate::store::GraphReader;
use loci_core::{LociError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

/// Upper bound on nodes pulled from a prefix scan before filtering. Keeps a
/// one-character pattern from degenerating into a full-graph scan.
const PREFIX_SCAN_CAP: usize = 20_000;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchRequest {
    /// Exact simple name, case-insensitive. Cheapest lookup.
    pub name: Option<String>,
    /// Exact qualified name.
    pub qualified_name: Option<String>,
    /// Regex over the simple name. A literal prefix, if present, is used to
    /// narrow the scan before the regex runs.
    pub name_pattern: Option<String>,
    pub label: Option<String>,
    /// Regex over the repository-relative file path.
    pub file_pattern: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub qualified_name: String,
    pub name: String,
    pub label: &'static str,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub language: Option<String>,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub in_degree: usize,
    pub out_degree: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchHit>,
    /// Exact count of matches before limit/offset.
    pub total: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

fn to_hit(reader: &GraphReader, node: &Node) -> Result<SearchHit> {
    let out_degree = reader.neighbours(node.id, true, None)?.len();
    let in_degree = reader.neighbours(node.id, false, None)?.len();
    Ok(SearchHit {
        qualified_name: node.qualified_name.clone(),
        name: node.name.clone(),
        label: node.label.as_str(),
        file_path: node.file_path.clone(),
        start_line: node.start_line,
        end_line: node.end_line,
        language: node.language.map(|l| l.as_str().to_string()),
        source: node.source.as_str(),
        signature: node.signature.clone(),
        in_degree,
        out_degree,
    })
}

/// Longest literal prefix of a regex, used to turn a pattern into a range scan.
fn literal_prefix(pattern: &str) -> String {
    let mut prefix = String::new();
    let mut chars = pattern.chars().peekable();
    if pattern.starts_with('^') {
        chars.next();
    }
    while let Some(c) = chars.next() {
        if matches!(
            c,
            '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' | '$' | '^'
        ) {
            break;
        }
        // A quantifier applies to the character before it, so that character
        // is not a guaranteed literal.
        if matches!(chars.peek(), Some('*') | Some('?') | Some('{')) {
            break;
        }
        prefix.push(c);
    }
    prefix
}

pub fn search(reader: &GraphReader, request: &SearchRequest) -> Result<SearchResponse> {
    let label = match &request.label {
        Some(l) => Some(
            NodeLabel::from_str_label(l)
                .ok_or_else(|| LociError::InvalidArgument(format!("unknown label '{l}'")))?,
        ),
        None => None,
    };

    let file_regex = match &request.file_pattern {
        Some(p) => Some(
            regex::Regex::new(p)
                .map_err(|e| LociError::InvalidArgument(format!("bad file_pattern: {e}")))?,
        ),
        None => None,
    };

    let name_regex = match &request.name_pattern {
        Some(p) => Some(
            regex::Regex::new(p)
                .map_err(|e| LociError::InvalidArgument(format!("bad name_pattern: {e}")))?,
        ),
        None => None,
    };

    // Candidate selection, cheapest strategy first.
    let mut candidates = if let Some(qn) = &request.qualified_name {
        reader.nodes_by_qualified_name(qn)?
    } else if let Some(name) = &request.name {
        reader.nodes_by_name(name)?
    } else if let Some(pattern) = &request.name_pattern {
        let prefix = literal_prefix(pattern);
        if prefix.is_empty() {
            reader.all_nodes()?
        } else {
            reader.nodes_by_name_prefix(&prefix, PREFIX_SCAN_CAP)?
        }
    } else {
        reader.all_nodes()?
    };

    candidates.retain(|node| {
        if let Some(l) = label {
            if node.label != l {
                return false;
            }
        }
        if let Some(re) = &name_regex {
            if !re.is_match(&node.name) {
                return false;
            }
        }
        if let Some(re) = &file_regex {
            if !re.is_match(&node.file_path) {
                return false;
            }
        }
        true
    });

    // Deterministic order so cursors stay meaningful between calls.
    candidates.sort_by(|a, b| {
        a.qualified_name
            .cmp(&b.qualified_name)
            .then(a.file_path.cmp(&b.file_path))
            .then(a.start_line.cmp(&b.start_line))
    });

    let total = candidates.len();
    let offset = request.offset.unwrap_or(0);
    let limit = request.limit.unwrap_or(50).clamp(1, 1000);

    let page: Vec<Node> = candidates.into_iter().skip(offset).take(limit).collect();
    let returned = page.len();
    let has_more = offset + returned < total;

    let mut results = Vec::with_capacity(returned);
    for node in &page {
        results.push(to_hit(reader, node)?);
    }

    Ok(SearchResponse {
        results,
        total,
        has_more,
        cursor: has_more.then(|| (offset + returned).to_string()),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Inbound,
    Outbound,
    Both,
}

impl Direction {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "inbound" => Self::Inbound,
            "outbound" => Self::Outbound,
            "both" => Self::Both,
            other => {
                return Err(LociError::InvalidArgument(format!(
                    "direction must be inbound, outbound or both; got '{other}'"
                )))
            }
        })
    }

    /// True when the trace actually looked for callers, so a zero count is
    /// meaningful rather than simply not requested.
    pub fn includes_inbound(self) -> bool {
        matches!(self, Self::Inbound | Self::Both)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceRow {
    pub qualified_name: String,
    pub name: String,
    pub label: &'static str,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub hop: u32,
    pub edge_type: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnresolvedRow {
    pub from_qualified_name: String,
    pub callee_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceResponse {
    pub root: String,
    pub callers: Vec<TraceRow>,
    pub callees: Vec<TraceRow>,
    pub callers_total: usize,
    pub callees_total: usize,
    /// Calls whose target could not be resolved to a node. Reported so the
    /// absence of a CALLS edge is never mistaken for the absence of a call.
    pub unresolved: Vec<UnresolvedRow>,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Breadth-first traversal along the given edge types, deduplicated by node.
fn traverse(
    reader: &GraphReader,
    start: u64,
    outbound: bool,
    depth: u32,
    edge_types: &[EdgeType],
) -> Result<Vec<TraceRow>> {
    let mut seen: HashSet<u64> = HashSet::from([start]);
    let mut queue: VecDeque<(u64, u32)> = VecDeque::from([(start, 0)]);
    let mut rows = Vec::new();

    while let Some((node_id, hop)) = queue.pop_front() {
        if hop >= depth {
            continue;
        }
        for (edge_type, other, _) in reader.neighbours(node_id, outbound, Some(edge_types))? {
            if other == UNRESOLVED_NODE_ID || !seen.insert(other) {
                continue;
            }
            if let Some(node) = reader.node(other)? {
                rows.push(TraceRow {
                    qualified_name: node.qualified_name.clone(),
                    name: node.name.clone(),
                    label: node.label.as_str(),
                    file_path: node.file_path.clone(),
                    start_line: node.start_line,
                    end_line: node.end_line,
                    hop: hop + 1,
                    edge_type: edge_type.as_str(),
                });
                queue.push_back((other, hop + 1));
            }
        }
    }

    rows.sort_by(|a, b| {
        a.hop
            .cmp(&b.hop)
            .then(a.qualified_name.cmp(&b.qualified_name))
    });
    Ok(rows)
}

pub fn trace(
    reader: &GraphReader,
    node: &Node,
    direction: Direction,
    depth: u32,
    limit: usize,
    offset: usize,
) -> Result<TraceResponse> {
    let edge_types = [EdgeType::Calls];
    let depth = depth.clamp(1, 10);

    let callers = if matches!(direction, Direction::Inbound | Direction::Both) {
        traverse(reader, node.id, false, depth, &edge_types)?
    } else {
        Vec::new()
    };
    let callees = if matches!(direction, Direction::Outbound | Direction::Both) {
        traverse(reader, node.id, true, depth, &edge_types)?
    } else {
        Vec::new()
    };

    let mut unresolved = Vec::new();
    if matches!(direction, Direction::Outbound | Direction::Both) {
        for (edge_type, other, edge_id) in
            reader.neighbours(node.id, true, Some(&[EdgeType::CallUnresolved]))?
        {
            if edge_type == EdgeType::CallUnresolved && other == UNRESOLVED_NODE_ID {
                if let Some(edge) = reader.edge(edge_id)? {
                    unresolved.push(UnresolvedRow {
                        from_qualified_name: node.qualified_name.clone(),
                        callee_name: edge.target_name.unwrap_or_default(),
                        line: edge.line,
                        reason: edge.detail.unwrap_or_else(|| "unresolved".to_string()),
                    });
                }
            }
        }
    }
    unresolved.sort_by(|a, b| a.callee_name.cmp(&b.callee_name).then(a.line.cmp(&b.line)));

    let callers_total = callers.len();
    let callees_total = callees.len();

    let callers_page: Vec<TraceRow> = callers.into_iter().skip(offset).take(limit).collect();
    let callees_page: Vec<TraceRow> = callees.into_iter().skip(offset).take(limit).collect();
    let has_more =
        offset + callers_page.len() < callers_total || offset + callees_page.len() < callees_total;

    Ok(TraceResponse {
        root: node.qualified_name.clone(),
        callers: callers_page,
        callees: callees_page,
        callers_total,
        callees_total,
        unresolved,
        has_more,
        cursor: has_more.then(|| (offset + limit).to_string()),
    })
}

/// One hop of a `query_graph` pattern.
#[derive(Debug, Clone, Deserialize)]
pub struct Hop {
    /// Edge type name, e.g. "CALLS".
    pub edge: String,
    /// "out" (default) or "in".
    #[serde(default)]
    pub direction: Option<String>,
    /// Optional label filter applied to the node reached by this hop.
    #[serde(default)]
    pub label: Option<String>,
}

/// A multi-hop pattern query. Deliberately not Cypher: this executes exactly
/// what it describes against the adjacency indexes.
#[derive(Debug, Clone, Deserialize)]
pub struct PatternQuery {
    /// Selects the starting nodes.
    pub start: SearchRequest,
    #[serde(default)]
    pub hops: Vec<Hop>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternRow {
    /// One entry per pattern position: the start node, then one per hop.
    pub path: Vec<PatternNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternNode {
    pub qualified_name: String,
    pub label: &'static str,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_edge: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternResponse {
    pub rows: Vec<PatternRow>,
    pub total: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

fn pattern_node(node: &Node, via: Option<&'static str>) -> PatternNode {
    PatternNode {
        qualified_name: node.qualified_name.clone(),
        label: node.label.as_str(),
        file_path: node.file_path.clone(),
        start_line: node.start_line,
        end_line: node.end_line,
        via_edge: via,
    }
}

pub fn run_pattern(reader: &GraphReader, query: &PatternQuery) -> Result<PatternResponse> {
    // Start nodes come from the same search machinery as search_graph.
    let mut start_request = query.start.clone();
    start_request.limit = Some(1000);
    start_request.offset = Some(0);
    let starts = search(reader, &start_request)?;

    let mut paths: Vec<Vec<PatternNode>> = Vec::new();
    for hit in &starts.results {
        for node in reader.nodes_by_qualified_name(&hit.qualified_name)? {
            paths.push(vec![pattern_node(&node, None)]);
        }
    }

    // Track node ids alongside the rendered path so we can keep walking.
    let mut frontier: Vec<(u64, Vec<PatternNode>)> = Vec::new();
    for hit in &starts.results {
        for node in reader.nodes_by_qualified_name(&hit.qualified_name)? {
            frontier.push((node.id, vec![pattern_node(&node, None)]));
        }
    }

    for hop in &query.hops {
        let edge_type = EdgeType::from_str_type(&hop.edge).ok_or_else(|| {
            LociError::InvalidArgument(format!("unknown edge type '{}'", hop.edge))
        })?;
        let outbound = match hop.direction.as_deref() {
            None | Some("out") => true,
            Some("in") => false,
            Some(other) => {
                return Err(LociError::InvalidArgument(format!(
                    "hop direction must be 'in' or 'out'; got '{other}'"
                )))
            }
        };
        let label = match &hop.label {
            Some(l) => Some(
                NodeLabel::from_str_label(l)
                    .ok_or_else(|| LociError::InvalidArgument(format!("unknown label '{l}'")))?,
            ),
            None => None,
        };

        let mut next: Vec<(u64, Vec<PatternNode>)> = Vec::new();
        for (node_id, path) in &frontier {
            for (found_type, other, _) in
                reader.neighbours(*node_id, outbound, Some(&[edge_type]))?
            {
                if other == UNRESOLVED_NODE_ID {
                    continue;
                }
                let Some(node) = reader.node(other)? else {
                    continue;
                };
                if let Some(l) = label {
                    if node.label != l {
                        continue;
                    }
                }
                let mut extended = path.clone();
                extended.push(pattern_node(&node, Some(found_type.as_str())));
                next.push((other, extended));
            }
        }
        frontier = next;
    }

    paths = frontier.into_iter().map(|(_, path)| path).collect();
    paths.sort_by(|a, b| {
        let a_key: Vec<&str> = a.iter().map(|n| n.qualified_name.as_str()).collect();
        let b_key: Vec<&str> = b.iter().map(|n| n.qualified_name.as_str()).collect();
        a_key.cmp(&b_key)
    });
    paths.dedup_by(|a, b| {
        a.iter()
            .map(|n| &n.qualified_name)
            .eq(b.iter().map(|n| &n.qualified_name))
    });

    let total = paths.len();
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(50).clamp(1, 1000);
    let page: Vec<PatternRow> = paths
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|path| PatternRow { path })
        .collect();
    let has_more = offset + page.len() < total;

    Ok(PatternResponse {
        rows: page,
        total,
        has_more,
        cursor: has_more.then(|| (offset + limit).to_string()),
    })
}

/// Resolve a user-supplied symbol reference to exactly one node.
///
/// Prefers an exact qualified-name hit; falls back to simple name. Ambiguity is
/// an explicit error carrying the candidates, never a silent pick.
pub fn resolve_symbol(reader: &GraphReader, reference: &str) -> Result<Node> {
    let exact = reader.nodes_by_qualified_name(reference)?;
    if exact.len() == 1 {
        return Ok(exact.into_iter().next().expect("length checked"));
    }
    if exact.len() > 1 {
        return Err(LociError::AmbiguousSymbol {
            name: reference.to_string(),
            count: exact.len(),
        });
    }

    let by_name = reader.nodes_by_name(reference)?;
    match by_name.len() {
        0 => Err(LociError::SymbolNotFound(reference.to_string())),
        1 => Ok(by_name.into_iter().next().expect("length checked")),
        n => Err(LociError::AmbiguousSymbol {
            name: reference.to_string(),
            count: n,
        }),
    }
}

/// Candidates to show alongside an ambiguity error so the agent can retry with
/// a qualified name instead of guessing.
pub fn symbol_candidates(reader: &GraphReader, reference: &str) -> Result<Vec<SearchHit>> {
    let mut nodes = reader.nodes_by_qualified_name(reference)?;
    if nodes.is_empty() {
        nodes = reader.nodes_by_name(reference)?;
    }
    nodes.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    nodes.iter().map(|n| to_hit(reader, n)).collect()
}

/// Edges touching a node, materialised for tools that report evidence.
pub fn incident_edges(reader: &GraphReader, node_id: u64) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    for (_, _, edge_id) in reader.neighbours(node_id, true, None)? {
        if let Some(edge) = reader.edge(edge_id)? {
            edges.push(edge);
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_prefix_stops_at_metacharacters() {
        assert_eq!(literal_prefix("handle.*"), "handle");
        assert_eq!(literal_prefix("^get_user$"), "get_user");
        assert_eq!(literal_prefix(".*thing"), "");
        assert_eq!(literal_prefix("ab?c"), "a");
    }

    #[test]
    fn direction_parsing_rejects_unknown_values() {
        assert_eq!(Direction::parse("inbound").unwrap(), Direction::Inbound);
        assert_eq!(
            Direction::parse("sideways").unwrap_err().code(),
            "invalid_argument"
        );
    }
}
