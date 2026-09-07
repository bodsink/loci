//! Incremental indexing must be indistinguishable from a full rebuild.
//!
//! The optimisation that makes a large re-index fast only rebuilds the edges of
//! files it believes are affected. If that belief is ever wrong the graph goes
//! quietly stale, which is worse than being slow. So the property under test is
//! not "incremental is fast" but "incremental produces exactly the same graph".

use loci_index::IndexOptions;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn serial() -> MutexGuard<'static, ()> {
    static DATA_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let dir = DATA_DIR.get_or_init(|| tempfile::tempdir().expect("data dir"));
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

fn index(root: &Path, name: &str, full: bool) {
    loci_index::index_repository(
        root,
        &IndexOptions {
            name: Some(name.to_string()),
            full,
            hybrid_lsp: false,
        },
    )
    .expect("index");
}

/// Edges described by what they mean rather than by id, because ids legitimately
/// differ between an incremental run and a rebuild.
fn edge_fingerprint(project: &str) -> BTreeSet<String> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let reader = store.read().expect("read");
    let nodes = reader.all_nodes().expect("nodes");

    let describe = |id: u64| -> String {
        nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| format!("{}:{}", n.label.as_str(), n.qualified_name))
            .unwrap_or_else(|| format!("#{id}"))
    };

    reader
        .all_edges()
        .expect("edges")
        .iter()
        .map(|edge| {
            format!(
                "{} -{}({})-> {} @{:?} {:?}",
                describe(edge.src),
                edge.edge_type.as_str(),
                edge.source.as_str(),
                describe(edge.dst),
                edge.line,
                edge.target_name,
            )
        })
        .collect()
}

/// Symbols with no structural parent, which should never happen: the indexer
/// emits exactly one DEFINES or CONTAINS edge for every definition it writes.
fn orphaned_symbols(project: &str) -> Vec<String> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let reader = store.read().expect("read");
    let parented: std::collections::HashSet<u64> = reader
        .all_edges()
        .expect("edges")
        .iter()
        .filter(|e| {
            matches!(
                e.edge_type,
                loci_graph::EdgeType::Defines | loci_graph::EdgeType::Contains
            )
        })
        .map(|e| e.dst)
        .collect();

    let mut orphans: Vec<String> = reader
        .all_nodes()
        .expect("nodes")
        .iter()
        .filter(|n| n.label != loci_graph::NodeLabel::File && !parented.contains(&n.id))
        .map(|n| n.qualified_name.clone())
        .collect();
    orphans.sort();
    orphans
}

fn node_fingerprint(project: &str) -> BTreeSet<String> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let reader = store.read().expect("read");
    reader
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| {
            format!(
                "{}:{}:{}:{}-{}",
                n.label.as_str(),
                n.qualified_name,
                n.file_path,
                n.start_line,
                n.end_line
            )
        })
        .collect()
}

/// Each step is a change that stresses a different part of the affected-set
/// logic: a body edit that changes nothing structural, a new definition that
/// should settle someone else's unresolved call, a deletion that should break
/// an existing call, a rename, and a whole file disappearing.
fn apply_step(root: &Path, step: usize) {
    match step {
        0 => {
            write(
                root,
                "core.py",
                "def helper():\n    return 1\n\ndef shared():\n    return 2\n",
            );
            write(
                root,
                "app.py",
                "from core import helper\n\ndef run():\n    helper()\n    shared()\n    late()\n",
            );
        }
        // A body edit only. Nothing another file resolves against changes.
        1 => write(
            root,
            "core.py",
            "def helper():\n    return 42\n\ndef shared():\n    return 2\n",
        ),
        // `late` appears for the first time, in a file that app.py never
        // mentions by path. app.py's unresolved call must become resolved.
        2 => write(root, "extra.py", "def late():\n    return 3\n"),
        // `shared` disappears, so app.py's resolved call must go back to
        // unresolved even though app.py itself did not change.
        3 => write(root, "core.py", "def helper():\n    return 42\n"),
        // `helper` is renamed; the old call must not keep pointing anywhere.
        4 => write(root, "core.py", "def helper_renamed():\n    return 42\n"),
        // The whole file goes away.
        5 => std::fs::remove_file(root.join("extra.py")).expect("remove"),
        _ => unreachable!(),
    }
}

#[test]
fn an_incrementally_built_graph_matches_a_full_rebuild_at_every_step() {
    let _guard = serial();
    let incremental_root = tempfile::tempdir().expect("temp");
    let rebuilt_root = tempfile::tempdir().expect("temp");

    for step in 0..6 {
        // Two identical trees: one kept up to date incrementally, one thrown
        // away and rebuilt from scratch at this same step.
        apply_step(incremental_root.path(), step);
        apply_step(rebuilt_root.path(), step);

        index(incremental_root.path(), "eq-incremental", false);
        index(rebuilt_root.path(), "eq-rebuilt", true);

        assert_eq!(
            node_fingerprint("eq-incremental"),
            node_fingerprint("eq-rebuilt"),
            "nodes diverged at step {step}"
        );

        let incremental = edge_fingerprint("eq-incremental");
        let rebuilt = edge_fingerprint("eq-rebuilt");
        assert_eq!(
            incremental, rebuilt,
            "edges diverged at step {step}\nonly incremental: {:?}\nonly rebuilt: {:?}",
            incremental.difference(&rebuilt).collect::<Vec<_>>(),
            rebuilt.difference(&incremental).collect::<Vec<_>>(),
        );
    }
}

/// Indexing the same tree twice must produce the same graph.
///
/// It did not: structural edge ids were a 31-bit hash of `(src, dst, type)`,
/// and node ids shift between runs, so a different handful of edges collided
/// and was silently overwritten each time. On a 600k-node project that lost
/// around a hundred DEFINES and CONTAINS edges, and the count moved run to run.
#[test]
fn indexing_the_same_tree_twice_produces_the_same_graph() {
    let _guard = serial();
    let first_root = tempfile::tempdir().expect("temp");
    let second_root = tempfile::tempdir().expect("temp");

    // Enough symbols that colliding ids are likely rather than lucky, and
    // deliberately repetitive so many edges share a shape.
    for root in [first_root.path(), second_root.path()] {
        for file in 0..40 {
            let mut source = String::new();
            for symbol in 0..40 {
                source.push_str(&format!(
                    "class C{symbol}:\n    def m{symbol}(self):\n        return {symbol}\n\ndef f{symbol}():\n    return C{symbol}().m{symbol}()\n\n"
                ));
            }
            write(root, &format!("pkg/mod{file}.py"), &source);
        }
    }

    index(first_root.path(), "det-first", true);
    let first_nodes = node_fingerprint("det-first");
    let first_edges = edge_fingerprint("det-first");

    // Index the second copy twice. A rebuild continues the node id counter
    // rather than resetting it, so this run allocates ids from a higher base.
    // That shift is what used to change which structural edge ids collided.
    index(second_root.path(), "det-second", true);
    index(second_root.path(), "det-second", true);
    let second_nodes = node_fingerprint("det-second");
    let second_edges = edge_fingerprint("det-second");

    // The scale-independent invariant. A collision in the id space overwrote
    // one structural edge with another, leaving a symbol with no parent. That
    // shows up here at any size, whereas the collisions themselves only become
    // likely in the hundreds of thousands of edges.
    for project in ["det-first", "det-second"] {
        assert_eq!(
            orphaned_symbols(project),
            Vec::<String>::new(),
            "every symbol must have exactly one DEFINES or CONTAINS parent in {project}"
        );
    }

    assert_eq!(first_nodes, second_nodes, "nodes differ between two indexes");
    assert_eq!(
        first_edges.len(),
        second_edges.len(),
        "edge count differs between two indexes of the same tree"
    );
    assert_eq!(first_edges, second_edges, "edges differ between two indexes");
}

/// The interesting steps must actually exercise resolution, otherwise the
/// equivalence test above could pass on a graph with no edges at all.
#[test]
fn the_equivalence_fixture_really_produces_resolved_and_unresolved_calls() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");

    apply_step(root.path(), 0);
    index(root.path(), "eq-shape", true);

    let edges = edge_fingerprint("eq-shape");
    assert!(
        edges.iter().any(|e| e.contains("-CALLS(ast)->")),
        "the fixture must produce resolved calls: {edges:?}"
    );
    assert!(
        edges.iter().any(|e| e.contains("CALL_UNRESOLVED")),
        "the fixture must produce an unresolved call: {edges:?}"
    );

    // After `late` is defined, that unresolved call must be gone.
    apply_step(root.path(), 2);
    index(root.path(), "eq-shape", false);
    let edges = edge_fingerprint("eq-shape");
    assert!(
        !edges.iter().any(|e| e.contains("\"late\"")),
        "defining late must resolve the call that named it: {edges:?}"
    );
}
