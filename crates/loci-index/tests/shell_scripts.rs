//! Shell scripts are code, and a packaging tree is mostly shell.
//!
//! A real project here had 8 scripts totalling ~570 lines invisible to the
//! graph: five `.sh` files under `scripts/`, and three Debian maintainer
//! scripts (`postinst`, `prerm`, `postrm`) that carry no extension at all and
//! so could only be recognised by their `#!` line.

use loci_index::IndexOptions;
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

fn index(root: &Path, name: &str) -> loci_index::IndexReport {
    loci_index::index_repository(
        root,
        &IndexOptions {
            name: Some(name.to_string()),
            full: true,
            hybrid_lsp: false,
        },
    )
    .expect("index")
}

fn symbols(project: &str) -> Vec<String> {
    let (_, store) = loci_index::open_project(project).expect("open");
    store
        .read()
        .expect("read")
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| n.name.clone())
        .collect()
}

/// Shaped after `scripts/build-deb-core.sh`, which sources a sibling for its
/// linker flags.
const BUILD_SCRIPT: &str = r#"#!/usr/bin/env bash
set -euo pipefail

source ./core-ldflags.sh

build_core() {
  local out="$1"
  go build -ldflags "$(core_ldflags)" -o "$out"
}

package_deb() {
  build_core /tmp/sagara-core
  dpkg-deb --build /tmp/stage
}

package_deb
"#;

#[test]
fn a_shell_script_contributes_its_functions_to_the_graph() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "scripts/build-deb-core.sh", BUILD_SCRIPT);

    let report = index(root.path(), "sh-basic");

    assert_eq!(
        report.languages.get("bash").copied(),
        Some(1),
        "the script must be recognised as shell: {:?}",
        report.languages
    );
    assert_eq!(report.files_parse_partial, 0, "shell must parse cleanly");

    let names = symbols("sh-basic");
    for expected in ["build_core", "package_deb"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} must reach the graph: {names:?}"
        );
    }
}

/// Without a shebang check these are skipped as an unknown language, purely
/// for lacking a suffix.
#[test]
fn an_extensionless_maintainer_script_is_recognised_by_its_shebang() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "packaging/sagara-core/postinst",
        "#!/bin/sh\nset -e\n\nenable_service() {\n  systemctl enable sagara-core\n}\n\nenable_service\n",
    );
    // A plain text file alongside it must stay skipped: the shebang is the
    // signal, not the absence of an extension.
    write(
        root.path(),
        "packaging/sagara-core/copyright",
        "Copyright 2026\n",
    );

    let report = index(root.path(), "sh-shebang");

    assert_eq!(
        report.languages.get("bash").copied(),
        Some(1),
        "only the script counts as shell: {:?}",
        report.languages
    );
    assert!(
        symbols("sh-shebang").iter().any(|n| n == "enable_service"),
        "a function in an extensionless script must reach the graph"
    );
}

/// `source lib.sh` is an import, not a call to a function named `source`. The
/// distinction matters because the query engine has no text predicates, so the
/// obvious implementation files every command as one or the other.
#[test]
fn sourcing_a_file_is_an_import_and_not_a_call() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "scripts/build-deb-core.sh", BUILD_SCRIPT);
    write(
        root.path(),
        "scripts/core-ldflags.sh",
        "#!/bin/sh\ncore_ldflags() {\n  echo \"-s -w\"\n}\n",
    );
    write(
        root.path(),
        "scripts/dot-form.sh",
        "#!/bin/sh\n. ./core-ldflags.sh\n",
    );

    index(root.path(), "sh-imports");

    let (_, store) = loci_index::open_project("sh-imports").expect("open");
    let reader = store.read().expect("read");
    let edges = reader.all_edges().expect("edges");

    let mistaken_for_calls = edges
        .iter()
        .filter(|e| {
            matches!(
                e.edge_type,
                loci_graph::EdgeType::Calls | loci_graph::EdgeType::CallUnresolved
            )
        })
        .filter(|e| matches!(e.target_name.as_deref(), Some("source") | Some(".")))
        .count();
    assert_eq!(
        mistaken_for_calls, 0,
        "`source` and `.` must never become call edges"
    );

    let imports = edges
        .iter()
        .filter(|e| e.edge_type == loci_graph::EdgeType::Imports)
        .count();
    assert!(
        imports >= 2,
        "both `source` and `.` must produce imports, found {imports}"
    );
}

/// A command that is genuinely a call must still be recorded, or the walk that
/// separates imports from calls has thrown the call graph away.
#[test]
fn a_function_called_from_another_function_is_recorded() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "scripts/build-deb-core.sh", BUILD_SCRIPT);

    index(root.path(), "sh-calls");

    let (_, store) = loci_index::open_project("sh-calls").expect("open");
    let reader = store.read().expect("read");
    let by_id: std::collections::BTreeMap<u64, String> = reader
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| (n.id, n.name.clone()))
        .collect();

    let resolved = reader
        .all_edges()
        .expect("edges")
        .iter()
        .filter(|e| e.edge_type == loci_graph::EdgeType::Calls)
        .filter(|e| by_id.get(&e.dst).map(String::as_str) == Some("build_core"))
        .count();
    assert!(
        resolved >= 1,
        "package_deb calls build_core, and both are defined in this file"
    );
}
