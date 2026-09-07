//! Dart was the largest gap a real project had left: 244 files, 202 of them a
//! Flutter application under `mobile/lib`, none of it visible to the graph.
//!
//! Unlike the config and markup formats added before it, this is behaviour —
//! classes, methods and calls — so what matters is that the call graph and the
//! type hierarchy come out, not just that the files stop being skipped.

use loci_graph::{EdgeType, NodeLabel};
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

fn nodes(project: &str) -> Vec<(NodeLabel, String, String)> {
    let (_, store) = loci_index::open_project(project).expect("open");
    store
        .read()
        .expect("read")
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| (n.label, n.name.clone(), n.qualified_name.clone()))
        .collect()
}

fn edges(project: &str, wanted: EdgeType) -> Vec<(String, String)> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let reader = store.read().expect("read");
    let by_id: BTreeMap<u64, String> = reader
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| (n.id, n.name.clone()))
        .collect();
    reader
        .all_edges()
        .expect("edges")
        .iter()
        .filter(|e| e.edge_type == wanted)
        .filter_map(|e| Some((by_id.get(&e.src)?.clone(), by_id.get(&e.dst)?.clone())))
        .collect()
}

fn labelled(found: &[(NodeLabel, String, String)], label: NodeLabel, name: &str) -> bool {
    found.iter().any(|(l, n, _)| *l == label && n == name)
}

/// A model class in the shape the project writes them: a const constructor,
/// final fields, and computed values exposed as getters.
const MODEL: &str = r#"class User extends Person with Logging implements Serialisable {
  const User({required this.id, this.fullName});

  final String id;
  final String? fullName;

  String get displayName => fullName ?? id;

  set nickname(String value) {}

  User copyWith({String? id}) => User(id: id ?? this.id);

  static User empty() => const User(id: '');
}

mixin Logging {
  void log(String message) {}
}

extension UserX on User {
  bool get isEmpty => id.isEmpty;
}

enum Role { admin, viewer }

typedef Handler = void Function(String);

void bootstrap() {
  final user = User.empty();
  user.log('ready');
  helper();
}

void helper() {}
"#;

#[test]
fn a_dart_file_is_recognised_and_parses_cleanly() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "user.dart", MODEL);

    let report = index(root.path(), "dart-basic");

    assert_eq!(
        report.languages.get("dart").copied(),
        Some(1),
        "dart must be a recognised language: {:?}",
        report.languages
    );
    assert_eq!(report.files_parse_partial, 0, "dart must parse cleanly");
}

#[test]
fn the_declarations_a_dart_file_makes_become_nodes() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "user.dart", MODEL);
    index(root.path(), "dart-decls");

    let found = nodes("dart-decls");

    for (label, name) in [
        (NodeLabel::Class, "User"),
        (NodeLabel::Trait, "Logging"),
        (NodeLabel::Class, "UserX"),
        (NodeLabel::Enum, "Role"),
        (NodeLabel::Field, "admin"),
        (NodeLabel::Type, "Handler"),
        (NodeLabel::Function, "bootstrap"),
        (NodeLabel::Field, "id"),
    ] {
        assert!(
            labelled(&found, label, name),
            "{label:?} {name} is missing: {found:?}"
        );
    }
}

/// Dart spells a method and a top-level function the same way, so the only
/// thing separating them is the body they sit in. Getters matter as much as
/// methods here: a model class exposes most of itself through them.
#[test]
fn methods_and_getters_are_methods_not_free_functions() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "user.dart", MODEL);
    index(root.path(), "dart-methods");

    let found = nodes("dart-methods");

    for name in ["copyWith", "empty", "displayName", "nickname", "log"] {
        assert!(
            labelled(&found, NodeLabel::Method, name),
            "{name} must be a method: {found:?}"
        );
    }
    assert!(
        labelled(&found, NodeLabel::Function, "helper"),
        "a top-level function must stay a function"
    );
    assert!(
        !labelled(&found, NodeLabel::Function, "copyWith"),
        "a method must not also be recorded as a free function"
    );
}

#[test]
fn a_method_is_named_under_the_class_that_declares_it() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "user.dart", MODEL);
    index(root.path(), "dart-scope");

    let found = nodes("dart-scope");

    assert!(
        found
            .iter()
            .any(|(_, name, qualified)| name == "copyWith" && qualified == "user.User.copyWith"),
        "a method must carry its class in its qualified name: {found:?}"
    );
}

#[test]
fn a_call_to_a_function_in_the_project_becomes_an_edge() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "user.dart", MODEL);
    index(root.path(), "dart-calls");

    let calls = edges("dart-calls", EdgeType::Calls);

    assert!(
        calls.contains(&("bootstrap".to_string(), "helper".to_string())),
        "a call between two functions in one file must resolve: {calls:?}"
    );
}

/// All three of Dart's ways of taking on another type, which are what a
/// question like "what implements this" has to walk.
#[test]
fn extends_with_and_implements_all_become_type_edges() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "user.dart", MODEL);
    write(
        root.path(),
        "person.dart",
        "class Person {}\nclass Serialisable {}\n",
    );
    index(root.path(), "dart-types");

    let inherits = edges("dart-types", EdgeType::Inherits);
    let implements = edges("dart-types", EdgeType::Implements);

    assert!(
        inherits.contains(&("User".to_string(), "Person".to_string())),
        "extends must be an inheritance edge: {inherits:?}"
    );
    assert!(
        implements.contains(&("User".to_string(), "Serialisable".to_string())),
        "implements must be recorded: {implements:?}"
    );
    assert!(
        implements.contains(&("User".to_string(), "Logging".to_string())),
        "a mixin is taken on the same way, and must be walkable too: {implements:?}"
    );
}

#[test]
fn an_import_of_a_file_in_the_project_becomes_an_edge() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "app.dart",
        "import 'models/user.dart';\n\nvoid run() {}\n",
    );
    write(root.path(), "models/user.dart", "class User {}\n");
    index(root.path(), "dart-imports");

    let imports = edges("dart-imports", EdgeType::Imports);

    assert!(
        imports.contains(&("app.dart".to_string(), "user.dart".to_string())),
        "a path import must reach the file it names: {imports:?}"
    );
}

/// `dart:` is the SDK and `package:` is the pub cache or the project's own
/// package, and neither is a path this indexer can follow yet. They must not
/// invent an edge, and must not stop the file being read.
#[test]
fn sdk_and_package_imports_do_not_invent_edges() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "app.dart",
        "import 'dart:async';\nimport 'package:flutter/material.dart';\n\nvoid run() {}\n",
    );

    let report = index(root.path(), "dart-external");

    assert_eq!(report.files_parse_partial, 0);
    assert!(
        edges("dart-external", EdgeType::Imports).is_empty(),
        "neither target is a file this project ships"
    );
}
