//! Proof that the Hybrid LSP pass changes the graph.
//!
//! The test indexes the same repository twice, once with the pass off and once
//! with it on, and asserts the difference. A test that only checked the "on"
//! run could pass even if the AST had resolved the call by itself, which would
//! prove nothing about LSP.

use loci_graph::{EdgeType, Evidence};
use loci_index::IndexOptions;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// `LOCI_DATA_DIR` and the project catalog are process-wide, so these tests
/// share one data directory and take turns. Setting the variable per test
/// races with whatever is already running and corrupts both runs.
fn serial() -> MutexGuard<'static, ()> {
    static DATA_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());

    let dir = DATA_DIR.get_or_init(|| tempfile::tempdir().expect("temp data dir"));
    std::env::set_var("LOCI_DATA_DIR", dir.path());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dir");
    }
    std::fs::write(path, contents).expect("write");
}

/// `svc.store.Persist(v)` is ambiguous to the AST because two types declare
/// `Persist`. gopls knows `svc.store` is a `*Store`.
fn write_ambiguous_go_repo(root: &Path) {
    write(root, "go.mod", "module locihybrid\n\ngo 1.21\n");
    write(
        root,
        "store.go",
        "package main\n\ntype Store struct{}\n\nfunc (s *Store) Persist(value string) {}\n\
         \ntype Cache struct{}\n\nfunc (c *Cache) Persist(value string) {}\n",
    );
    write(
        root,
        "service.go",
        "package main\n\ntype Service struct {\n\tstore *Store\n}\n\n\
         func (svc *Service) Save(value string) {\n\tsvc.store.Persist(value)\n}\n",
    );
}

struct Indexed {
    calls_to_persist: usize,
    unresolved_persist: usize,
    lsp_edges: usize,
}

fn index(root: &Path, name: &str, hybrid_lsp: bool) -> (Indexed, loci_index::IndexReport) {
    let report = loci_index::index_repository(
        root,
        &IndexOptions {
            name: Some(name.to_string()),
            full: true,
            hybrid_lsp,
        },
    )
    .expect("index");

    let (_, store) = loci_index::open_project(&report.project).expect("open");
    let reader = store.read().expect("read");
    let nodes = reader.all_nodes().expect("nodes");
    let edges = reader.all_edges().expect("edges");

    let persist_nodes: Vec<u64> = nodes
        .iter()
        .filter(|n| n.name == "Persist")
        .map(|n| n.id)
        .collect();

    let indexed = Indexed {
        calls_to_persist: edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Calls && persist_nodes.contains(&e.dst))
            .count(),
        unresolved_persist: edges
            .iter()
            .filter(|e| {
                e.edge_type == EdgeType::CallUnresolved
                    && e.target_name.as_deref() == Some("Persist")
            })
            .count(),
        lsp_edges: edges.iter().filter(|e| e.source == Evidence::Lsp).count(),
    };

    (indexed, report)
}

#[test]
fn hybrid_lsp_resolves_a_call_the_ast_left_unresolved() {
    let _guard = serial();
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path();
    write_ambiguous_go_repo(root);

    // Baseline: the AST alone must fail on this call. If it ever succeeds, the
    // test has stopped measuring what it claims to.
    let (ast_only, _) = index(root, "hybrid-off", false);
    assert_eq!(
        ast_only.calls_to_persist, 0,
        "the AST must not resolve an ambiguous method call"
    );
    assert_eq!(
        ast_only.unresolved_persist, 1,
        "the call must be recorded as unresolved, not dropped"
    );
    assert_eq!(ast_only.lsp_edges, 0, "no LSP edges without the pass");

    let (hybrid, report) = index(root, "hybrid-off", true);

    let go = report
        .hybrid_lsp
        .languages
        .iter()
        .find(|l| l.language == "go")
        .expect("a go entry in the hybrid report");

    if let Some(reason) = &go.skipped_reason {
        if std::env::var("LOCI_REQUIRE_LSP").is_ok() {
            panic!("gopls required but skipped: {reason}");
        }
        eprintln!("skipping: gopls unavailable ({reason})");
        return;
    }

    assert_eq!(
        go.attempted, 1,
        "exactly one call should have been asked about"
    );
    assert_eq!(go.resolved, 1, "gopls resolves this call: {go:?}");
    assert_eq!(
        hybrid.lsp_edges, 1,
        "the upgraded edge must be tagged as LSP evidence"
    );
    assert_eq!(
        hybrid.calls_to_persist, 1,
        "the call must now reach a real Persist definition"
    );
}

/// With no server for a language, indexing must produce exactly the AST result
/// and say why, rather than failing or silently degrading.
#[test]
fn a_missing_server_leaves_the_ast_result_untouched() {
    let _guard = serial();
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path();
    // Perl has no server on any machine running this suite by default.
    write(
        root,
        "lib/App.pm",
        "package App;\n\nsub run {\n    my ($self) = @_;\n    $self->missing_helper();\n}\n\n1;\n",
    );

    let (with_flag, report) = index(root, "no-server", true);

    assert!(report.hybrid_lsp.enabled);
    assert_eq!(
        with_flag.lsp_edges, 0,
        "no server means no LSP evidence may be claimed"
    );

    let perl = report
        .hybrid_lsp
        .languages
        .iter()
        .find(|l| l.language == "perl")
        .expect("a perl entry explaining itself");
    assert!(
        perl.skipped_reason.is_some(),
        "a skipped language must say why: {perl:?}"
    );
    assert_eq!(perl.resolved, 0);
}
