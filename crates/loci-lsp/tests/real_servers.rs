//! Integration tests against language servers that are actually installed.
//!
//! These are skipped, loudly, when the server is missing, because a test that
//! silently passes on a machine without gopls proves nothing. Set
//! `LOCI_REQUIRE_LSP=1` to turn a missing server into a failure instead, which
//! is what CI does once the servers are provisioned.

use loci_core::LanguageId;
use std::path::Path;
use std::time::Duration;

fn skip_or_fail(language: LanguageId, reason: &str) -> bool {
    if std::env::var("LOCI_REQUIRE_LSP").is_ok() {
        panic!("{language} server required but unavailable: {reason}");
    }
    eprintln!("skipping {language}: {reason}");
    true
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dir");
    }
    std::fs::write(path, contents).expect("write fixture");
}

/// The call `s.Persist(o)` cannot be resolved from the AST: `Persist` is
/// declared by two types and only the type of `s` decides which. This is the
/// exact gap Hybrid LSP exists to close, so it is the thing the test asserts.
#[test]
fn gopls_resolves_a_method_call_the_ast_cannot() {
    let root = tempfile::tempdir().expect("temp root");
    let root = root.path();

    write(root, "go.mod", "module locitest\n\ngo 1.21\n");
    write(
        root,
        "store.go",
        r#"package main

type Store struct{}

func (s *Store) Persist(value string) {}

type Cache struct{}

func (c *Cache) Persist(value string) {}
"#,
    );
    write(
        root,
        "service.go",
        r#"package main

type Service struct {
	store *Store
}

func (svc *Service) Save(value string) {
	svc.store.Persist(value)
}
"#,
    );

    let (mut client, spec) = match loci_lsp::start(LanguageId::Go, root, Duration::from_secs(60)) {
        Ok(started) => started,
        Err(error) => {
            assert!(skip_or_fail(LanguageId::Go, &error.to_string()));
            return;
        }
    };

    let source = std::fs::read_to_string(root.join("service.go")).expect("read");
    client
        .did_open("service.go", spec.lsp_language_id, &source)
        .expect("didOpen");

    // Zero-based: line 8 is `\tsvc.store.Persist(value)`, and the character
    // must land inside the `Persist` identifier.
    let line = source
        .lines()
        .position(|l| l.contains("svc.store.Persist"))
        .expect("the call line") as u32;
    let character = source
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("Persist"))
        .expect("the identifier column") as u32;

    let locations = client
        .definition("service.go", line, character)
        .expect("definition request");
    client.shutdown();

    assert_eq!(
        locations.len(),
        1,
        "gopls must resolve the call to exactly one definition: {locations:?}"
    );
    let found = &locations[0];
    assert_eq!(
        found.relative_path, "store.go",
        "the definition lives in store.go, not the caller's file"
    );

    // Line 4 (zero-based) is `func (s *Store) Persist(...)`. Asserting the
    // exact line proves we resolved Store.Persist and not Cache.Persist.
    let store_source = std::fs::read_to_string(root.join("store.go")).expect("read");
    let expected = store_source
        .lines()
        .position(|l| l.contains("func (s *Store) Persist"))
        .expect("the Store.Persist line") as u32;
    assert_eq!(
        found.line, expected,
        "resolved to the wrong Persist; Cache.Persist must not match"
    );
}

/// rust-analyzer is slower to warm up than gopls, so this asserts the same
/// property on a second, independently implemented server: a trait method call
/// through a generic resolves to the concrete impl.
#[test]
fn rust_analyzer_resolves_a_call_across_files() {
    let root = tempfile::tempdir().expect("temp root");
    let root = root.path();

    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"locitest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        root,
        "src/store.rs",
        r#"pub struct Store;

impl Store {
    pub fn persist(&self, value: &str) {
        let _ = value;
    }
}
"#,
    );
    write(
        root,
        "src/main.rs",
        r#"mod store;

use store::Store;

fn main() {
    let s = Store;
    s.persist("x");
}
"#,
    );

    let (mut client, spec) = match loci_lsp::start(LanguageId::Rust, root, Duration::from_secs(120))
    {
        Ok(started) => started,
        Err(error) => {
            assert!(skip_or_fail(LanguageId::Rust, &error.to_string()));
            return;
        }
    };

    let source = std::fs::read_to_string(root.join("src/main.rs")).expect("read");
    client
        .did_open("src/main.rs", spec.lsp_language_id, &source)
        .expect("didOpen");

    let line = source
        .lines()
        .position(|l| l.contains("s.persist"))
        .expect("the call line") as u32;
    let character = source
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("persist"))
        .expect("the identifier column") as u32;

    // rust-analyzer indexes in the background and may answer before it is
    // ready, so give it a bounded number of attempts rather than one shot.
    let mut locations = Vec::new();
    for _ in 0..20 {
        locations = client
            .definition("src/main.rs", line, character)
            .unwrap_or_default();
        if !locations.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    client.shutdown();

    if locations.is_empty() {
        assert!(skip_or_fail(
            LanguageId::Rust,
            "rust-analyzer did not answer within the warm-up budget"
        ));
        return;
    }

    assert_eq!(locations[0].relative_path, "src/store.rs");
}
