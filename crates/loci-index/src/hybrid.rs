//! Hybrid LSP resolution pass.
//!
//! The AST pass runs first and resolves what it can. This pass takes only the
//! calls it gave up on and asks a real language server where they point. That
//! ordering matters for two reasons: a language server is orders of magnitude
//! slower than the AST resolver, and the AST answer is already correct for the
//! common case. Only the residue is worth the cost.
//!
//! Every upgrade is recorded as `Evidence::Lsp`, so a consumer can always tell
//! which edges a type checker vouched for and which came from syntax alone.

use loci_core::{LanguageId, Result, Sandbox};
use loci_graph::{Edge, EdgeType, Evidence, GraphStore, Node, NodeLabel, StoredCall};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

/// Limits that keep a slow or confused server from stalling an index run.
#[derive(Debug, Clone)]
pub struct HybridBudget {
    /// Wall clock for one request to one server.
    pub request_timeout: Duration,
    /// Wall clock for the entire pass, across every language.
    pub total_budget: Duration,
    /// Most unresolved calls to ask about per language.
    pub max_calls_per_language: usize,
}

impl Default for HybridBudget {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            total_budget: Duration::from_secs(120),
            max_calls_per_language: 2_000,
        }
    }
}

/// What the pass actually did, per language. Reported verbatim so nobody has to
/// guess whether LSP contributed anything.
#[derive(Debug, Clone, Serialize, Default)]
pub struct HybridLanguageReport {
    pub language: String,
    pub server: String,
    pub attempted: usize,
    pub resolved: usize,
    /// The server started but had no answer for these.
    pub unanswered: usize,
    /// The server answered, but the target is outside the indexed graph, for
    /// example a definition in a third-party dependency.
    pub outside_graph: usize,
    pub duration_ms: u64,
    /// Set when the language was skipped; the pass never fails the index.
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HybridReport {
    pub enabled: bool,
    pub languages: Vec<HybridLanguageReport>,
    pub total_resolved: usize,
    /// True when the pass stopped early because `total_budget` ran out.
    pub budget_exhausted: bool,
}

impl HybridReport {
    pub fn disabled() -> Self {
        Self::default()
    }
}

/// Index of graph nodes by file, so an LSP location can be turned back into the
/// symbol that contains it.
struct DefinitionIndex {
    /// file path -> callable nodes, each with its 1-based inclusive line span.
    by_file: HashMap<String, Vec<(u64, u32, u32)>>,
}

impl DefinitionIndex {
    fn build(nodes: &[Node]) -> Self {
        let mut by_file: HashMap<String, Vec<(u64, u32, u32)>> = HashMap::new();
        for node in nodes {
            if !node.label.is_callable() && node.label != NodeLabel::Class {
                continue;
            }
            by_file.entry(node.file_path.clone()).or_default().push((
                node.id,
                node.start_line,
                node.end_line,
            ));
        }
        Self { by_file }
    }

    /// Find the symbol a definition line lands in.
    ///
    /// A language server points at the declaration line, which may sit inside
    /// an enclosing class as well as the method itself, so the narrowest span
    /// wins. Returns `None` when the location is in no indexed symbol.
    fn symbol_at(&self, relative_path: &str, line_zero_based: u32) -> Option<u64> {
        let line = line_zero_based + 1;
        self.by_file
            .get(relative_path)?
            .iter()
            .filter(|(_, start, end)| *start <= line && line <= *end)
            .min_by_key(|(_, start, end)| end.saturating_sub(*start))
            .map(|(id, _, _)| *id)
    }
}

/// One call the AST could not resolve, with everything needed to ask a server.
struct Pending {
    src_node: u64,
    call: StoredCall,
}

/// Run the pass and write any upgraded edges.
///
/// Never returns an error for a server problem: a missing, slow, or broken
/// server is recorded in the report and the AST result stands.
pub fn resolve(
    store: &GraphStore,
    root: &Path,
    unresolved: &[(u64, StoredCall)],
    budget: &HybridBudget,
    next_edge_id: &mut u32,
) -> Result<HybridReport> {
    let mut report = HybridReport {
        enabled: true,
        ..Default::default()
    };

    if unresolved.is_empty() {
        return Ok(report);
    }

    let nodes = store.read()?.all_nodes()?;
    let index = DefinitionIndex::build(&nodes);
    let sandbox = Sandbox::new(root)?;

    // Group by language so each server is started once.
    let mut by_language: BTreeMap<LanguageId, Vec<Pending>> = BTreeMap::new();
    for (src_node, call) in unresolved {
        let Some(language) = LanguageId::from_path(Path::new(&call.file_path)) else {
            continue;
        };
        by_language.entry(language).or_default().push(Pending {
            src_node: *src_node,
            call: call.clone(),
        });
    }

    let started = Instant::now();
    let mut upgrades: Vec<Edge> = Vec::new();

    for (language, pending) in by_language {
        if started.elapsed() >= budget.total_budget {
            report.budget_exhausted = true;
            break;
        }

        let mut language_report = HybridLanguageReport {
            language: language.as_str().to_string(),
            server: loci_lsp::spec_for(language)
                .map(|s| s.executable.to_string())
                .unwrap_or_default(),
            ..Default::default()
        };
        let language_started = Instant::now();

        let (mut client, spec) = match loci_lsp::start(language, root, budget.request_timeout) {
            Ok(started) => started,
            Err(error) => {
                language_report.skipped_reason = Some(error.to_string());
                report.languages.push(language_report);
                continue;
            }
        };

        for item in pending.iter().take(budget.max_calls_per_language) {
            if started.elapsed() >= budget.total_budget {
                report.budget_exhausted = true;
                break;
            }

            // The server needs the file contents before it can answer.
            let Ok(source) = sandbox.read_to_string(&item.call.file_path) else {
                continue;
            };
            if client
                .did_open(&item.call.file_path, spec.lsp_language_id, &source)
                .is_err()
            {
                break;
            }

            language_report.attempted += 1;

            // Our lines are 1-based; LSP's are 0-based.
            let locations = match client.definition(
                &item.call.file_path,
                item.call.line.saturating_sub(1),
                item.call.character,
            ) {
                Ok(locations) => locations,
                Err(_) => {
                    language_report.unanswered += 1;
                    continue;
                }
            };

            let Some(location) = locations.first() else {
                language_report.unanswered += 1;
                continue;
            };

            let Some(dst) = index.symbol_at(&location.relative_path, location.line) else {
                // A definition in a dependency is a real answer, just not one
                // the graph can point at.
                language_report.outside_graph += 1;
                continue;
            };

            if dst == item.src_node {
                continue;
            }

            let mut edge = Edge::new(item.src_node, dst, EdgeType::Calls);
            edge.source = Evidence::Lsp;
            edge.detail = Some(format!("lsp_definition:{}", spec.executable));
            edge.line = Some(item.call.line);
            edge.target_name = Some(item.call.callee_name.clone());
            upgrades.push(edge);
            language_report.resolved += 1;
        }

        client.shutdown();
        language_report.duration_ms = language_started.elapsed().as_millis() as u64;
        report.total_resolved += language_report.resolved;
        report.languages.push(language_report);
    }

    if !upgrades.is_empty() {
        let writer = store.write()?;
        for mut edge in upgrades {
            edge.id = *next_edge_id;
            *next_edge_id += 1;
            writer.put_edge(&edge)?;
        }
        writer.commit()?;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u64, label: NodeLabel, file: &str, start: u32, end: u32) -> Node {
        let mut node = Node::new(label, "x", "a.x", file, start, end, None);
        node.id = id;
        node
    }

    #[test]
    fn a_definition_line_maps_to_the_narrowest_enclosing_symbol() {
        let nodes = vec![
            node(1, NodeLabel::Class, "store.go", 1, 40),
            node(2, NodeLabel::Method, "store.go", 10, 20),
            node(3, NodeLabel::Method, "store.go", 25, 30),
        ];
        let index = DefinitionIndex::build(&nodes);

        // Line 15 (1-based) is inside both the class and the first method; the
        // method is the answer because a server points at the declaration.
        assert_eq!(index.symbol_at("store.go", 14), Some(2));
        assert_eq!(index.symbol_at("store.go", 27), Some(3));
        // Inside the class but in neither method.
        assert_eq!(index.symbol_at("store.go", 4), Some(1));
    }

    #[test]
    fn a_location_outside_the_graph_resolves_to_nothing() {
        let index = DefinitionIndex::build(&[node(1, NodeLabel::Method, "a.go", 1, 5)]);
        assert_eq!(index.symbol_at("vendor/other.go", 3), None);
        assert_eq!(index.symbol_at("a.go", 99), None);
    }

    #[test]
    fn the_default_budget_is_bounded_on_every_axis() {
        let budget = HybridBudget::default();
        assert!(budget.request_timeout <= Duration::from_secs(30));
        assert!(budget.total_budget <= Duration::from_secs(300));
        assert!(budget.max_calls_per_language > 0);
    }
}
