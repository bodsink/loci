//! A build file says how a repository turns into artefacts, and until now that
//! was the largest thing left unindexed: `Makefile` and `CMakeLists.txt` were
//! both reported as unsupported_language.
//!
//! Neither is named by a useful extension. `Makefile` has none at all, and
//! `.txt` cannot be allowed to mean CMake in general, so both are recognised
//! by whole file name.

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

/// Resolved edges of one type, as `(source name, target name)`.
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

const MAKEFILE: &str = "\
BINARY := sagara
PREFIX ?= /usr/local

.PHONY: all

all: build

build:
\tgo build -o $(BINARY) ./cmd

install: build
\tinstall -m 0755 $(BINARY) $(PREFIX)/bin
";

#[test]
fn a_makefile_is_recognised_without_any_extension() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "Makefile", MAKEFILE);

    let report = index(root.path(), "build-make");

    assert_eq!(
        report.languages.get("make").copied(),
        Some(1),
        "a file called Makefile must be recognised: {:?}",
        report.languages
    );
    assert_eq!(report.files_parse_partial, 0, "make must parse cleanly");
}

/// A target is a unit other targets invoke, and a variable is a setting, so
/// they must not land under the same label.
#[test]
fn targets_become_callable_and_variables_become_fields() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "Makefile", MAKEFILE);

    index(root.path(), "build-make-labels");
    let found = nodes("build-make-labels");

    for target in ["all", "build", "install"] {
        assert!(
            found
                .iter()
                .any(|(label, name, _)| *label == NodeLabel::Function && name == target),
            "target {target} must be callable: {found:?}"
        );
    }
    for variable in ["BINARY", "PREFIX"] {
        assert!(
            found
                .iter()
                .any(|(label, name, _)| *label == NodeLabel::Field && name == variable),
            "variable {variable} must be a field: {found:?}"
        );
    }
}

/// The point of treating prerequisites as calls: `trace_path` can then answer
/// "what breaks if this target changes", which is the question a Makefile is
/// usually opened to answer.
#[test]
fn a_prerequisite_becomes_a_call_edge_between_targets() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "Makefile", MAKEFILE);

    index(root.path(), "build-make-edges");
    let calls = edges("build-make-edges", EdgeType::Calls);

    assert!(
        calls.contains(&("install".to_string(), "build".to_string())),
        "install depends on build: {calls:?}"
    );
    assert!(
        calls.contains(&("all".to_string(), "build".to_string())),
        "all depends on build: {calls:?}"
    );
}

#[test]
fn an_included_makefile_is_an_import_not_a_call() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "Makefile", "include config.mk\n\nall: build\n");
    write(root.path(), "config.mk", "CC := gcc\n");

    index(root.path(), "build-make-include");
    let imports = edges("build-make-include", EdgeType::Imports);

    assert!(
        imports
            .iter()
            .any(|(from, to)| from == "Makefile" && to == "config.mk"),
        "the include must reach the included file: {imports:?}"
    );
}

const CMAKELISTS: &str = "\
cmake_minimum_required(VERSION 3.16)
project(sagara LANGUAGES CXX)

function(add_sagara_module name)
  add_library(${name} STATIC ${ARGN})
endfunction()

macro(configure_thing arg)
  message(STATUS \"${arg}\")
endmacro()

add_sagara_module(core core.cpp)
include(cmake/toolchain.cmake)
add_subdirectory(src)
";

#[test]
fn a_cmakelists_is_recognised_by_name_while_plain_txt_is_not() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "CMakeLists.txt", CMAKELISTS);
    write(
        root.path(),
        "cmake/toolchain.cmake",
        "set(CMAKE_C_COMPILER cc)\n",
    );
    // The extension is the same, and it must go on meaning nothing.
    write(root.path(), "notes.txt", "not a build file\n");

    let report = index(root.path(), "build-cmake");

    assert_eq!(
        report.languages.get("cmake").copied(),
        Some(2),
        "CMakeLists.txt and the .cmake file, and nothing else: {:?}",
        report.languages
    );
    assert_eq!(report.files_parse_partial, 0, "cmake must parse cleanly");
}

#[test]
fn cmake_functions_and_macros_reach_the_graph() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "CMakeLists.txt", CMAKELISTS);

    index(root.path(), "build-cmake-defs");
    let found = nodes("build-cmake-defs");

    for definition in ["add_sagara_module", "configure_thing"] {
        assert!(
            found
                .iter()
                .any(|(label, name, _)| *label == NodeLabel::Function && name == definition),
            "{definition} must be defined in the graph: {found:?}"
        );
    }
    // The first argument names the definition; the rest are parameters and must
    // not be mistaken for further names.
    assert!(
        !found
            .iter()
            .any(|(_, name, _)| name == "arg" || name == "name"),
        "a parameter must not become a definition: {found:?}"
    );
}

#[test]
fn calling_a_cmake_function_resolves_to_its_definition() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "CMakeLists.txt", CMAKELISTS);

    index(root.path(), "build-cmake-calls");
    let calls = edges("build-cmake-calls", EdgeType::Calls);

    assert!(
        calls.iter().any(|(_, to)| to == "add_sagara_module"),
        "the call to the function defined above must resolve: {calls:?}"
    );
}

/// `include()` and `add_subdirectory()` are ordinary commands, so without the
/// dedicated walk they would become calls to functions named "include" and
/// "add_subdirectory" that nothing defines.
#[test]
fn include_and_add_subdirectory_become_imports() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "CMakeLists.txt", CMAKELISTS);
    write(
        root.path(),
        "cmake/toolchain.cmake",
        "set(CMAKE_C_COMPILER cc)\n",
    );
    write(
        root.path(),
        "src/CMakeLists.txt",
        "add_library(core core.cpp)\n",
    );

    index(root.path(), "build-cmake-imports");

    let imports = edges("build-cmake-imports", EdgeType::Imports);
    assert!(
        imports.iter().any(|(_, to)| to == "toolchain.cmake"),
        "include() must reach the included file: {imports:?}"
    );
    assert!(
        imports
            .iter()
            .filter(|(from, _)| from == "CMakeLists.txt")
            .count()
            >= 2,
        "add_subdirectory() must reach the nested CMakeLists too: {imports:?}"
    );

    let (_, store) = loci_index::open_project("build-cmake-imports").expect("open");
    let reader = store.read().expect("read");
    let bogus = reader
        .all_edges()
        .expect("edges")
        .iter()
        .filter(|e| {
            matches!(e.edge_type, EdgeType::Calls | EdgeType::CallUnresolved)
                && matches!(
                    e.target_name.as_deref(),
                    Some("include") | Some("add_subdirectory")
                )
        })
        .count();
    assert_eq!(bogus, 0, "neither may also be recorded as a call");
}

/// CMake does not care about case, and projects that shout their commands are
/// common enough that treating INCLUDE() as a call would be a silent hole.
#[test]
fn an_uppercase_command_is_still_an_include() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "CMakeLists.txt",
        "INCLUDE(cmake/tools.cmake)\n",
    );
    write(root.path(), "cmake/tools.cmake", "set(A 1)\n");

    index(root.path(), "build-cmake-case");
    let imports = edges("build-cmake-case", EdgeType::Imports);

    assert!(
        imports.iter().any(|(_, to)| to == "tools.cmake"),
        "INCLUDE must behave as include: {imports:?}"
    );
}

#[test]
fn build_files_are_parsed_but_never_lsp_eligible() {
    use loci_core::LanguageId;
    for language in [LanguageId::Make, LanguageId::Cmake] {
        assert!(
            loci_parse::is_bundled(language),
            "{language} must have a grammar"
        );
        assert!(
            !language.hybrid_lsp_eligible(),
            "{language} has no language server in scope and must not claim one"
        );
    }
}
