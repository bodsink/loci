use crate::walk;
use loci_core::{content_hash, Result, Sandbox};
use loci_graph::{GraphStore, NodeLabel};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub path: String,
    /// `added`, `modified` or `removed`.
    pub change: &'static str,
    /// Symbols the index currently attributes to this file. For a modified
    /// file these are the *pre-change* symbols, which is what a caller needs
    /// to reason about blast radius.
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeReport {
    pub project: String,
    pub root: String,
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub files: Vec<ChangedFile>,
    pub total: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub note: &'static str,
}

const NOTE: &str = "Compares the working tree against the hashes recorded at the last index run. \
                    Run index_repository to bring the graph up to date.";

/// Compare the project root on disk with what the graph recorded.
///
/// This is filesystem-based, not git-based: it answers "is the index stale?"
/// without requiring a repository or a clean working tree.
pub fn detect_changes(
    store: &GraphStore,
    sandbox: &Sandbox,
    project: &str,
    limit: usize,
    offset: usize,
) -> Result<ChangeReport> {
    let reader = store.read()?;
    let recorded: HashMap<String, String> = reader
        .all_files()?
        .into_iter()
        .map(|record| (record.path, record.hash))
        .collect();

    let mut symbols_by_file: HashMap<String, Vec<String>> = HashMap::new();
    for node in reader.all_nodes()? {
        if node.label == NodeLabel::File || node.file_path.is_empty() {
            continue;
        }
        symbols_by_file
            .entry(node.file_path.clone())
            .or_default()
            .push(node.qualified_name);
    }

    let candidates = walk::collect(sandbox);
    let on_disk: HashSet<String> = candidates.iter().map(|c| c.relative_path.clone()).collect();

    let mut files = Vec::new();
    let mut unchanged = 0usize;

    for candidate in &candidates {
        let current = std::fs::read(&candidate.absolute_path)
            .map(|bytes| content_hash(&bytes))
            .unwrap_or_default();

        match recorded.get(&candidate.relative_path) {
            Some(previous) if *previous == current && !current.is_empty() => unchanged += 1,
            Some(_) => files.push(ChangedFile {
                path: candidate.relative_path.clone(),
                change: "modified",
                symbols: sorted_symbols(&symbols_by_file, &candidate.relative_path),
            }),
            None => files.push(ChangedFile {
                path: candidate.relative_path.clone(),
                change: "added",
                symbols: Vec::new(),
            }),
        }
    }

    for path in recorded.keys() {
        if !on_disk.contains(path) {
            files.push(ChangedFile {
                path: path.clone(),
                change: "removed",
                symbols: sorted_symbols(&symbols_by_file, path),
            });
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let added = files.iter().filter(|f| f.change == "added").count();
    let modified = files.iter().filter(|f| f.change == "modified").count();
    let removed = files.iter().filter(|f| f.change == "removed").count();
    let total = files.len();

    let page: Vec<ChangedFile> = files.into_iter().skip(offset).take(limit).collect();
    let has_more = offset + page.len() < total;

    Ok(ChangeReport {
        project: project.to_string(),
        root: sandbox.root().to_string_lossy().to_string(),
        added,
        modified,
        removed,
        unchanged,
        files: page,
        total,
        has_more,
        cursor: has_more.then(|| (offset + limit).to_string()),
        note: NOTE,
    })
}

fn sorted_symbols(map: &HashMap<String, Vec<String>>, path: &str) -> Vec<String> {
    let mut symbols = map.get(path).cloned().unwrap_or_default();
    symbols.sort();
    symbols.truncate(50);
    symbols
}
