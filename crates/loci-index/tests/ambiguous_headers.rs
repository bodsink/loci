//! `.h` says nothing about whether a file is C or C++.
//!
//! Mapping the extension straight to C shredded every C++ header: a real Qt
//! project reported 59 of its 245 files as `parse_partial`, and a 19-line
//! header produced 10 parse errors under the C grammar and none under C++.
//! The indexer now picks by measured error count, so this asserts both
//! directions — C++ headers stop being partial, and plain C headers stay on C.

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

/// Reduced from `desktop-qt/src/core/saved_router.h` in the project that
/// exposed this: a default member initialiser, an inline `const` method and a
/// static factory. None of it is C.
const QT_STYLE_HEADER: &str = r#"#pragma once

#include <QString>
#include <QJsonObject>

struct SavedRouter {
    QString id;
    QString kind = QStringLiteral("ip");

    bool isMac() const { return kind == QStringLiteral("mac"); }

    QJsonObject toJson() const;
    static SavedRouter fromJson(const QJsonObject &obj);
};
"#;

#[test]
fn a_cpp_header_named_dot_h_is_not_reported_as_partial() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(root.path(), "src/saved_router.h", QT_STYLE_HEADER);

    let report = index(root.path(), "hdr-cpp");

    assert!(
        report.files_parse_partial == 0,
        "a C++ header must not be left partial: {:?}",
        report.parse_partial_examples
    );

    // And the symbols must actually be in the graph, not merely error-free.
    let (_, store) = loci_index::open_project("hdr-cpp").expect("open");
    let names: Vec<String> = store
        .read()
        .expect("read")
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "SavedRouter"),
        "the struct must reach the graph: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "isMac"),
        "an inline const method must reach the graph: {names:?}"
    );
}

/// The fallback must not drag genuine C onto the C++ grammar. `new` and
/// `class` are ordinary identifiers in C and reserved words in C++, so a C
/// header using them parses cleanly only as C.
#[test]
fn a_c_header_using_cpp_keywords_as_identifiers_stays_on_c() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "src/alloc.h",
        "struct pool { int size; };\nvoid *new(struct pool *class);\n",
    );

    let report = index(root.path(), "hdr-c");

    assert!(
        report.files_parse_partial == 0,
        "a plain C header must not be left partial: {:?}",
        report.parse_partial_examples
    );
    assert_eq!(
        report.languages.get("c").copied(),
        Some(1),
        "the file must still be counted as C, not reclassified: {:?}",
        report.languages
    );
}

/// Reduced from `desktop-qt/src/ui/modules/interface_detail.h`, which collapsed
/// with errors reported over lines 54-217. Raw C++ scored worse on it than C,
/// so an implementation that only rewrites the already-winning language never
/// reaches the rewrite, and the whole file stays lost.
#[test]
fn a_qt_header_that_scores_worse_as_raw_cpp_than_as_c_is_still_recovered() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "src/interface_detail.h",
        r#"#pragma once

#include <QWidget>

class InterfaceDetail : public QWidget {
    Q_OBJECT
    Q_PROPERTY(int mtu READ mtu NOTIFY mtuChanged)

public:
    explicit InterfaceDetail(QWidget *parent = nullptr);
    Q_INVOKABLE bool mtuValueValid(int mtu) const;

signals:
    void mtuChanged(int mtu);

public slots:
    void reload(const QString &iface = {});

private:
    int mtu_ = 0;
};
"#,
    );

    let report = index(root.path(), "hdr-qt");

    assert!(
        report.files_parse_partial == 0,
        "a Qt header must be recovered: {:?}",
        report.parse_partial_examples
    );

    let (_, store) = loci_index::open_project("hdr-qt").expect("open");
    let names: Vec<String> = store
        .read()
        .expect("read")
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| n.name.clone())
        .collect();
    for expected in ["InterfaceDetail", "mtuValueValid", "reload"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} must reach the graph, including members declared after \
             the Qt macros: {names:?}"
        );
    }
}

/// A header that neither grammar can fully parse must still be reported as
/// partial rather than quietly presented as clean.
#[test]
fn a_header_neither_grammar_understands_is_still_reported_partial() {
    let _guard = serial();
    let root = tempfile::tempdir().expect("temp");
    write(
        root.path(),
        "src/broken.h",
        "struct S { ( ) ] } garbage;;;\n",
    );

    let report = index(root.path(), "hdr-broken");

    assert_eq!(
        report.files_parse_partial, 1,
        "unparseable input must stay visible as partial: {:?}",
        report.parse_partial_examples
    );
}
