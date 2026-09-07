//! Configuration is where a repository says which binary a service runs and
//! which job a pipeline executes. Skipping it leaves that unanswerable.
//!
//! These formats are parsed for structure only. A section becomes a Module and
//! a key becomes a Field, reusing labels that already exist rather than
//! inventing a category for config.

use loci_graph::NodeLabel;
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

/// Every node as `(label, simple name, qualified name)`.
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

/// Taken from `configs/systemd/sagara-core.service`. The unit suffix, not an
/// `.ini` extension, is what has to be recognised.
#[test]
fn a_systemd_unit_yields_its_sections_and_settings() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "configs/systemd/sagara-core.service",
        "[Unit]\nDescription=Sagara Core\nAfter=network-online.target\n\n\
         [Service]\nType=simple\nExecStart=/usr/bin/sagara-core --config /etc/sagara.toml\n\n\
         [Install]\nWantedBy=multi-user.target\n",
    );

    let report = index(root.path(), "cfg-systemd");
    assert_eq!(
        report.languages.get("ini").copied(),
        Some(1),
        "a .service file must be recognised: {:?}",
        report.languages
    );
    assert_eq!(report.files_parse_partial, 0);

    let found = nodes("cfg-systemd");
    let sections: Vec<&String> = found
        .iter()
        .filter(|(label, _, _)| *label == NodeLabel::Module)
        .map(|(_, name, _)| name)
        .collect();
    assert!(
        ["Unit", "Service", "Install"]
            .iter()
            .all(|s| sections.iter().any(|n| n.as_str() == *s)),
        "each section must be a Module: {sections:?}"
    );

    // The setting that names the binary is the whole reason to index this file.
    let exec_start = found
        .iter()
        .find(|(label, name, _)| *label == NodeLabel::Field && name == "ExecStart")
        .expect("ExecStart must be a Field");
    assert!(
        exec_start.2.ends_with("Service.ExecStart"),
        "a setting must be qualified by its section, got {}",
        exec_start.2
    );
}

#[test]
fn a_toml_file_yields_its_tables_and_keys() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "configs/app.toml",
        "[server]\nlisten = \"0.0.0.0:9473\"\n\n[database.primary]\nurl = \"postgres://x\"\n",
    );

    index(root.path(), "cfg-toml");
    let found = nodes("cfg-toml");

    assert!(
        found
            .iter()
            .any(|(label, name, _)| *label == NodeLabel::Module && name == "server"),
        "a table must be a Module: {found:?}"
    );
    let listen = found
        .iter()
        .find(|(label, name, _)| *label == NodeLabel::Field && name == "listen")
        .expect("listen must be a Field");
    assert!(
        listen.2.ends_with("server.listen"),
        "a key must be qualified by its table, got {}",
        listen.2
    );
    assert!(
        found
            .iter()
            .any(|(label, name, _)| *label == NodeLabel::Module && name == "database.primary"),
        "a dotted table key must survive as written"
    );
}

/// A workflow's structure is nesting, so the qualified name is the only thing
/// that distinguishes one `runs-on` from another.
#[test]
fn a_yaml_workflow_keeps_its_nesting_in_qualified_names() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        ".github/workflows/ci.yml",
        "name: CI\njobs:\n  build:\n    runs-on: ubuntu-latest\n  release:\n    runs-on: macos-14\n",
    );

    let report = index(root.path(), "cfg-yaml");
    assert_eq!(report.languages.get("yaml").copied(), Some(1));

    let qualified: Vec<String> = nodes("cfg-yaml")
        .into_iter()
        .filter(|(label, _, _)| *label == NodeLabel::Field)
        .map(|(_, _, q)| q)
        .collect();

    for expected in ["jobs.build.runs-on", "jobs.release.runs-on"] {
        assert!(
            qualified.iter().any(|q| q.ends_with(expected)),
            "{expected} must be distinguishable: {qualified:?}"
        );
    }
}

/// A workflow lives under `.github`, which the hidden-file rule used to prune,
/// so parsing YAML alone would have delivered nothing. `.git` must stay out.
#[test]
fn tracked_dot_directories_are_walked_but_the_git_directory_is_not() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        ".github/workflows/ci.yml",
        "name: CI\non:\n  push: {}\n",
    );
    write(
        root.path(),
        ".git/config",
        "[core]\n\trepositoryformatversion = 0\n",
    );
    write(root.path(), ".venv/pyvenv.cfg", "[venv]\nversion = 3\n");
    // A tracked dotted file is deliberate project content, unlike a dotted
    // directory full of machinery.
    write(root.path(), ".air.toml", "[build]\ncmd = \"go build\"\n");
    // Ignore files steer the walk; listing them as unindexable is noise.
    write(root.path(), ".cursorignore", "build/\n");

    let report = index(root.path(), "cfg-hidden");
    assert!(
        !report
            .skipped_examples
            .iter()
            .any(|e| e.path.ends_with("ignore")),
        "an ignore file must not appear in coverage at all: {:?}",
        report.skipped_examples
    );

    let paths: Vec<String> = nodes("cfg-hidden")
        .into_iter()
        .map(|(_, _, qualified)| qualified)
        .collect();

    assert!(
        paths.iter().any(|q| q.contains("github.workflows.ci")),
        "a CI workflow is tracked source and must be indexed: {paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|q| q.contains("air") && q.contains("build")),
        "a tracked dotted config file must be indexed: {paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|q| q.contains("git.config") || q.contains("venv")),
        "internal dot directories must stay pruned: {paths:?}"
    );
}

/// Config formats have no language server, so advertising them as LSP-eligible
/// would promise resolution that can never happen.
#[test]
fn config_formats_are_parsed_but_never_lsp_eligible() {
    use loci_core::LanguageId;
    for language in [LanguageId::Toml, LanguageId::Yaml, LanguageId::Ini] {
        assert!(loci_parse::is_bundled(language), "{language} must parse");
        assert!(
            !language.hybrid_lsp_eligible(),
            "{language} must not claim LSP support"
        );
    }
}
