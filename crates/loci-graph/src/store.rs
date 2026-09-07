use crate::schema::{
    pack_adjacency, unpack_adjacency, CoverageStatus, Edge, EdgeType, FileFacts, FileRecord, Node,
    NodeLabel, ProjectMeta,
};
use loci_core::{LociError, Result};
use redb::{
    Database, MultimapTableDefinition, ReadableDatabase, ReadableMultimapTable, ReadableTable,
    ReadableTableMetadata, TableDefinition,
};
use std::collections::BTreeMap;
use std::path::Path;

const NODES: TableDefinition<u64, &[u8]> = TableDefinition::new("nodes");
const EDGES: TableDefinition<u32, &[u8]> = TableDefinition::new("edges");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const ADRS: TableDefinition<&str, &[u8]> = TableDefinition::new("adrs");
const TRACES: TableDefinition<&str, &[u8]> = TableDefinition::new("traces");

const CALLSITES: TableDefinition<&str, &[u8]> = TableDefinition::new("callsites");

const QN_INDEX: MultimapTableDefinition<&str, u64> = MultimapTableDefinition::new("qn_idx");
const NAME_INDEX: MultimapTableDefinition<&str, u64> = MultimapTableDefinition::new("name_idx");
const FILE_INDEX: MultimapTableDefinition<&str, u64> = MultimapTableDefinition::new("file_idx");
const ADJ_OUT: MultimapTableDefinition<u64, u128> = MultimapTableDefinition::new("adj_out");
const ADJ_IN: MultimapTableDefinition<u64, u128> = MultimapTableDefinition::new("adj_in");

const META_KEY: &str = "project";

fn storage<E: std::fmt::Display>(e: E) -> LociError {
    LociError::Storage(e.to_string())
}

/// A single project's persistent graph.
///
/// One redb file holds nodes, edges, the lookup indexes that make symbol
/// queries a B-tree seek rather than a scan, and the coverage records.
pub struct GraphStore {
    db: Database,
}

impl GraphStore {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            loci_core::paths::ensure_dir(parent)?;
        }
        let db = Database::create(path).map_err(storage)?;
        let store = Self { db };
        store.initialise_tables()?;
        Ok(store)
    }

    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(LociError::Storage(format!(
                "no graph at {}",
                path.display()
            )));
        }
        let db = Database::open(path).map_err(storage)?;
        Ok(Self { db })
    }

    /// Creates every table so read transactions never fail on a fresh database.
    fn initialise_tables(&self) -> Result<()> {
        let tx = self.db.begin_write().map_err(storage)?;
        {
            tx.open_table(NODES).map_err(storage)?;
            tx.open_table(EDGES).map_err(storage)?;
            tx.open_table(META).map_err(storage)?;
            tx.open_table(FILES).map_err(storage)?;
            tx.open_table(ADRS).map_err(storage)?;
            tx.open_table(TRACES).map_err(storage)?;
            tx.open_table(CALLSITES).map_err(storage)?;
            tx.open_multimap_table(QN_INDEX).map_err(storage)?;
            tx.open_multimap_table(NAME_INDEX).map_err(storage)?;
            tx.open_multimap_table(FILE_INDEX).map_err(storage)?;
            tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
            tx.open_multimap_table(ADJ_IN).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(())
    }

    pub fn write(&self) -> Result<GraphWriter> {
        let tx = self.db.begin_write().map_err(storage)?;
        Ok(GraphWriter { tx })
    }

    pub fn read(&self) -> Result<GraphReader> {
        let tx = self.db.begin_read().map_err(storage)?;
        Ok(GraphReader { tx })
    }
}

pub struct GraphWriter {
    tx: redb::WriteTransaction,
}

impl GraphWriter {
    pub fn put_meta(&self, meta: &ProjectMeta) -> Result<()> {
        let bytes = serde_json::to_vec(meta).map_err(storage)?;
        let mut table = self.tx.open_table(META).map_err(storage)?;
        table.insert(META_KEY, bytes.as_slice()).map_err(storage)?;
        Ok(())
    }

    pub fn put_node(&self, node: &Node) -> Result<()> {
        let bytes = serde_json::to_vec(node).map_err(storage)?;
        {
            let mut table = self.tx.open_table(NODES).map_err(storage)?;
            table.insert(node.id, bytes.as_slice()).map_err(storage)?;
        }
        {
            let mut qn = self.tx.open_multimap_table(QN_INDEX).map_err(storage)?;
            qn.insert(node.qualified_name.as_str(), node.id)
                .map_err(storage)?;
        }
        {
            let mut names = self.tx.open_multimap_table(NAME_INDEX).map_err(storage)?;
            names
                .insert(node.name.to_lowercase().as_str(), node.id)
                .map_err(storage)?;
        }
        if !node.file_path.is_empty() {
            let mut files = self.tx.open_multimap_table(FILE_INDEX).map_err(storage)?;
            files
                .insert(node.file_path.as_str(), node.id)
                .map_err(storage)?;
        }
        Ok(())
    }

    pub fn put_edge(&self, edge: &Edge) -> Result<()> {
        let bytes = serde_json::to_vec(edge).map_err(storage)?;
        {
            let mut table = self.tx.open_table(EDGES).map_err(storage)?;
            table.insert(edge.id, bytes.as_slice()).map_err(storage)?;
        }
        {
            let mut out = self.tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
            out.insert(edge.src, pack_adjacency(edge.edge_type, edge.dst, edge.id))
                .map_err(storage)?;
        }
        if edge.dst != crate::schema::UNRESOLVED_NODE_ID {
            let mut inbound = self.tx.open_multimap_table(ADJ_IN).map_err(storage)?;
            inbound
                .insert(edge.dst, pack_adjacency(edge.edge_type, edge.src, edge.id))
                .map_err(storage)?;
        }
        Ok(())
    }

    /// Insert many edges, opening each table once.
    ///
    /// `put_edge` opens three tables per call, which is fine for a handful and
    /// ruinous for millions: a full rebuild of this project's edges spends most
    /// of its time in `open_table`, not in the writes themselves.
    pub fn put_edges(&self, edges: &[Edge]) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }

        let mut table = self.tx.open_table(EDGES).map_err(storage)?;
        let mut out = self.tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
        let mut inbound = self.tx.open_multimap_table(ADJ_IN).map_err(storage)?;

        for edge in edges {
            let bytes = serde_json::to_vec(edge).map_err(storage)?;
            table.insert(edge.id, bytes.as_slice()).map_err(storage)?;
            out.insert(edge.src, pack_adjacency(edge.edge_type, edge.dst, edge.id))
                .map_err(storage)?;
            if edge.dst != crate::schema::UNRESOLVED_NODE_ID {
                inbound
                    .insert(edge.dst, pack_adjacency(edge.edge_type, edge.src, edge.id))
                    .map_err(storage)?;
            }
        }
        Ok(())
    }

    pub fn put_file(&self, record: &FileRecord) -> Result<()> {
        let bytes = serde_json::to_vec(record).map_err(storage)?;
        let mut table = self.tx.open_table(FILES).map_err(storage)?;
        table
            .insert(record.path.as_str(), bytes.as_slice())
            .map_err(storage)?;
        Ok(())
    }

    /// Remove a file and everything it defined. Used by incremental re-index
    /// when a file changed or disappeared.
    pub fn remove_file(&self, path: &str) -> Result<()> {
        let node_ids: Vec<u64> = {
            let files = self.tx.open_multimap_table(FILE_INDEX).map_err(storage)?;
            let mut ids = Vec::new();
            for entry in files.get(path).map_err(storage)? {
                ids.push(entry.map_err(storage)?.value());
            }
            ids
        };

        for node_id in &node_ids {
            self.remove_node_edges(*node_id)?;
        }

        {
            let mut nodes = self.tx.open_table(NODES).map_err(storage)?;
            let mut qn = self.tx.open_multimap_table(QN_INDEX).map_err(storage)?;
            let mut names = self.tx.open_multimap_table(NAME_INDEX).map_err(storage)?;
            for node_id in &node_ids {
                if let Some(raw) = nodes.remove(*node_id).map_err(storage)? {
                    let node: Node = serde_json::from_slice(raw.value()).map_err(storage)?;
                    qn.remove(node.qualified_name.as_str(), node.id)
                        .map_err(storage)?;
                    names
                        .remove(node.name.to_lowercase().as_str(), node.id)
                        .map_err(storage)?;
                }
            }
        }

        {
            let mut files = self.tx.open_multimap_table(FILE_INDEX).map_err(storage)?;
            files.remove_all(path).map_err(storage)?;
        }
        {
            let mut table = self.tx.open_table(FILES).map_err(storage)?;
            table.remove(path).map_err(storage)?;
        }
        {
            let mut table = self.tx.open_table(CALLSITES).map_err(storage)?;
            table.remove(path).map_err(storage)?;
        }
        Ok(())
    }

    /// Remove nodes that no file owns, together with their edges.
    ///
    /// `remove_file` reaches nodes through the file that defined them. A Go
    /// package node belongs to a directory instead, so it needs a way out of
    /// the graph when that directory stops holding Go files.
    pub fn remove_nodes(&self, node_ids: &[u64]) -> Result<()> {
        if node_ids.is_empty() {
            return Ok(());
        }

        for node_id in node_ids {
            self.remove_node_edges(*node_id)?;
        }

        let mut nodes = self.tx.open_table(NODES).map_err(storage)?;
        let mut qn = self.tx.open_multimap_table(QN_INDEX).map_err(storage)?;
        let mut names = self.tx.open_multimap_table(NAME_INDEX).map_err(storage)?;
        let mut files = self.tx.open_multimap_table(FILE_INDEX).map_err(storage)?;
        for node_id in node_ids {
            if let Some(raw) = nodes.remove(*node_id).map_err(storage)? {
                let node: Node = serde_json::from_slice(raw.value()).map_err(storage)?;
                qn.remove(node.qualified_name.as_str(), node.id)
                    .map_err(storage)?;
                names
                    .remove(node.name.to_lowercase().as_str(), node.id)
                    .map_err(storage)?;
                if !node.file_path.is_empty() {
                    files
                        .remove(node.file_path.as_str(), node.id)
                        .map_err(storage)?;
                }
            }
        }
        Ok(())
    }

    /// Drop every edge touching a node, from both adjacency directions.
    fn remove_node_edges(&self, node_id: u64) -> Result<()> {
        let mut edge_ids: Vec<u32> = Vec::new();
        let mut outbound: Vec<(EdgeType, u64, u32)> = Vec::new();
        let mut inbound: Vec<(EdgeType, u64, u32)> = Vec::new();

        {
            let out = self.tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
            for entry in out.get(node_id).map_err(storage)? {
                if let Some(unpacked) = unpack_adjacency(entry.map_err(storage)?.value()) {
                    edge_ids.push(unpacked.2);
                    outbound.push(unpacked);
                }
            }
            let inb = self.tx.open_multimap_table(ADJ_IN).map_err(storage)?;
            for entry in inb.get(node_id).map_err(storage)? {
                if let Some(unpacked) = unpack_adjacency(entry.map_err(storage)?.value()) {
                    edge_ids.push(unpacked.2);
                    inbound.push(unpacked);
                }
            }
        }

        {
            let mut out = self.tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
            let mut inb = self.tx.open_multimap_table(ADJ_IN).map_err(storage)?;
            out.remove_all(node_id).map_err(storage)?;
            inb.remove_all(node_id).map_err(storage)?;

            // Mirror entries live on the other endpoint and must go too.
            for (ty, dst, edge_id) in outbound {
                if dst != crate::schema::UNRESOLVED_NODE_ID {
                    inb.remove(dst, pack_adjacency(ty, node_id, edge_id))
                        .map_err(storage)?;
                }
            }
            for (ty, src, edge_id) in inbound {
                out.remove(src, pack_adjacency(ty, node_id, edge_id))
                    .map_err(storage)?;
            }
        }

        {
            let mut edges = self.tx.open_table(EDGES).map_err(storage)?;
            for edge_id in edge_ids {
                edges.remove(edge_id).map_err(storage)?;
            }
        }
        Ok(())
    }

    /// Persist the cross-file facts a file contributes, so resolution can be
    /// redone without re-parsing. Incremental runs rebuild every resolved edge
    /// from these, which keeps edges from untouched files correct when a callee
    /// moves or is renamed.
    pub fn put_file_facts(&self, path: &str, facts: &FileFacts) -> Result<()> {
        let bytes = serde_json::to_vec(facts).map_err(storage)?;
        let mut table = self.tx.open_table(CALLSITES).map_err(storage)?;
        table.insert(path, bytes.as_slice()).map_err(storage)?;
        Ok(())
    }

    /// Drop every edge of the given types, from both adjacency directions.
    pub fn clear_edges_of_type(&self, types: &[EdgeType]) -> Result<()> {
        let doomed: Vec<Edge> = {
            let table = self.tx.open_table(EDGES).map_err(storage)?;
            let mut found = Vec::new();
            for entry in table.iter().map_err(storage)? {
                let (_, raw) = entry.map_err(storage)?;
                let edge: Edge = serde_json::from_slice(raw.value()).map_err(storage)?;
                if types.contains(&edge.edge_type) {
                    found.push(edge);
                }
            }
            found
        };

        let mut edges = self.tx.open_table(EDGES).map_err(storage)?;
        let mut out = self.tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
        let mut inbound = self.tx.open_multimap_table(ADJ_IN).map_err(storage)?;
        for edge in doomed {
            edges.remove(edge.id).map_err(storage)?;
            out.remove(edge.src, pack_adjacency(edge.edge_type, edge.dst, edge.id))
                .map_err(storage)?;
            if edge.dst != crate::schema::UNRESOLVED_NODE_ID {
                inbound
                    .remove(edge.dst, pack_adjacency(edge.edge_type, edge.src, edge.id))
                    .map_err(storage)?;
            }
        }
        Ok(())
    }

    /// Drop the resolved edges leaving the given files, leaving every other
    /// edge in place.
    ///
    /// This is the incremental counterpart to `clear_edges_of_type`. Clearing
    /// everything costs time proportional to the whole graph even when one file
    /// changed, which is the difference between a one second and a one minute
    /// re-index on a large repository.
    pub fn clear_resolved_edges_for_files(
        &self,
        paths: &[String],
        types: &[EdgeType],
    ) -> Result<()> {
        let mut node_ids: Vec<u64> = Vec::new();
        {
            let files = self.tx.open_multimap_table(FILE_INDEX).map_err(storage)?;
            for path in paths {
                for entry in files.get(path.as_str()).map_err(storage)? {
                    node_ids.push(entry.map_err(storage)?.value());
                }
            }
        }

        // Collect first: the adjacency tables cannot be read and written at once.
        let mut doomed: Vec<(EdgeType, u64, u64, u32)> = Vec::new();
        {
            let out = self.tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
            for src in &node_ids {
                for entry in out.get(*src).map_err(storage)? {
                    if let Some((edge_type, dst, edge_id)) =
                        unpack_adjacency(entry.map_err(storage)?.value())
                    {
                        if types.contains(&edge_type) {
                            doomed.push((edge_type, *src, dst, edge_id));
                        }
                    }
                }
            }
        }

        let mut edges = self.tx.open_table(EDGES).map_err(storage)?;
        let mut out = self.tx.open_multimap_table(ADJ_OUT).map_err(storage)?;
        let mut inbound = self.tx.open_multimap_table(ADJ_IN).map_err(storage)?;
        for (edge_type, src, dst, edge_id) in doomed {
            edges.remove(edge_id).map_err(storage)?;
            out.remove(src, pack_adjacency(edge_type, dst, edge_id))
                .map_err(storage)?;
            if dst != crate::schema::UNRESOLVED_NODE_ID {
                inbound
                    .remove(dst, pack_adjacency(edge_type, src, edge_id))
                    .map_err(storage)?;
            }
        }
        Ok(())
    }

    pub fn put_adr(&self, id: &str, value: &serde_json::Value) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(storage)?;
        let mut table = self.tx.open_table(ADRS).map_err(storage)?;
        table.insert(id, bytes.as_slice()).map_err(storage)?;
        Ok(())
    }

    pub fn remove_adr(&self, id: &str) -> Result<bool> {
        let mut table = self.tx.open_table(ADRS).map_err(storage)?;
        let existed = table.remove(id).map_err(storage)?.is_some();
        Ok(existed)
    }

    pub fn put_trace(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(storage)?;
        let mut table = self.tx.open_table(TRACES).map_err(storage)?;
        table.insert(key, bytes.as_slice()).map_err(storage)?;
        Ok(())
    }

    pub fn commit(self) -> Result<()> {
        self.tx.commit().map_err(storage)
    }
}

pub struct GraphReader {
    tx: redb::ReadTransaction,
}

impl GraphReader {
    pub fn meta(&self) -> Result<Option<ProjectMeta>> {
        let table = self.tx.open_table(META).map_err(storage)?;
        match table.get(META_KEY).map_err(storage)? {
            Some(raw) => Ok(Some(serde_json::from_slice(raw.value()).map_err(storage)?)),
            None => Ok(None),
        }
    }

    pub fn node(&self, id: u64) -> Result<Option<Node>> {
        let table = self.tx.open_table(NODES).map_err(storage)?;
        match table.get(id).map_err(storage)? {
            Some(raw) => Ok(Some(serde_json::from_slice(raw.value()).map_err(storage)?)),
            None => Ok(None),
        }
    }

    pub fn edge(&self, id: u32) -> Result<Option<Edge>> {
        let table = self.tx.open_table(EDGES).map_err(storage)?;
        match table.get(id).map_err(storage)? {
            Some(raw) => Ok(Some(serde_json::from_slice(raw.value()).map_err(storage)?)),
            None => Ok(None),
        }
    }

    /// Exact qualified-name lookup: one B-tree seek.
    pub fn nodes_by_qualified_name(&self, qn: &str) -> Result<Vec<Node>> {
        let index = self.tx.open_multimap_table(QN_INDEX).map_err(storage)?;
        let mut ids = Vec::new();
        for entry in index.get(qn).map_err(storage)? {
            ids.push(entry.map_err(storage)?.value());
        }
        self.load_nodes(&ids)
    }

    /// Exact simple-name lookup, case-insensitive.
    pub fn nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        let index = self.tx.open_multimap_table(NAME_INDEX).map_err(storage)?;
        let mut ids = Vec::new();
        for entry in index.get(name.to_lowercase().as_str()).map_err(storage)? {
            ids.push(entry.map_err(storage)?.value());
        }
        self.load_nodes(&ids)
    }

    /// Prefix scan over the name index. Bounded by `cap` so a broad prefix
    /// cannot turn into a full-graph scan.
    pub fn nodes_by_name_prefix(&self, prefix: &str, cap: usize) -> Result<Vec<Node>> {
        let index = self.tx.open_multimap_table(NAME_INDEX).map_err(storage)?;
        let lower = prefix.to_lowercase();
        let mut ids = Vec::new();
        for entry in index.range(lower.as_str()..).map_err(storage)? {
            let (key, values) = entry.map_err(storage)?;
            if !key.value().starts_with(&lower) {
                break;
            }
            for value in values {
                ids.push(value.map_err(storage)?.value());
                if ids.len() >= cap {
                    break;
                }
            }
            if ids.len() >= cap {
                break;
            }
        }
        self.load_nodes(&ids)
    }

    pub fn nodes_in_file(&self, path: &str) -> Result<Vec<Node>> {
        let index = self.tx.open_multimap_table(FILE_INDEX).map_err(storage)?;
        let mut ids = Vec::new();
        for entry in index.get(path).map_err(storage)? {
            ids.push(entry.map_err(storage)?.value());
        }
        self.load_nodes(&ids)
    }

    /// Full node scan. Only for whole-graph reports such as get_architecture.
    pub fn all_nodes(&self) -> Result<Vec<Node>> {
        let table = self.tx.open_table(NODES).map_err(storage)?;
        let mut nodes = Vec::new();
        for entry in table.iter().map_err(storage)? {
            let (_, raw) = entry.map_err(storage)?;
            nodes.push(serde_json::from_slice(raw.value()).map_err(storage)?);
        }
        Ok(nodes)
    }

    pub fn all_edges(&self) -> Result<Vec<Edge>> {
        let table = self.tx.open_table(EDGES).map_err(storage)?;
        let mut edges = Vec::new();
        for entry in table.iter().map_err(storage)? {
            let (_, raw) = entry.map_err(storage)?;
            edges.push(serde_json::from_slice(raw.value()).map_err(storage)?);
        }
        Ok(edges)
    }

    /// Neighbours of `node_id`, optionally filtered by edge type.
    ///
    /// Returns `(edge_type, other_node_id, edge_id)`. Unresolved calls appear
    /// with `other_node_id == UNRESOLVED_NODE_ID`.
    pub fn neighbours(
        &self,
        node_id: u64,
        outbound: bool,
        allowed: Option<&[EdgeType]>,
    ) -> Result<Vec<(EdgeType, u64, u32)>> {
        let table = if outbound { ADJ_OUT } else { ADJ_IN };
        let index = self.tx.open_multimap_table(table).map_err(storage)?;
        let mut out = Vec::new();
        for entry in index.get(node_id).map_err(storage)? {
            if let Some(unpacked) = unpack_adjacency(entry.map_err(storage)?.value()) {
                if allowed.is_none_or(|types| types.contains(&unpacked.0)) {
                    out.push(unpacked);
                }
            }
        }
        Ok(out)
    }

    pub fn file_record(&self, path: &str) -> Result<Option<FileRecord>> {
        let table = self.tx.open_table(FILES).map_err(storage)?;
        match table.get(path).map_err(storage)? {
            Some(raw) => Ok(Some(serde_json::from_slice(raw.value()).map_err(storage)?)),
            None => Ok(None),
        }
    }

    pub fn file_facts(&self, path: &str) -> Result<FileFacts> {
        let table = self.tx.open_table(CALLSITES).map_err(storage)?;
        match table.get(path).map_err(storage)? {
            Some(raw) => serde_json::from_slice(raw.value()).map_err(storage),
            None => Ok(FileFacts::default()),
        }
    }

    /// Every file's facts, kept per file so an incremental run can resolve a
    /// subset instead of the whole project.
    pub fn all_file_facts_by_file(&self) -> Result<std::collections::BTreeMap<String, FileFacts>> {
        let table = self.tx.open_table(CALLSITES).map_err(storage)?;
        let mut all = std::collections::BTreeMap::new();
        for entry in table.iter().map_err(storage)? {
            let (path, raw) = entry.map_err(storage)?;
            let facts: FileFacts = serde_json::from_slice(raw.value()).map_err(storage)?;
            all.insert(path.value().to_string(), facts);
        }
        Ok(all)
    }

    /// Number of edges, without deserialising any of them.
    pub fn edge_count(&self) -> Result<u64> {
        let table = self.tx.open_table(EDGES).map_err(storage)?;
        table.len().map_err(storage)
    }

    /// Number of nodes, without deserialising any of them.
    pub fn node_count(&self) -> Result<u64> {
        let table = self.tx.open_table(NODES).map_err(storage)?;
        table.len().map_err(storage)
    }

    pub fn all_files(&self) -> Result<Vec<FileRecord>> {
        let table = self.tx.open_table(FILES).map_err(storage)?;
        let mut records = Vec::new();
        for entry in table.iter().map_err(storage)? {
            let (_, raw) = entry.map_err(storage)?;
            records.push(serde_json::from_slice(raw.value()).map_err(storage)?);
        }
        Ok(records)
    }

    pub fn files_with_status(&self, status: CoverageStatus) -> Result<Vec<FileRecord>> {
        Ok(self
            .all_files()?
            .into_iter()
            .filter(|f| f.status == status)
            .collect())
    }

    pub fn adrs(&self) -> Result<Vec<serde_json::Value>> {
        let table = self.tx.open_table(ADRS).map_err(storage)?;
        let mut out = Vec::new();
        for entry in table.iter().map_err(storage)? {
            let (_, raw) = entry.map_err(storage)?;
            out.push(serde_json::from_slice(raw.value()).map_err(storage)?);
        }
        Ok(out)
    }

    pub fn adr(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let table = self.tx.open_table(ADRS).map_err(storage)?;
        match table.get(id).map_err(storage)? {
            Some(raw) => Ok(Some(serde_json::from_slice(raw.value()).map_err(storage)?)),
            None => Ok(None),
        }
    }

    pub fn traces(&self) -> Result<Vec<serde_json::Value>> {
        let table = self.tx.open_table(TRACES).map_err(storage)?;
        let mut out = Vec::new();
        for entry in table.iter().map_err(storage)? {
            let (_, raw) = entry.map_err(storage)?;
            out.push(serde_json::from_slice(raw.value()).map_err(storage)?);
        }
        Ok(out)
    }

    pub fn label_counts(&self) -> Result<BTreeMap<&'static str, u64>> {
        let mut counts = BTreeMap::new();
        for node in self.all_nodes()? {
            *counts.entry(node.label.as_str()).or_insert(0) += 1;
        }
        Ok(counts)
    }

    fn load_nodes(&self, ids: &[u64]) -> Result<Vec<Node>> {
        let table = self.tx.open_table(NODES).map_err(storage)?;
        let mut nodes = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(raw) = table.get(*id).map_err(storage)? {
                nodes.push(serde_json::from_slice(raw.value()).map_err(storage)?);
            }
        }
        Ok(nodes)
    }
}

/// Convenience used by tests and by tools that only need one label filter.
pub fn filter_by_label(nodes: Vec<Node>, label: Option<NodeLabel>) -> Vec<Node> {
    match label {
        Some(l) => nodes.into_iter().filter(|n| n.label == l).collect(),
        None => nodes,
    }
}
