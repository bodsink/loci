//! Persistent code knowledge graph.
//!
//! One redb file per project holds nodes, edges, the lookup indexes that keep
//! symbol queries to a B-tree seek, and per-file coverage records. Nothing here
//! stores source text: nodes carry a repository-relative path and a line range,
//! and readers go back to the file through a sandbox when they need the code.

pub mod catalog;
pub mod coverage;
pub mod query;
pub mod schema;
pub mod store;

pub use catalog::{Catalog, ProjectEntry};
pub use schema::{
    CoverageReason, CoverageStatus, Edge, EdgeType, Evidence, FileFacts, FileRecord, Node,
    NodeLabel, ProjectMeta, StoredCall, StoredImport, StoredReceiverBinding, StoredRouteLink,
    StoredTypeRel, STRUCTURAL_EDGE_ID_BASE, UNRESOLVED_NODE_ID,
};
pub use store::{GraphReader, GraphStore, GraphWriter};

#[cfg(test)]
mod tests {
    use super::*;
    use loci_core::LanguageId;

    fn temp_store() -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::create(&dir.path().join("graph.redb")).unwrap();
        (dir, store)
    }

    fn function(id: u64, name: &str, qn: &str, file: &str, line: u32) -> Node {
        let mut node = Node::new(
            NodeLabel::Function,
            name,
            qn,
            file,
            line,
            line + 5,
            Some(LanguageId::Python),
        );
        node.id = id;
        node
    }

    #[test]
    fn writes_and_reads_nodes_by_every_index() {
        let (_dir, store) = temp_store();

        let writer = store.write().unwrap();
        writer
            .put_node(&function(
                1,
                "handle_order",
                "api.orders.handle_order",
                "api/orders.py",
                10,
            ))
            .unwrap();
        writer
            .put_node(&function(
                2,
                "handle_refund",
                "api.orders.handle_refund",
                "api/orders.py",
                30,
            ))
            .unwrap();
        writer.commit().unwrap();

        let reader = store.read().unwrap();
        assert_eq!(reader.nodes_by_name("handle_order").unwrap().len(), 1);
        assert_eq!(
            reader
                .nodes_by_qualified_name("api.orders.handle_refund")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(reader.nodes_in_file("api/orders.py").unwrap().len(), 2);
        assert_eq!(reader.nodes_by_name_prefix("handle", 100).unwrap().len(), 2);
    }

    #[test]
    fn traverses_call_edges_in_both_directions() {
        let (_dir, store) = temp_store();

        let writer = store.write().unwrap();
        writer
            .put_node(&function(1, "a", "m.a", "m.py", 1))
            .unwrap();
        writer
            .put_node(&function(2, "b", "m.b", "m.py", 10))
            .unwrap();
        let mut edge = Edge::new(1, 2, EdgeType::Calls);
        edge.id = 1;
        writer.put_edge(&edge).unwrap();
        writer.commit().unwrap();

        let reader = store.read().unwrap();
        let out = reader
            .neighbours(1, true, Some(&[EdgeType::Calls]))
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, 2);

        let inbound = reader
            .neighbours(2, false, Some(&[EdgeType::Calls]))
            .unwrap();
        assert_eq!(inbound.len(), 1);
        assert_eq!(inbound[0].1, 1);
    }

    #[test]
    fn unresolved_calls_have_no_inbound_mirror() {
        let (_dir, store) = temp_store();

        let writer = store.write().unwrap();
        writer
            .put_node(&function(1, "a", "m.a", "m.py", 1))
            .unwrap();
        let mut edge = Edge::unresolved_call(1, "requests.get", 4, "external symbol");
        edge.id = 1;
        writer.put_edge(&edge).unwrap();
        writer.commit().unwrap();

        let reader = store.read().unwrap();
        let out = reader.neighbours(1, true, None).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, UNRESOLVED_NODE_ID);
        assert_eq!(reader.neighbours(0, false, None).unwrap().len(), 0);
    }

    #[test]
    fn removing_a_file_drops_its_nodes_and_edges() {
        let (_dir, store) = temp_store();

        let writer = store.write().unwrap();
        writer
            .put_node(&function(1, "a", "m.a", "m.py", 1))
            .unwrap();
        writer
            .put_node(&function(2, "b", "other.py", "other.py", 1))
            .unwrap();
        let mut edge = Edge::new(1, 2, EdgeType::Calls);
        edge.id = 1;
        writer.put_edge(&edge).unwrap();
        writer
            .put_file(&FileRecord {
                path: "m.py".to_string(),
                hash: "abc".to_string(),
                size: 10,
                language: Some(LanguageId::Python),
                status: CoverageStatus::Indexed,
                reason: None,
                detail: None,
                node_ids: vec![1],
            })
            .unwrap();
        writer.commit().unwrap();

        let writer = store.write().unwrap();
        writer.remove_file("m.py").unwrap();
        writer.commit().unwrap();

        let reader = store.read().unwrap();
        assert!(reader.node(1).unwrap().is_none());
        assert!(reader.nodes_by_name("a").unwrap().is_empty());
        assert!(reader.file_record("m.py").unwrap().is_none());
        // The mirror entry on the surviving node is gone too.
        assert!(reader.neighbours(2, false, None).unwrap().is_empty());
    }

    #[test]
    fn graph_survives_reopening_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.redb");

        {
            let store = GraphStore::create(&path).unwrap();
            let writer = store.write().unwrap();
            writer
                .put_node(&function(1, "persisted", "m.persisted", "m.py", 3))
                .unwrap();
            writer
                .put_meta(&ProjectMeta::new("sample".into(), "/tmp/sample".into()))
                .unwrap();
            writer.commit().unwrap();
        }

        let store = GraphStore::open(&path).unwrap();
        let reader = store.read().unwrap();
        assert_eq!(reader.nodes_by_name("persisted").unwrap().len(), 1);
        assert_eq!(reader.meta().unwrap().unwrap().name, "sample");
    }

    #[test]
    fn a_second_exclusive_open_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.redb");
        let _held = GraphStore::create(&path).unwrap();
        let err = match GraphStore::open(&path) {
            Ok(_) => panic!("a second exclusive open must fail"),
            Err(error) => error,
        };
        assert_eq!(err.code(), "storage_error");
        assert!(
            err.to_string().contains("locked for writing"),
            "exclusive lock must be named, not the raw redb string: {err}"
        );
    }

    #[test]
    fn two_read_only_opens_can_run_together() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.redb");
        {
            let store = GraphStore::create(&path).unwrap();
            let writer = store.write().unwrap();
            writer
                .put_node(&function(1, "shared", "m.shared", "m.py", 1))
                .unwrap();
            writer
                .put_meta(&ProjectMeta::new("sample".into(), "/tmp/sample".into()))
                .unwrap();
            writer.commit().unwrap();
        }

        let first = GraphStore::open_read(&path).unwrap();
        let second = GraphStore::open_read(&path).unwrap();
        assert_eq!(first.read().unwrap().node_count().unwrap(), 1);
        assert_eq!(second.read().unwrap().node_count().unwrap(), 1);
    }
}
