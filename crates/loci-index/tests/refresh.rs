//! Automatic refresh must notice an edit and leave a clean tree alone.

use loci_graph::query::{self, SearchRequest};
use loci_index::{refresh_if_stale, IndexOptions};

fn isolated() -> tempfile::TempDir {
    let data = tempfile::tempdir().expect("data dir");
    std::env::set_var("LOCI_DATA_DIR", data.path());
    data
}

#[test]
fn refresh_leaves_a_clean_tree_and_indexes_an_edit() {
    let _data = isolated();
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("app.py"), "def alpha():\n    return 1\n").expect("write");

    let report = loci_index::index_repository(
        root.path(),
        &IndexOptions {
            name: Some("refresh-demo".into()),
            full: false,
            hybrid_lsp: false,
        },
    )
    .expect("index");
    assert_eq!(report.files_reparsed, 1);

    let again = loci_index::index_repository(
        root.path(),
        &IndexOptions {
            name: None,
            full: false,
            hybrid_lsp: false,
        },
    )
    .expect("second index");
    assert_eq!(
        again.files_reparsed, 0,
        "matching mtime and size must not re-parse"
    );
    assert_eq!(
        again.phase_ms.load_facts, 0,
        "a clean tree must not load the graph"
    );
    assert!(!refresh_if_stale(&report.project).expect("clean refresh"));

    let oversized = vec![0u8; loci_core::MAX_FILE_BYTES as usize + 1];
    std::fs::write(root.path().join("blob.bin"), &oversized).expect("oversized");
    let with_blob = loci_index::index_repository(
        root.path(),
        &IndexOptions {
            name: None,
            full: false,
            hybrid_lsp: false,
        },
    )
    .expect("index oversized");
    assert_eq!(with_blob.files_reparsed, 1);
    let blob_again = loci_index::index_repository(
        root.path(),
        &IndexOptions {
            name: None,
            full: false,
            hybrid_lsp: false,
        },
    )
    .expect("second oversized index");
    assert_eq!(
        blob_again.files_reparsed, 0,
        "an oversized file with a stored hash must not be reprocessed"
    );
    assert_eq!(blob_again.phase_ms.load_facts, 0);

    std::fs::write(
        root.path().join("app.py"),
        "def alpha():\n    return 1\n\ndef beta():\n    return 2\n",
    )
    .expect("edit");
    assert!(refresh_if_stale(&report.project).expect("stale refresh"));

    let (entry, store) = loci_index::open_project(&report.project).expect("open");
    assert_eq!(entry.id, report.project);
    let reader = store.read().expect("read");
    let found = query::search(
        &reader,
        &SearchRequest {
            name: Some("beta".into()),
            ..Default::default()
        },
    )
    .expect("search");
    assert_eq!(found.total, 1, "the edit must be in the graph");
}
