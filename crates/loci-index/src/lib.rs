//! Repository walking, incremental indexing and graph construction.
//!
//! An index run is two phases. First every changed file is parsed in parallel
//! and its symbols are written. Then, with the whole project's symbols known,
//! cross-file edges (calls, imports, inheritance, routes) are rebuilt from the
//! per-file facts stored on disk. The second phase runs even for an incremental
//! update, so an edge from an untouched file into a symbol that just moved is
//! still correct.

pub mod changes;
pub mod hybrid;
pub mod imports;
pub mod resolve;
pub mod walk;

use loci_core::{content_hash, LanguageId, LociError, Result, Sandbox, SCHEMA_VERSION};
use loci_graph::{
    catalog::{Catalog, ProjectEntry},
    coverage::COVERAGE_NOTE,
    CoverageReason, CoverageStatus, Edge, EdgeType, Evidence, FileFacts, FileRecord, GraphStore,
    Node, NodeLabel, ProjectMeta, StoredCall, StoredImport, StoredReceiverBinding, StoredRouteLink,
    StoredTypeRel, STRUCTURAL_EDGE_ID_BASE,
};
use rayon::prelude::*;
use resolve::SymbolTable;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;
use walk::Candidate;

/// How many example paths to embed in a report. The full lists live in the
/// graph and are queryable through check_index_coverage.
const EXAMPLE_CAP: usize = 10;

/// Structural edge ids start at `0x8000_0000`. Once incremental runs have
/// consumed most of the range below it, the next run rebuilds from scratch and
/// resets the counter rather than colliding.
const EDGE_ID_REBUILD_THRESHOLD: u32 = 0x7000_0000;

/// Same idea for the structural half, which runs from `0x8000_0000` upwards.
const STRUCTURAL_EDGE_ID_REBUILD_THRESHOLD: u32 = 0xF000_0000;

#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// Display name; defaults to the root's basename.
    pub name: Option<String>,
    /// Re-parse every file even when its hash is unchanged.
    pub full: bool,
    /// Consult language servers for calls the AST could not resolve.
    ///
    /// Off by default: it adds seconds to a run that otherwise takes
    /// milliseconds, and it is only worth paying when the extra precision is
    /// wanted. Nothing about the AST result changes when it is off.
    pub hybrid_lsp: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PhaseTimings {
    /// Enumerating candidate files.
    pub walk: u64,
    /// Hashing files to decide what changed.
    pub hash: u64,
    /// Parsing changed files and writing their symbols.
    pub parse: u64,
    /// Loading every node and file fact for cross-file resolution.
    pub load_facts: u64,
    /// Resolving and writing cross-file edges.
    pub resolve_edges: u64,
    /// Consulting language servers.
    pub hybrid_lsp: u64,
    /// Summarising coverage for the report.
    pub report: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexReport {
    pub project: String,
    pub root: String,
    pub store_path: String,
    pub duration_ms: u64,
    pub nodes: u64,
    pub edges: u64,
    /// Totals below describe the graph as it now stands, not this run.
    pub files_indexed: usize,
    pub files_parse_partial: usize,
    pub files_skipped: usize,
    /// Counts below describe only what this run did.
    pub files_reparsed: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub languages: BTreeMap<String, u64>,
    pub parse_partial_examples: Vec<String>,
    pub skipped_examples: Vec<CoverageExample>,
    pub bundled_languages: Vec<String>,
    /// What the Hybrid LSP pass did, or that it was off.
    pub hybrid_lsp: hybrid::HybridReport,
    /// Set when a requested project name could not be honoured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_note: Option<String>,
    /// Wall clock per phase. Without this a slow run is a mystery, and at scale
    /// the phases have very different costs.
    pub phase_ms: PhaseTimings,
    pub coverage_note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageExample {
    pub path: String,
    pub reason: &'static str,
    /// Further files skipped the same way in the same directory, folded into
    /// this one so a single noisy folder cannot fill the whole sample.
    #[serde(skip_serializing_if = "is_zero")]
    pub others_like_it: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Outcome of parsing one file, produced in parallel and applied serially.
struct FileOutcome {
    relative_path: String,
    hash: String,
    size: u64,
    language: Option<LanguageId>,
    status: CoverageStatus,
    reason: Option<CoverageReason>,
    detail: Option<String>,
    line_count: u32,
    extracted: Option<loci_parse::ExtractedFile>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn format_error_ranges(ranges: &[(u32, u32)]) -> String {
    let shown: Vec<String> = ranges
        .iter()
        .take(20)
        .map(|(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}-{end}")
            }
        })
        .collect();
    format!("parse errors at lines {}", shown.join(", "))
}

/// Read and parse one candidate. Never panics on bad input; failures become
/// coverage records.
/// Extract with the mapped grammar, and for ambiguous extensions fall back to
/// the alternative when the first choice does not fit.
///
/// `.h` is the case that matters: the extension says nothing about whether the
/// file is C or C++. Mapping it to C alone shreds every C++ header — a Qt
/// project here reported 59 of 245 files as partial, and one 19-line header
/// produced 10 parse errors under C and none under C++. Choosing by measured
/// error count rather than by extension keeps plain C headers on the C grammar,
/// where a few C constructs are not valid C++.
fn extract_best_effort(
    language: LanguageId,
    path: &str,
    source: &str,
) -> loci_core::Result<(LanguageId, loci_parse::ExtractedFile)> {
    let mut best = (language, loci_parse::extract(language, path, source)?);
    if best.1.error_ranges.is_empty() {
        return Ok(best);
    }

    let alternative = ambiguous_alternative(language, path);
    if let Some(alternative) = alternative {
        best = better_of(best, alternative, path, source);
    }

    // Try the rewrite whenever C++ is possible at all, not only when raw C++
    // already won. A heavily Qt header parses worse as raw C++ than as C, so
    // gating on the winner above would skip the very files that need this.
    let mut prepared = None;
    if !best.1.error_ranges.is_empty()
        && (language == LanguageId::Cpp || alternative == Some(LanguageId::Cpp))
    {
        prepared = loci_parse::prepare_cpp(source);
        if let Some(prepared) = &prepared {
            best = better_of(best, LanguageId::Cpp, path, prepared);
        }
    }

    // A preprocessor conditional chosen inside a single declaration defeats
    // both attempts above, since neither branch is a construct on its own.
    // Flattening builds on the Qt rewrite where there was one, because a file
    // can need both.
    let settled = best.0;
    if !best.1.error_ranges.is_empty() && matches!(settled, LanguageId::C | LanguageId::Cpp) {
        let base = prepared.as_deref().unwrap_or(source);
        if let Some(flattened) = loci_parse::flatten_conditionals(base) {
            best = better_of(best, settled, path, &flattened);
        }
    }

    // TypeScript has three independent grammar holes. A file can need more
    // than one, so each pass builds on whatever the last one produced.
    let settled = best.0;
    if !best.1.error_ranges.is_empty() && is_typescript_family(settled) {
        let mut prepared: Option<String> = None;
        if let Some(markup) = loci_parse::neutralise_jsx_ampersands(settled, source) {
            let before = best.1.error_ranges.len();
            best = better_of(best, settled, path, &markup);
            if best.1.error_ranges.len() < before {
                prepared = Some(markup);
            }
        }
        if !best.1.error_ranges.is_empty() {
            let base = prepared.as_deref().unwrap_or(source);
            if let Some(separated) = loci_parse::separate_keyword_members(settled, base) {
                let before = best.1.error_ranges.len();
                best = better_of(best, settled, path, &separated);
                if best.1.error_ranges.len() < before {
                    prepared = Some(separated);
                }
            }
        }
        if !best.1.error_ranges.is_empty() {
            let base = prepared.as_deref().unwrap_or(source);
            if let Some(imports) = loci_parse::neutralise_import_type_arrays(settled, base) {
                best = better_of(best, settled, path, &imports);
            }
        }
    }

    if !best.1.error_ranges.is_empty() && settled == LanguageId::Make {
        if let Some(repaired) = loci_parse::repair_make_keyword_targets(source) {
            best = better_of(best, LanguageId::Make, path, &repaired);
            loci_parse::restore_make_target_names(source, &mut best.1);
        }
    }

    Ok(best)
}

/// Languages that share the tree-sitter-typescript lexer, and so its
/// keyword-over-identifier and import-type-array faults. JSX lives in the
/// JavaScript grammars too.
fn is_typescript_family(language: LanguageId) -> bool {
    matches!(
        language,
        LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript | LanguageId::Jsx
    )
}

/// Keep `candidate` only if it parses `text` with fewer errors than `best`.
fn better_of(
    best: (LanguageId, loci_parse::ExtractedFile),
    candidate: LanguageId,
    path: &str,
    text: &str,
) -> (LanguageId, loci_parse::ExtractedFile) {
    match loci_parse::extract(candidate, path, text) {
        Ok(other) if other.error_ranges.len() < best.1.error_ranges.len() => (candidate, other),
        _ => best,
    }
}

/// The other language a path's extension could plausibly mean.
fn ambiguous_alternative(language: LanguageId, path: &str) -> Option<LanguageId> {
    let extension = path.rsplit('.').next()?;
    match (language, extension) {
        (LanguageId::C, "h") => Some(LanguageId::Cpp),
        _ => None,
    }
}

fn process_file(candidate: &Candidate) -> FileOutcome {
    let mut outcome = FileOutcome {
        relative_path: candidate.relative_path.clone(),
        hash: String::new(),
        size: candidate.size,
        language: candidate.language,
        status: CoverageStatus::Skipped,
        reason: None,
        detail: None,
        line_count: 0,
        extracted: None,
    };

    if walk::is_oversized(candidate.size) {
        outcome.reason = Some(CoverageReason::Oversized);
        outcome.detail = Some(format!(
            "{} bytes exceeds the {} byte parse limit",
            candidate.size,
            loci_core::MAX_FILE_BYTES
        ));
        return outcome;
    }

    let bytes = match std::fs::read(&candidate.absolute_path) {
        Ok(b) => b,
        Err(e) => {
            outcome.reason = Some(CoverageReason::ReadError);
            outcome.detail = Some(e.to_string());
            return outcome;
        }
    };
    outcome.hash = content_hash(&bytes);

    if walk::looks_binary(&bytes) {
        outcome.reason = Some(CoverageReason::Binary);
        return outcome;
    }

    let Some(language) = candidate.language else {
        outcome.reason = Some(CoverageReason::UnsupportedLanguage);
        outcome.detail = Some("no language mapping for this file extension".to_string());
        return outcome;
    };

    if !loci_parse::is_bundled(language) {
        outcome.reason = Some(CoverageReason::UnsupportedLanguage);
        outcome.detail = Some(format!("no {language} grammar is bundled in this build"));
        return outcome;
    }

    let source = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            outcome.reason = Some(CoverageReason::ReadError);
            outcome.detail = Some("file is not valid UTF-8".to_string());
            return outcome;
        }
    };
    outcome.line_count = source.lines().count().max(1) as u32;

    match extract_best_effort(language, &candidate.relative_path, &source) {
        Ok((language, extracted)) => {
            outcome.language = Some(language);
            if extracted.error_ranges.is_empty() {
                outcome.status = CoverageStatus::Indexed;
            } else {
                outcome.status = CoverageStatus::ParsePartial;
                outcome.reason = Some(CoverageReason::ParseError);
                outcome.detail = Some(format_error_ranges(&extracted.error_ranges));
            }
            outcome.extracted = Some(extracted);
        }
        Err(e) => {
            outcome.reason = Some(CoverageReason::ParseError);
            outcome.detail = Some(e.to_string());
        }
    }

    outcome
}

/// Index a repository into its own persistent graph.
pub fn index_repository(root: &Path, options: &IndexOptions) -> Result<IndexReport> {
    let started = Instant::now();
    let sandbox = Sandbox::new(root)?;
    let root_display = sandbox.root().to_string_lossy().to_string();

    let mut catalog = Catalog::load()?;
    let name = options
        .name
        .clone()
        .unwrap_or_else(|| loci_core::paths::default_project_name(sandbox.root()));
    let project_id = catalog.allocate_id(&name, sandbox.root());
    let store_path = loci_core::paths::graph_db_path(&project_id)?;

    // One root keeps one graph, so an explicit name for an already-indexed root
    // is not applied. Silently returning a different id than the caller asked
    // for is how an agent ends up querying the wrong project.
    let name_note = options
        .name
        .as_deref()
        .filter(|requested| loci_core::paths::sanitize_project_id(requested) != project_id)
        .map(|requested| {
            format!(
                "the requested name '{requested}' was not applied: this root is already indexed \
                 as '{project_id}', and one repository root maps to one graph. Delete \
                 '{project_id}' first if you really want a different id."
            )
        });

    let store = if store_path.exists() {
        GraphStore::open(&store_path)?
    } else {
        GraphStore::create(&store_path)?
    };

    // A store written by an older layout cannot be trusted; rebuild it.
    let existing_meta = store.read()?.meta()?;
    let schema_mismatch = existing_meta
        .as_ref()
        .is_some_and(|m| m.schema_version != SCHEMA_VERSION);
    // Incremental runs never reuse edge ids, so the resolved range drains over
    // a project's lifetime. Rebuilding compacts it back to zero; letting it run
    // into the structural range would silently overwrite structural edges.
    let edge_ids_exhausted = existing_meta.as_ref().is_some_and(|m| {
        m.next_edge_id >= EDGE_ID_REBUILD_THRESHOLD
            || m.next_structural_edge_id >= STRUCTURAL_EDGE_ID_REBUILD_THRESHOLD
    });

    let full = options.full || schema_mismatch || existing_meta.is_none() || edge_ids_exhausted;

    let mut phase_ms = PhaseTimings::default();

    let phase_started = Instant::now();
    let candidates = walk::collect(&sandbox);
    phase_ms.walk = phase_started.elapsed().as_millis() as u64;

    let previous: HashMap<String, FileRecord> = store
        .read()?
        .all_files()?
        .into_iter()
        .map(|record| (record.path.clone(), record))
        .collect();

    // Split candidates into work and skips before touching the disk again.
    let phase_started = Instant::now();
    let mut to_process: Vec<&Candidate> = Vec::new();
    let mut unchanged: Vec<&Candidate> = Vec::new();
    for candidate in &candidates {
        match previous.get(&candidate.relative_path) {
            Some(record) if !full && record.size == candidate.size && !record.hash.is_empty() => {
                // Size match is a cheap gate; the hash check below is decisive.
                let current = std::fs::read(&candidate.absolute_path)
                    .map(|b| content_hash(&b))
                    .unwrap_or_default();
                if current == record.hash {
                    unchanged.push(candidate);
                } else {
                    to_process.push(candidate);
                }
            }
            _ => to_process.push(candidate),
        }
    }

    phase_ms.hash = phase_started.elapsed().as_millis() as u64;

    let phase_started = Instant::now();
    let outcomes: Vec<FileOutcome> = to_process.par_iter().map(|c| process_file(c)).collect();

    let seen: std::collections::HashSet<&str> = candidates
        .iter()
        .map(|c| c.relative_path.as_str())
        .collect();
    let removed: Vec<String> = previous
        .keys()
        .filter(|path| !seen.contains(path.as_str()))
        .cloned()
        .collect();

    let mut next_node_id = existing_meta.as_ref().map_or(1, |m| m.next_node_id).max(1);
    let mut next_edge_id = existing_meta.as_ref().map_or(1, |m| m.next_edge_id).max(1);
    let mut next_structural_edge_id = if full {
        // A full run rewrites every structural edge, so the counter restarts.
        STRUCTURAL_EDGE_ID_BASE
    } else {
        existing_meta
            .as_ref()
            .map_or(STRUCTURAL_EDGE_ID_BASE, |m| m.next_structural_edge_id)
            .max(STRUCTURAL_EDGE_ID_BASE)
    };

    // Names defined by files that changed, captured before their nodes are
    // deleted. Any other file whose facts mention one of these may resolve
    // differently now, so it has to be re-resolved too.
    let mut touched_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    if !full {
        let reader = store.read()?;
        for path in outcomes
            .iter()
            .map(|o| o.relative_path.as_str())
            .chain(removed.iter().map(String::as_str))
        {
            for node in reader.nodes_in_file(path)? {
                touched_names.insert(node.name);
                touched_names.insert(node.qualified_name);
            }
        }
    }

    // Phase one: replace the symbols of every file that changed.
    {
        let writer = store.write()?;

        if full {
            for path in previous.keys() {
                writer.remove_file(path)?;
            }
        } else {
            for outcome in &outcomes {
                writer.remove_file(&outcome.relative_path)?;
            }
            for path in &removed {
                writer.remove_file(path)?;
            }
        }

        for outcome in &outcomes {
            write_file_symbols(
                &writer,
                outcome,
                &mut next_node_id,
                &mut next_structural_edge_id,
            )?;
        }

        writer.commit()?;
    }
    phase_ms.parse = phase_started.elapsed().as_millis() as u64;

    // The new symbols matter as much as the old ones: a definition that just
    // appeared can settle a call that was unresolved elsewhere.
    if !full {
        for outcome in &outcomes {
            let Some(extracted) = &outcome.extracted else {
                continue;
            };
            for definition in &extracted.definitions {
                touched_names.insert(definition.name.clone());
                touched_names.insert(definition.qualified_name.clone());
            }
        }
    }

    // Phase two: rebuild the edges that depend on project-wide symbols. When
    // only part of the project changed, only that part is rebuilt.
    let scope = if full {
        EdgeScope::Everything
    } else {
        EdgeScope::Files {
            changed: outcomes
                .iter()
                .map(|o| o.relative_path.clone())
                .chain(removed.iter().cloned())
                .collect(),
            touched_names,
        }
    };
    // Manifests are read once per run. They change how imports are spelled
    // across the whole project, so a stale one would misroute every edge.
    let resolver = imports::ImportResolver::discover(sandbox.root());
    let resolved = rebuild_resolved_edges(
        &store,
        &mut next_node_id,
        &mut next_edge_id,
        &mut phase_ms,
        &scope,
        &resolver,
    )?;
    let (mut node_count, mut edge_count, languages) =
        (resolved.nodes, resolved.edges, resolved.languages);

    // Phase three, optional: ask language servers about what syntax could not
    // settle. A failure here leaves the AST result untouched.
    let phase_started = Instant::now();
    let hybrid = if options.hybrid_lsp {
        let report = hybrid::resolve(
            &store,
            sandbox.root(),
            &resolved.unresolved_calls,
            &hybrid::HybridBudget::default(),
            &mut next_edge_id,
        )?;
        if report.total_resolved > 0 {
            let reader = store.read()?;
            node_count = reader.node_count()?;
            edge_count = reader.edge_count()?;
        }
        report
    } else {
        hybrid::HybridReport::disabled()
    };
    phase_ms.hybrid_lsp = phase_started.elapsed().as_millis() as u64;

    // One scan, three buckets. Asking the store separately for each status
    // re-read and re-parsed every file record, which is measurable once a
    // project has tens of thousands of files.
    let phase_started = Instant::now();
    let mut files_indexed = 0usize;
    let mut parse_partial: Vec<FileRecord> = Vec::new();
    let mut skipped: Vec<FileRecord> = Vec::new();
    for record in store.read()?.all_files()? {
        match record.status {
            CoverageStatus::Indexed => files_indexed += 1,
            CoverageStatus::ParsePartial => parse_partial.push(record),
            CoverageStatus::Skipped => skipped.push(record),
            CoverageStatus::Excluded => {}
        }
    }
    phase_ms.report = phase_started.elapsed().as_millis() as u64;

    let duration_ms = started.elapsed().as_millis() as u64;

    {
        let writer = store.write()?;
        let mut meta = ProjectMeta::new(project_id.clone(), root_display.clone());
        meta.indexed_at_unix = now_unix();
        meta.duration_ms = duration_ms;
        meta.node_count = node_count;
        meta.edge_count = edge_count;
        meta.file_count = (files_indexed + parse_partial.len()) as u64;
        meta.languages = languages.clone();
        meta.bundled_languages = loci_parse::bundled_language_ids()
            .into_iter()
            .map(String::from)
            .collect();
        meta.next_node_id = next_node_id;
        meta.next_edge_id = next_edge_id;
        meta.next_structural_edge_id = next_structural_edge_id;
        writer.put_meta(&meta)?;
        writer.commit()?;
    }

    catalog.upsert(ProjectEntry {
        id: project_id.clone(),
        name,
        root: root_display.clone(),
        store_path: store_path.to_string_lossy().to_string(),
        indexed_at_unix: now_unix(),
    });
    catalog.save()?;

    Ok(IndexReport {
        project: project_id,
        root: root_display,
        store_path: store_path.to_string_lossy().to_string(),
        duration_ms,
        nodes: node_count,
        edges: edge_count,
        files_indexed,
        files_parse_partial: parse_partial.len(),
        files_skipped: skipped.len(),
        files_reparsed: outcomes.len(),
        files_unchanged: unchanged.len(),
        files_removed: removed.len(),
        languages,
        parse_partial_examples: parse_partial
            .iter()
            .take(EXAMPLE_CAP)
            .map(|f| match &f.detail {
                Some(detail) => format!("{} ({detail})", f.path),
                None => f.path.clone(),
            })
            .collect(),
        skipped_examples: loci_graph::coverage::sample_files(&skipped, EXAMPLE_CAP)
            .into_iter()
            .map(|s| CoverageExample {
                path: s.file.path.clone(),
                reason: s.file.reason.map_or("unknown", CoverageReason::as_str),
                others_like_it: s.others_like_it,
            })
            .collect(),
        bundled_languages: loci_parse::bundled_language_ids()
            .into_iter()
            .map(String::from)
            .collect(),
        hybrid_lsp: hybrid,
        name_note,
        phase_ms,
        coverage_note: COVERAGE_NOTE,
    })
}

/// Write the File node, its definitions, its routes and the containment edges.
fn write_file_symbols(
    writer: &loci_graph::GraphWriter,
    outcome: &FileOutcome,
    next_node_id: &mut u64,
    next_structural_edge_id: &mut u32,
) -> Result<()> {
    let mut node_ids = Vec::new();
    let mut structural_edges: Vec<Edge> = Vec::new();

    let file_name = outcome
        .relative_path
        .rsplit('/')
        .next()
        .unwrap_or(&outcome.relative_path)
        .to_string();
    let module_qn = loci_parse::module_prefix(&outcome.relative_path);
    let file_qn = if module_qn.is_empty() {
        outcome.relative_path.clone()
    } else {
        module_qn.clone()
    };

    let mut file_node = Node::new(
        NodeLabel::File,
        file_name,
        file_qn.clone(),
        outcome.relative_path.clone(),
        1,
        outcome.line_count.max(1),
        outcome.language,
    );
    file_node.id = *next_node_id;
    *next_node_id += 1;
    let file_node_id = file_node.id;
    writer.put_node(&file_node)?;
    node_ids.push(file_node_id);

    let mut record = FileRecord {
        path: outcome.relative_path.clone(),
        hash: outcome.hash.clone(),
        size: outcome.size,
        language: outcome.language,
        status: outcome.status,
        reason: outcome.reason,
        detail: outcome.detail.clone(),
        node_ids: Vec::new(),
    };

    let Some(extracted) = &outcome.extracted else {
        record.node_ids = node_ids;
        writer.put_file(&record)?;
        writer.put_file_facts(&outcome.relative_path, &FileFacts::default())?;
        return Ok(());
    };

    // Definition nodes, with ids assigned in source order.
    let mut definition_ids: Vec<(usize, u64)> = Vec::new();
    for (position, definition) in extracted.definitions.iter().enumerate() {
        let mut node = Node::new(
            definition.label,
            definition.name.clone(),
            definition.qualified_name.clone(),
            outcome.relative_path.clone(),
            definition.start_line,
            definition.end_line,
            outcome.language,
        );
        node.id = *next_node_id;
        *next_node_id += 1;
        node.signature = definition.signature.clone();
        if let Some(returns) = &definition.returns {
            node.extra.insert("returns".to_string(), returns.clone());
        }
        node.source = Evidence::Ast;
        writer.put_node(&node)?;
        node_ids.push(node.id);
        definition_ids.push((position, node.id));
    }

    // Containment: innermost enclosing definition, else the file.
    for (position, node_id) in &definition_ids {
        let definition = &extracted.definitions[*position];
        let parent = extracted
            .definitions
            .iter()
            .enumerate()
            .filter(|(other_position, other)| {
                *other_position != *position
                    && other.start_byte <= definition.start_byte
                    && definition.end_byte <= other.end_byte
            })
            .min_by_key(|(_, other)| other.end_byte - other.start_byte)
            .and_then(|(other_position, _)| {
                definition_ids
                    .iter()
                    .find(|(p, _)| *p == other_position)
                    .map(|(_, id)| *id)
            });

        let (src, edge_type) = match parent {
            Some(parent_id) => (parent_id, EdgeType::Contains),
            None => (file_node_id, EdgeType::Defines),
        };
        let mut edge = Edge::new(src, *node_id, edge_type);
        // Structural edges are allocated from a private range so they never
        // collide with the resolved edges rebuilt in phase two.
        edge.id = take_structural_edge_id(next_structural_edge_id);
        structural_edges.push(edge);
    }

    // Route nodes only exist where the AST showed a framework registration.
    let mut route_links = Vec::new();
    for route in &extracted.routes {
        let route_qn = format!("route:{} {}", route.method, route.path);
        let mut node = Node::new(
            NodeLabel::Route,
            route.path.clone(),
            route_qn.clone(),
            outcome.relative_path.clone(),
            route.line,
            route.end_line,
            outcome.language,
        );
        node.id = *next_node_id;
        *next_node_id += 1;
        node.extra
            .insert("method".to_string(), route.method.clone());
        node.extra.insert("path".to_string(), route.path.clone());
        node.extra
            .insert("framework".to_string(), route.framework_hint.clone());
        writer.put_node(&node)?;
        node_ids.push(node.id);

        let mut edge = Edge::new(file_node_id, node.id, EdgeType::Defines);
        edge.id = take_structural_edge_id(next_structural_edge_id);
        structural_edges.push(edge);

        if let Some(handler) = &route.handler_name {
            route_links.push(StoredRouteLink {
                route_qualified_name: route_qn,
                handler_name: handler.clone(),
                handler_receiver: route.handler_receiver.clone(),
                file_path: outcome.relative_path.clone(),
            });
        }
    }

    // Facts that need the whole project to resolve.
    let calls = extracted
        .calls
        .iter()
        .map(|call| StoredCall {
            from_qualified_name: resolve::owner_qualified_name(
                extracted.enclosing_definition(call.byte),
                &file_qn,
            ),
            callee_name: call.callee_name.clone(),
            receiver: call.receiver.clone(),
            line: call.line,
            character: call.character,
            file_path: outcome.relative_path.clone(),
        })
        .collect();

    let imports = extracted
        .imports
        .iter()
        .map(|import| StoredImport {
            from_file: outcome.relative_path.clone(),
            target: import.target.clone(),
            line: import.line,
        })
        .collect();

    let type_relations = extracted
        .type_relations
        .iter()
        .filter_map(|relation| {
            let subtype = extracted
                .definitions
                .iter()
                .find(|d| d.name == relation.subtype)?;
            Some(StoredTypeRel {
                subtype_qualified_name: subtype.qualified_name.clone(),
                supertype_name: relation.supertype.clone(),
                edge_type: match relation.kind {
                    loci_parse::TypeRelKind::Inherits => EdgeType::Inherits,
                    loci_parse::TypeRelKind::Implements => EdgeType::Implements,
                },
                line: relation.line,
            })
        })
        .collect();

    let receiver_bindings = extracted
        .receiver_bindings
        .iter()
        .map(|binding| StoredReceiverBinding {
            variable: binding.variable.clone(),
            constructor: binding.constructor.clone(),
            file_path: outcome.relative_path.clone(),
        })
        .collect();

    writer.put_file_facts(
        &outcome.relative_path,
        &FileFacts {
            calls,
            imports,
            type_relations,
            route_links,
            receiver_bindings,
        },
    )?;

    writer.put_edges(&structural_edges)?;
    record.node_ids = node_ids;
    writer.put_file(&record)?;
    Ok(())
}

/// Next id for a structural edge, taken from the high half of the id space.
///
/// This used to hash `(src, dst, type)` into 31 bits. At 600k structural edges
/// that space collides by the birthday bound, and each collision silently
/// overwrote a DEFINES or CONTAINS edge, so two indexes of the same tree could
/// disagree on the edge count. A counter cannot collide.
fn take_structural_edge_id(next: &mut u32) -> u32 {
    let id = *next;
    *next = next.saturating_add(1);
    id
}

/// Turn an import as written in source into the module name a File node uses.
///
/// Shared with the affected-file analysis on purpose: when these two disagreed,
/// an incremental run silently dropped import edges into re-parsed files.
fn normalise_import_target(target: &str) -> String {
    target
        .trim_start_matches("./")
        .trim_end_matches(".js")
        // Web extensions are written out in a `<script src>` and increasingly
        // in ESM imports too, and a path that keeps its suffix resolves to
        // nothing.
        .trim_end_matches(".mjs")
        .trim_end_matches(".cjs")
        .trim_end_matches(".jsx")
        .trim_end_matches(".tsx")
        .trim_end_matches(".ts")
        .trim_end_matches(".dart")
        .trim_end_matches(".sh")
        .trim_end_matches(".mk")
        .trim_end_matches(".cmake")
        .replace(['/', '\\'], ".")
}

/// Extensions an import may leave off, tried in this order.
///
/// A path spelled without a suffix is the norm in TypeScript and the bundlers
/// around it, so the extension has to be guessed back. Declaration files come
/// last: when both `x.ts` and `x.d.ts` exist the implementation is the file the
/// importer depends on.
const IMPLICIT_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "dart", "vue", "py", "go", "d.ts",
];

/// Bare names a directory import falls back to.
const DIRECTORY_ENTRIES: &[&str] = &["index", "mod", "main", "__init__"];

/// The spellings an import could use to name `path`, each with a rank.
///
/// This runs once per indexed file and answers the question from the file's
/// side, which is the cheap direction: expanding every import into the ~35
/// paths it might mean instead cost 2.8 seconds of edge building on a 4,500
/// file project, against 35 milliseconds before.
///
/// Both the edge builder and the incremental affected-file analysis derive
/// their matching from this one function. When those two disagreed about what
/// an import resolves to, an incremental run silently dropped edges that a full
/// run produced, and only a full reindex put them back.
///
/// Rank breaks ties when two files claim a spelling — `x.ts` and `x.tsx` both
/// answer to `x` — so the winner does not depend on iteration order.
fn path_spellings(path: &str) -> Vec<(u32, String)> {
    let mut out = vec![(0, path.to_string())];

    // Longest suffix first: `.d.ts` must not be read as `.ts`.
    let stem = if let Some(stem) = path.strip_suffix(".d.ts") {
        Some((IMPLICIT_EXTENSIONS.len() as u32, stem))
    } else {
        IMPLICIT_EXTENSIONS
            .iter()
            .enumerate()
            .find_map(|(rank, extension)| {
                path.strip_suffix(&format!(".{extension}"))
                    .map(|stem| (rank as u32 + 1, stem))
            })
    };

    if let Some((rank, stem)) = stem {
        out.push((rank, stem.to_string()));

        // A directory import loads the entry point inside it, so the directory
        // is another name for this file.
        let (parent, base) = match stem.rsplit_once('/') {
            Some((parent, base)) => (parent, base),
            None => ("", stem),
        };
        if let Some(entry) = DIRECTORY_ENTRIES.iter().position(|e| *e == base) {
            if !parent.is_empty() {
                out.push((100 + entry as u32 * 20 + rank, parent.to_string()));
            }
        }
    }

    out
}

/// Indexed files, keyed by every path an import could spell them with.
struct PathIndex {
    by_spelling: HashMap<String, (u32, u64)>,
    /// Package node per Go package directory. A Go import names a package, so
    /// it has no single file to point at.
    go_packages: HashMap<String, u64>,
}

impl PathIndex {
    fn build(nodes: &[Node], go_packages: HashMap<String, u64>) -> Self {
        let mut by_spelling: HashMap<String, (u32, u64)> = HashMap::new();

        for node in nodes.iter().filter(|n| n.label == NodeLabel::File) {
            for (rank, spelling) in path_spellings(&node.file_path) {
                by_spelling
                    .entry(spelling)
                    .and_modify(|held| {
                        if rank < held.0 {
                            *held = (rank, node.id);
                        }
                    })
                    .or_insert((rank, node.id));
            }
        }

        Self {
            by_spelling,
            go_packages,
        }
    }

    fn get(&self, spelling: &str) -> Option<u64> {
        self.by_spelling.get(spelling).map(|(_, node)| *node)
    }
}

/// The node an import points at, or `None` when it leaves the index.
///
/// For most languages that is a file. A Go import is the exception: it names a
/// package, so it points at the package node. Pointing it at every file of the
/// package instead was measured on a real repository and rejected — one package
/// of 141 files with 456 importers produced 64,296 edges on its own, claiming a
/// dependency on each file when the importer used one type.
///
/// Manifest-driven resolution runs first because it is the precise answer. The
/// dotted-module fallback stays for the languages whose imports genuinely name
/// a module rather than a path — Python, Rust, Java — where the old matching
/// was already right.
fn resolve_import(
    resolver: &imports::ImportResolver,
    from_file: &str,
    target: &str,
    paths: &PathIndex,
    by_module: &HashMap<String, u64>,
) -> Option<u64> {
    for candidate in resolver.candidates(from_file, target) {
        if let Some(id) = paths.get(&candidate) {
            return Some(id);
        }
        if let Some(&id) = paths.go_packages.get(&candidate) {
            return Some(id);
        }
    }
    by_module.get(&normalise_import_target(target)).copied()
}

/// Bring the Go package nodes in line with the Go files currently indexed.
///
/// These nodes are the only ones no file owns: a package is a directory, so
/// `remove_file` can never reach them. They are kept, rather than derived on
/// every run, because their ids have to stay valid for import edges written by
/// files that this run did not touch.
///
/// Returns the package node per directory, and whether a package was created —
/// which means files outside the current scope may now have somewhere to point.
fn sync_go_packages(
    writer: &loci_graph::GraphWriter,
    nodes: &[Node],
    resolver: &imports::ImportResolver,
    next_node_id: &mut u64,
) -> Result<(HashMap<String, u64>, bool)> {
    let mut wanted: BTreeMap<String, String> = BTreeMap::new();
    for node in nodes
        .iter()
        .filter(|n| n.label == NodeLabel::File && n.language == Some(LanguageId::Go))
    {
        let directory = match node.file_path.rsplit_once('/') {
            Some((dir, _)) => dir.to_string(),
            None => String::new(),
        };
        // A Go file outside every module has no import path, so nothing could
        // ever refer to it by package.
        if let Some(import_path) = resolver.go_import_path(&directory) {
            wanted.insert(directory, import_path);
        }
    }

    let existing: HashMap<String, u64> = nodes
        .iter()
        .filter(|n| n.label == NodeLabel::Package)
        .map(|n| (n.file_path.clone(), n.id))
        .collect();

    let doomed: Vec<u64> = existing
        .iter()
        .filter(|(directory, _)| !wanted.contains_key(*directory))
        .map(|(_, id)| *id)
        .collect();
    writer.remove_nodes(&doomed)?;

    let mut packages = HashMap::new();
    let mut created = false;
    for (directory, import_path) in wanted {
        let name = import_path
            .rsplit('/')
            .next()
            .unwrap_or(&import_path)
            .to_string();
        let id = match existing.get(&directory) {
            Some(&id) => id,
            None => {
                created = true;
                let id = *next_node_id;
                *next_node_id += 1;
                id
            }
        };

        let mut node = Node::new(
            NodeLabel::Package,
            name,
            import_path,
            directory.clone(),
            1,
            1,
            Some(LanguageId::Go),
        );
        node.id = id;
        node.source = Evidence::Ast;
        writer.put_node(&node)?;
        packages.insert(directory, id);
    }

    Ok((packages, created))
}

/// Which files' cross-file edges need rebuilding.
enum EdgeScope {
    /// Rebuild the whole project. Used for a full index and whenever the
    /// incremental preconditions do not hold.
    Everything,
    Files {
        /// Files re-parsed or removed in this run.
        changed: Vec<String>,
        /// Symbol names those files defined, before and after. A file that
        /// mentions one of these can resolve differently now.
        touched_names: std::collections::HashSet<String>,
    },
}

impl EdgeScope {
    /// Expand to the exact set of files whose edges must be rebuilt, or `None`
    /// to mean the whole project.
    ///
    /// A file is included when its own facts changed, or when it references a
    /// name whose definition moved, appeared or disappeared. Everything else
    /// keeps the edges it already has, because both its facts and the symbols
    /// it resolved against are untouched.
    fn expand(
        &self,
        facts_by_file: &BTreeMap<String, FileFacts>,
        resolver: &imports::ImportResolver,
    ) -> Option<Vec<String>> {
        let (changed, touched_names) = match self {
            Self::Everything => return None,
            Self::Files {
                changed,
                touched_names,
            } => (changed, touched_names),
        };

        let mut affected: std::collections::HashSet<&str> =
            changed.iter().map(String::as_str).collect();

        // A file that imports a path which just appeared or vanished resolves
        // differently now, even though no symbol name it mentions changed.
        let mut changed_spellings: std::collections::HashSet<String> = changed
            .iter()
            .flat_map(|path| path_spellings(path))
            .map(|(_, spelling)| spelling)
            .collect();
        // A Go import names the directory, so a changed Go file makes its whole
        // package a changed target.
        for path in changed.iter().filter(|p| p.ends_with(".go")) {
            if let Some((directory, _)) = path.rsplit_once('/') {
                changed_spellings.insert(directory.to_string());
            }
        }
        for (path, facts) in facts_by_file {
            if affected.contains(path.as_str()) {
                continue;
            }
            let retargets = facts.imports.iter().any(|i| {
                resolver
                    .candidates(&i.from_file, &i.target)
                    .iter()
                    .any(|c| changed_spellings.contains(c))
            });
            if retargets {
                affected.insert(path.as_str());
            }
        }

        if !touched_names.is_empty() {
            for (path, facts) in facts_by_file {
                if affected.contains(path.as_str()) {
                    continue;
                }
                let mentions = facts
                    .calls
                    .iter()
                    .any(|c| touched_names.contains(&c.callee_name))
                    || facts
                        .type_relations
                        .iter()
                        .any(|r| touched_names.contains(&r.supertype_name))
                    || facts
                        .route_links
                        .iter()
                        .any(|l| touched_names.contains(&l.handler_name))
                    || facts
                        .imports
                        .iter()
                        .any(|i| touched_names.contains(&normalise_import_target(&i.target)));
                if mentions {
                    affected.insert(path.as_str());
                }
            }
        }

        Some(affected.into_iter().map(String::from).collect())
    }
}

/// Rebuild the cross-file edges in `scope`.
fn rebuild_resolved_edges(
    store: &GraphStore,
    next_node_id: &mut u64,
    next_edge_id: &mut u32,
    phase_ms: &mut PhaseTimings,
    scope: &EdgeScope,
    resolver: &imports::ImportResolver,
) -> Result<ResolvedEdges> {
    let phase_started = Instant::now();
    let (nodes, facts_by_file) = {
        let reader = store.read()?;
        (reader.all_nodes()?, reader.all_file_facts_by_file()?)
    };

    // Resolution always consults the whole project's symbols; only the set of
    // facts being resolved narrows.
    let mut table = SymbolTable::build(&nodes);
    phase_ms.load_facts = phase_started.elapsed().as_millis() as u64;
    let phase_started = Instant::now();

    let writer = store.write()?;
    let (go_packages, package_created) = sync_go_packages(&writer, &nodes, resolver, next_node_id)?;

    // A package that did not exist a moment ago is somewhere files outside this
    // run's scope can now point, so their edges have to be reconsidered too.
    let affected = if package_created {
        None
    } else {
        scope.expand(&facts_by_file, resolver)
    };

    let mut facts = FileFacts::default();
    match &affected {
        None => {
            for file_facts in facts_by_file.values() {
                facts.calls.extend(file_facts.calls.iter().cloned());
                facts.imports.extend(file_facts.imports.iter().cloned());
                facts
                    .type_relations
                    .extend(file_facts.type_relations.iter().cloned());
                facts
                    .route_links
                    .extend(file_facts.route_links.iter().cloned());
                facts
                    .receiver_bindings
                    .extend(file_facts.receiver_bindings.iter().cloned());
            }
        }
        Some(paths) => {
            for path in paths {
                let Some(file_facts) = facts_by_file.get(path) else {
                    continue;
                };
                facts.calls.extend(file_facts.calls.iter().cloned());
                facts.imports.extend(file_facts.imports.iter().cloned());
                facts
                    .type_relations
                    .extend(file_facts.type_relations.iter().cloned());
                facts
                    .route_links
                    .extend(file_facts.route_links.iter().cloned());
                facts
                    .receiver_bindings
                    .extend(file_facts.receiver_bindings.iter().cloned());
            }
        }
    }

    // Bindings come from every file, not just the affected ones: a variable
    // typed in an untouched file still types the calls made through it here.
    let all_bindings: Vec<StoredReceiverBinding> = facts_by_file
        .values()
        .flat_map(|file_facts| file_facts.receiver_bindings.iter().cloned())
        .collect();
    table.learn_receiver_bindings(&all_bindings);

    let file_node_by_path: HashMap<String, u64> = nodes
        .iter()
        .filter(|n| n.label == NodeLabel::File)
        .map(|n| (n.file_path.clone(), n.id))
        .collect();
    let file_node_by_module: HashMap<String, u64> = nodes
        .iter()
        .filter(|n| n.label == NodeLabel::File)
        .map(|n| (n.qualified_name.clone(), n.id))
        .collect();
    let path_index = PathIndex::build(&nodes, go_packages);

    const RESOLVED_TYPES: &[EdgeType] = &[
        EdgeType::Calls,
        EdgeType::CallUnresolved,
        EdgeType::Imports,
        EdgeType::Inherits,
        EdgeType::Implements,
        EdgeType::RoutesTo,
    ];

    match &affected {
        None => {
            writer.clear_edges_of_type(RESOLVED_TYPES)?;
            // Nothing resolved survives a full rebuild, so ids can restart.
            // Structural ids live in a disjoint high range.
            *next_edge_id = 1;
        }
        Some(paths) => {
            writer.clear_resolved_edges_for_files(paths, RESOLVED_TYPES)?;
        }
    }

    // Accumulate every resolved edge and write them in one batch. Writing them
    // one at a time reopens three redb tables per edge, which dominates the
    // run once a project has millions of edges.
    let calls = resolve::build_call_edges(&table, &facts.calls, next_edge_id);
    let unresolved_calls = calls.unresolved;
    let mut pending: Vec<Edge> = calls.edges;

    for import in &facts.imports {
        let Some(&src) = file_node_by_path.get(&import.from_file) else {
            continue;
        };
        // Only record an import when it lands on a file we actually indexed;
        // third-party targets are left out rather than invented.
        let Some(dst) = resolve_import(
            resolver,
            &import.from_file,
            &import.target,
            &path_index,
            &file_node_by_module,
        ) else {
            continue;
        };
        if src == dst {
            continue;
        }
        let mut edge = Edge::new(src, dst, EdgeType::Imports);
        edge.id = *next_edge_id;
        *next_edge_id += 1;
        edge.line = Some(import.line);
        edge.detail = Some(import.target.clone());
        pending.push(edge);
    }

    for relation in &facts.type_relations {
        let Some(src) = table.node_id_for_qualified_name(&relation.subtype_qualified_name) else {
            continue;
        };
        let Some(dst) = table.resolve_type(&relation.supertype_name) else {
            continue;
        };
        if src == dst {
            continue;
        }
        let mut edge = Edge::new(src, dst, relation.edge_type);
        edge.id = *next_edge_id;
        *next_edge_id += 1;
        edge.line = Some(relation.line);
        pending.push(edge);
    }

    for link in &facts.route_links {
        let Some(src) = table.node_id_for_qualified_name(&link.route_qualified_name) else {
            continue;
        };
        // A handler written `h.Login` is a method call in every respect but the
        // parentheses, so it is resolved as one.
        let Some((dst, _)) = table.resolve_call_with_receiver(
            &link.handler_name,
            &link.file_path,
            &link.route_qualified_name,
            link.handler_receiver.as_deref(),
        ) else {
            continue;
        };
        let mut edge = Edge::new(src, dst, EdgeType::RoutesTo);
        edge.id = *next_edge_id;
        *next_edge_id += 1;
        edge.detail = Some(link.handler_name.clone());
        pending.push(edge);
    }

    writer.put_edges(&pending)?;
    writer.commit()?;
    phase_ms.resolve_edges = phase_started.elapsed().as_millis() as u64;

    // Phase two only writes edges, so the node list read at the top is still
    // current. Re-reading it, and reading every edge just to count them, cost
    // seconds on a large project.
    let reader = store.read()?;
    let mut languages: BTreeMap<String, u64> = BTreeMap::new();
    for node in &nodes {
        if node.label == NodeLabel::File {
            if let Some(language) = node.language {
                *languages.entry(language.as_str().to_string()).or_insert(0) += 1;
            }
        }
    }

    Ok(ResolvedEdges {
        // Counted from the store, not from the snapshot read at the top of this
        // function: package nodes are created in between, and reporting the
        // snapshot understated the graph by exactly that many.
        nodes: reader.node_count()?,
        edges: reader.edge_count()?,
        languages,
        unresolved_calls,
    })
}

/// Outcome of the cross-file resolution phase.
struct ResolvedEdges {
    nodes: u64,
    edges: u64,
    languages: BTreeMap<String, u64>,
    /// Sites the AST gave up on, which the Hybrid LSP pass may still settle.
    unresolved_calls: Vec<(u64, StoredCall)>,
}

/// Open the graph for a project id, failing explicitly when it is missing.
///
/// Shared read lock: the UI and MCP can inspect the same project at once.
/// Use [`open_project_write`] for ADR/trace writes; indexing opens the store
/// exclusively itself.
pub fn open_project(project_id: &str) -> Result<(ProjectEntry, GraphStore)> {
    open_project_with(project_id, GraphStore::open_read)
}

/// Exclusive writer lock. Blocks readers until this handle is dropped.
pub fn open_project_write(project_id: &str) -> Result<(ProjectEntry, GraphStore)> {
    open_project_with(project_id, GraphStore::open)
}

fn open_project_with(
    project_id: &str,
    open: fn(&Path) -> Result<GraphStore>,
) -> Result<(ProjectEntry, GraphStore)> {
    let catalog = Catalog::load()?;
    let entry = catalog.require(project_id)?.clone();
    let path = Path::new(&entry.store_path);
    if !path.exists() {
        return Err(LociError::IndexMissing(project_id.to_string()));
    }
    let store = open(path)?;
    Ok((entry, store))
}

/// Delete a project's store and catalog entry.
pub fn delete_project(project_id: &str) -> Result<ProjectEntry> {
    let mut catalog = Catalog::load()?;
    let entry = catalog
        .remove(project_id)
        .ok_or_else(|| LociError::ProjectNotFound(project_id.to_string()))?;

    let store_dir = loci_core::paths::project_store_dir(project_id)?;
    if store_dir.exists() {
        std::fs::remove_dir_all(&store_dir).map_err(|e| LociError::io(&store_dir, e))?;
    }
    catalog.save()?;
    Ok(entry)
}
