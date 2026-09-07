//! HTML was the largest single group a real repository reported as
//! unsupported_language: nineteen files in one Go and React project.
//!
//! What it contributes is deliberately narrow. A document exposes names
//! through `id`, and it pulls in files through `src` and `href`. Everything
//! else on a page is layout, and layout is not something the graph can answer
//! questions about.

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

/// The entry point of the React project this was measured on, unchanged.
const ENTRY: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <link rel="icon" type="image/svg+xml" href="/favicon.svg" />
    <title>GoInfraCloud</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
"#;

#[test]
fn an_html_file_is_recognised_and_parses_cleanly() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "index.html", ENTRY);

    let report = index(root.path(), "markup-basic");

    assert_eq!(
        report.languages.get("html").copied(),
        Some(1),
        "html must be a recognised language: {:?}",
        report.languages
    );
    assert_eq!(report.files_parse_partial, 0, "html must parse cleanly");
}

#[test]
fn an_element_with_an_id_becomes_a_named_node() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "index.html", ENTRY);
    index(root.path(), "markup-ids");

    let found = nodes("markup-ids");

    assert!(
        found
            .iter()
            .any(|(label, name, qualified)| *label == NodeLabel::Field
                && name == "root"
                && qualified == "root"),
        "the mount point must be addressable by its id: {found:?}"
    );
}

/// The point of indexing the entry point at all: it is the one file that says
/// which module starts the application.
#[test]
fn a_script_src_becomes_an_import_edge_to_the_file_it_names() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "index.html", ENTRY);
    write(
        root.path(),
        "src/main.tsx",
        "export const start = () => 1\n",
    );
    index(root.path(), "markup-entry");

    let imports = edges("markup-entry", EdgeType::Imports);

    assert!(
        imports.contains(&("index.html".to_string(), "main.tsx".to_string())),
        "index.html must reach the file it boots: {imports:?}"
    );
}

/// Everything a page links to that is not a file in the project. Each of these
/// would otherwise become an edge to a node that cannot exist.
#[test]
fn links_that_do_not_name_a_project_file_are_not_imports() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "mail.html",
        r##"<html>
  <body>
    <script src="https://cdn.example.com/a.js"></script>
    <script src="//cdn.example.com/b.js"></script>
    <script src="{{ .ScriptURL }}"></script>
    <img src="data:image/png;base64,iVBOR" />
    <iframe src="#section"></iframe>
  </body>
</html>
"##,
    );

    let report = index(root.path(), "markup-links");

    assert_eq!(report.files_parse_partial, 0);
    assert!(
        edges("markup-links", EdgeType::Imports).is_empty(),
        "none of those name a file this project ships"
    );
}

/// The templates that made up seventeen of the nineteen skipped files are Go
/// templates, not documents: `{{ }}` throughout, and sometimes no `<html>` at
/// all. The grammar reads the actions as text, which is the right answer here.
#[test]
fn a_go_template_is_parsed_rather_than_skipped() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "templates/snmp_alert.html",
        r#"{{/* Data: telegram.TrapAlertData */}}
{{- if eq .Severity "critical" -}}
<b>SNMP TRAP ALERT — {{ .Severity | upper }}</b>
{{- end -}}
<a href="{{ .DashboardURL }}">Open the dashboard</a>
"#,
    );

    let report = index(root.path(), "markup-template");

    assert_eq!(
        report.languages.get("html").copied(),
        Some(1),
        "a fragment is still html: {:?}",
        report.languages
    );
    assert_eq!(
        report.files_parse_partial, 0,
        "template actions are text, not syntax errors"
    );
}

/// `<a href>` is navigation. Importing it would have added 144 edges on the
/// project this was measured on, every one of them to a URL or a template
/// expression.
#[test]
fn an_anchor_is_not_an_import_even_when_it_names_a_real_file() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "index.html",
        "<html><body><a href=\"other.html\">next</a></body></html>\n",
    );
    write(
        root.path(),
        "other.html",
        "<html><body>done</body></html>\n",
    );

    index(root.path(), "markup-anchor");

    assert!(
        edges("markup-anchor", EdgeType::Imports).is_empty(),
        "a link between pages is not a dependency"
    );
}
