//! Imports only became edges when the text of the import happened to match a
//! file's dotted module name. That is almost never true in real projects.
//!
//! Measured on a real repository before this: `frontend/src/types/index.ts` was
//! imported by 941 files and had an in-degree of zero, because the importers
//! wrote `@/types` and nothing taught the index what `@/` meant. The same held
//! for `../lib/axios` from a subdirectory and for every Dart `package:` URI.
//!
//! These tests pin the spellings that a manifest — or plain path arithmetic —
//! makes resolvable, and equally pin the ones that must stay unresolved.

use loci_graph::EdgeType;
use loci_index::IndexOptions;
use std::collections::BTreeMap;
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

fn index(root: &Path, name: &str) {
    loci_index::index_repository(
        root,
        &IndexOptions {
            name: Some(name.to_string()),
            full: true,
            hybrid_lsp: false,
        },
    )
    .expect("index");
}

/// Import edges as `(importing path, imported path)`.
///
/// Paths rather than names, because the interesting targets are files like
/// `index.ts` whose basename says nothing about which one it is. A Go package
/// node carries its directory in the same field, so it reads the same way.
fn import_edges(project: &str) -> Vec<(String, String)> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let reader = store.read().expect("read");
    let by_id: BTreeMap<u64, String> = reader
        .all_nodes()
        .expect("nodes")
        .iter()
        .filter(|n| {
            matches!(
                n.label,
                loci_graph::NodeLabel::File | loci_graph::NodeLabel::Package
            )
        })
        .map(|n| (n.id, n.file_path.clone()))
        .collect();
    let mut edges: Vec<(String, String)> = reader
        .all_edges()
        .expect("edges")
        .iter()
        .filter(|e| e.edge_type == EdgeType::Imports)
        .filter_map(|e| Some((by_id.get(&e.src)?.clone(), by_id.get(&e.dst)?.clone())))
        .collect();
    edges.sort();
    edges
}

fn imports(project: &str, from: &str, to: &str) -> bool {
    import_edges(project)
        .iter()
        .any(|(src, dst)| src == from && dst == to)
}

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    /* Path mapping */
    "moduleResolution": "bundler",
    "paths": {
      "@/*": ["./src/*"],
    },
  },
}"#;

/// The frontend layout the measurement came from: a `tsconfig.json` one level
/// down, sources under `src`, and a barrel file imported by everything.
fn write_frontend(root: &Path) {
    write(root, "frontend/tsconfig.json", TSCONFIG);
    write(
        root,
        "frontend/src/types/index.ts",
        "export interface User { id: string }\n",
    );
    write(
        root,
        "frontend/src/lib/axios.ts",
        "export const client = 1;\n",
    );
    write(
        root,
        "frontend/src/pages/Home.tsx",
        "import { User } from '@/types';\n\
         import { client } from '../lib/axios';\n\
         import React from 'react';\n\
         export const Home = () => null;\n",
    );
}

#[test]
fn an_alias_import_reaches_the_file_the_manifest_points_at() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_frontend(dir.path());
    index(dir.path(), "imports-alias");

    assert!(
        imports(
            "imports-alias",
            "frontend/src/pages/Home.tsx",
            "frontend/src/types/index.ts"
        ),
        "'@/types' should resolve through tsconfig paths to the barrel file, got {:?}",
        import_edges("imports-alias")
    );
}

/// This is the case that needed no manifest at all and still failed: the old
/// resolver dotted `../lib/axios` into `...lib.axios` and compared it to module
/// names, so a relative import could only ever match a file at the root.
#[test]
fn a_relative_import_from_a_subdirectory_becomes_an_edge() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_frontend(dir.path());
    index(dir.path(), "imports-relative");

    assert!(
        imports(
            "imports-relative",
            "frontend/src/pages/Home.tsx",
            "frontend/src/lib/axios.ts"
        ),
        "'../lib/axios' should resolve, got {:?}",
        import_edges("imports-relative")
    );
}

#[test]
fn a_third_party_import_stays_unresolved() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_frontend(dir.path());
    index(dir.path(), "imports-third-party");

    assert!(
        !import_edges("imports-third-party")
            .iter()
            .any(|(_, dst)| dst.contains("react")),
        "'react' is not in the repository and must not invent an edge"
    );
}

/// An alias belongs to the compilation its manifest describes. Applying it to a
/// sibling project would fabricate edges between unrelated trees.
#[test]
fn an_alias_does_not_apply_outside_its_own_project() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_frontend(dir.path());
    write(dir.path(), "admin/src/types.ts", "export const x = 1;\n");
    write(
        dir.path(),
        "admin/src/main.ts",
        "import { x } from '@/types';\nexport const y = x;\n",
    );
    index(dir.path(), "imports-scope");

    assert!(
        !imports("imports-scope", "admin/src/main.ts", "admin/src/types.ts"),
        "admin has no tsconfig, so '@/' means nothing there"
    );
    assert!(
        !imports(
            "imports-scope",
            "admin/src/main.ts",
            "frontend/src/types/index.ts"
        ),
        "an alias must not reach across into the frontend tree"
    );
}

#[test]
fn a_dart_package_uri_resolves_through_pubspec() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "mobile/pubspec.yaml",
        "name: goinfracloud\ndescription: app\n\ndependencies:\n  flutter:\n    sdk: flutter\n",
    );
    write(
        dir.path(),
        "mobile/lib/models/user.dart",
        "class User {\n  const User();\n}\n",
    );
    write(
        dir.path(),
        "mobile/lib/app.dart",
        "import 'package:goinfracloud/models/user.dart';\n\
         import 'package:flutter/material.dart';\n\
         import 'dart:async';\n\n\
         class App {\n  User? current;\n}\n",
    );
    index(dir.path(), "imports-dart");

    assert!(
        imports(
            "imports-dart",
            "mobile/lib/app.dart",
            "mobile/lib/models/user.dart"
        ),
        "the app's own package: URI should resolve, got {:?}",
        import_edges("imports-dart")
    );
    assert_eq!(
        import_edges("imports-dart").len(),
        1,
        "'package:flutter' and 'dart:async' are outside the repository"
    );
}

/// `import x from './widgets'` names a directory; the entry point inside it is
/// the file that is actually loaded.
#[test]
fn a_directory_import_lands_on_its_entry_point() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "src/widgets/index.ts", "export const w = 1;\n");
    write(
        dir.path(),
        "src/app.ts",
        "import { w } from './widgets';\nexport const a = w;\n",
    );
    index(dir.path(), "imports-directory");

    assert!(
        imports("imports-directory", "src/app.ts", "src/widgets/index.ts"),
        "got {:?}",
        import_edges("imports-directory")
    );
}

fn write_go_repo(root: &Path) {
    write(
        root,
        "backend/go.mod",
        "module github.com/bodsink/olt\n\ngo 1.21\n",
    );
    write(
        root,
        "backend/internal/handlers/auth.go",
        "package handlers\n\nfunc Login() {}\n",
    );
    write(
        root,
        "backend/internal/handlers/users.go",
        "package handlers\n\nfunc ListUsers() {}\n",
    );
    write(
        root,
        "backend/cmd/api/main.go",
        "package main\n\nimport (\n\t\"fmt\"\n\t\"github.com/bodsink/olt/internal/handlers\"\n)\n\n\
         func main() {\n\tfmt.Println(handlers.Login)\n}\n",
    );
}

fn packages(project: &str) -> Vec<(String, String)> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let mut found: Vec<(String, String)> = store
        .read()
        .expect("read")
        .all_nodes()
        .expect("nodes")
        .iter()
        .filter(|n| n.label == loci_graph::NodeLabel::Package)
        .map(|n| (n.file_path.clone(), n.qualified_name.clone()))
        .collect();
    found.sort();
    found
}

/// A Go import names a package, so it points at one package node. Pointing it
/// at every file of the package instead was measured on a real repository and
/// produced 64,296 edges from a single package.
#[test]
fn a_go_import_points_at_one_package_node() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_go_repo(dir.path());
    index(dir.path(), "imports-go");

    assert!(
        imports(
            "imports-go",
            "backend/cmd/api/main.go",
            "backend/internal/handlers"
        ),
        "got {:?}",
        import_edges("imports-go")
    );
    assert_eq!(
        import_edges("imports-go").len(),
        1,
        "one import, one edge; 'fmt' is the standard library and must not appear"
    );
}

/// The package node is named the way source refers to it, so an agent that has
/// only read an import statement can look it up.
#[test]
fn a_package_node_is_named_by_its_import_path() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_go_repo(dir.path());
    index(dir.path(), "imports-go-naming");

    assert_eq!(
        packages("imports-go-naming"),
        vec![
            (
                "backend/cmd/api".to_string(),
                "github.com/bodsink/olt/cmd/api".to_string()
            ),
            (
                "backend/internal/handlers".to_string(),
                "github.com/bodsink/olt/internal/handlers".to_string()
            ),
        ]
    );
}

/// Nothing else owns a package node, so if it is not removed when its Go files
/// go away it stays in the graph forever.
#[test]
fn a_package_node_disappears_with_its_last_go_file() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_go_repo(dir.path());
    index(dir.path(), "imports-go-removal");
    assert_eq!(packages("imports-go-removal").len(), 2);

    std::fs::remove_file(dir.path().join("backend/internal/handlers/auth.go")).expect("remove");
    std::fs::remove_file(dir.path().join("backend/internal/handlers/users.go")).expect("remove");
    loci_index::index_repository(
        dir.path(),
        &IndexOptions {
            name: Some("imports-go-removal".to_string()),
            full: false,
            hybrid_lsp: false,
        },
    )
    .expect("incremental index");

    assert_eq!(
        packages("imports-go-removal"),
        vec![(
            "backend/cmd/api".to_string(),
            "github.com/bodsink/olt/cmd/api".to_string()
        )],
        "the emptied package must not linger"
    );
    assert!(
        import_edges("imports-go-removal").is_empty(),
        "the import edge went with it"
    );
}

/// Go files in the same package do not import each other, and an import must
/// not produce a self-edge.
#[test]
fn a_go_package_does_not_import_itself() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "go.mod", "module example.com/app\n\ngo 1.21\n");
    write(
        dir.path(),
        "store/a.go",
        "package store\n\nimport \"example.com/app/store\"\n\nfunc A() {}\n",
    );
    write(dir.path(), "store/b.go", "package store\n\nfunc B() {}\n");
    index(dir.path(), "imports-go-self");

    assert!(
        !import_edges("imports-go-self")
            .iter()
            .any(|(src, dst)| src == dst),
        "no file may import itself"
    );
}

/// Python and Rust name modules, not paths, and resolved correctly before.
#[test]
fn dotted_module_imports_still_resolve() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "core.py", "def helper():\n    return 1\n");
    write(
        dir.path(),
        "app.py",
        "from core import helper\n\ndef run():\n    return helper()\n",
    );
    index(dir.path(), "imports-dotted");

    assert!(
        imports("imports-dotted", "app.py", "core.py"),
        "got {:?}",
        import_edges("imports-dotted")
    );
}

/// An import that pointed nowhere must become an edge as soon as its target
/// appears, without a full reindex. The affected-file analysis has to notice a
/// changed *path*, not just a changed symbol name.
#[test]
fn an_incremental_run_connects_an_import_whose_target_appears() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "frontend/tsconfig.json", TSCONFIG);
    write(
        dir.path(),
        "frontend/src/pages/Home.tsx",
        "import { User } from '@/types';\nexport const Home = () => null;\n",
    );
    index(dir.path(), "imports-incremental");
    assert!(
        import_edges("imports-incremental").is_empty(),
        "nothing to point at yet"
    );

    write(
        dir.path(),
        "frontend/src/types/index.ts",
        "export interface User { id: string }\n",
    );
    loci_index::index_repository(
        dir.path(),
        &IndexOptions {
            name: Some("imports-incremental".to_string()),
            full: false,
            hybrid_lsp: false,
        },
    )
    .expect("incremental index");

    assert!(
        imports(
            "imports-incremental",
            "frontend/src/pages/Home.tsx",
            "frontend/src/types/index.ts"
        ),
        "the incremental run should connect the import, got {:?}",
        import_edges("imports-incremental")
    );
}
