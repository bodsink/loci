//! Turning an import as written into the repository path it names.
//!
//! Source rarely spells an import the way the graph stores a file. TypeScript
//! writes `@/types`, Dart writes `package:app/models/user.dart`, and both mean
//! a path that only a project manifest can reveal. Matching the literal text
//! against file names therefore resolves almost nothing: in a real project
//! measured here, a module imported by 941 files had an in-degree of zero.
//!
//! This module reads the manifests that define those spellings — `tsconfig.json`
//! for path aliases, `pubspec.yaml` for a Dart package name — and turns an
//! import into candidate repository paths. It deliberately stops there. Deciding
//! which candidate is a file the index actually holds belongs to the caller,
//! which is the only party that knows what was indexed.

use std::collections::BTreeMap;
use std::path::Path;

/// A `tsconfig.json`, reduced to what decides where an import points.
#[derive(Debug, Clone)]
struct TsProject {
    /// Directory holding the manifest, repository-relative. Empty at the root.
    dir: String,
    /// Alias patterns in declaration order, each mapping to one or more
    /// substitution targets, already resolved against `baseUrl`.
    aliases: Vec<TsAlias>,
}

/// One entry of `compilerOptions.paths`.
///
/// TypeScript allows a single `*` in the pattern; the text it matches is
/// substituted into the `*` of each target.
#[derive(Debug, Clone)]
struct TsAlias {
    prefix: String,
    suffix: String,
    wildcard: bool,
    targets: Vec<String>,
}

/// A Dart package, which owns the `package:<name>/...` spelling.
#[derive(Debug, Clone)]
struct DartPackage {
    name: String,
    /// Directory `package:` URIs resolve against, repository-relative.
    lib_dir: String,
}

/// A Go module, which prefixes the import path of every package inside it.
#[derive(Debug, Clone)]
struct GoModule {
    /// The `module` line of `go.mod`, e.g. `github.com/bodsink/olt-management`.
    path: String,
    /// Directory holding `go.mod`, repository-relative. Empty at the root.
    dir: String,
}

/// Import spellings a repository's manifests make meaningful.
#[derive(Debug, Default, Clone)]
pub struct ImportResolver {
    ts_projects: Vec<TsProject>,
    dart_packages: Vec<DartPackage>,
    go_modules: Vec<GoModule>,
}

/// How deep to look for manifests.
///
/// Manifests sit at a project root, so they are shallow even in a monorepo
/// (`packages/web/tsconfig.json` is depth three). The limit keeps discovery
/// off deep vendored trees that no ignore file happened to exclude.
const MANIFEST_DEPTH: usize = 6;

/// `extends` chains are short in practice; the limit only stops a cycle.
const MAX_EXTENDS: usize = 8;

impl ImportResolver {
    /// Read every manifest under `root` that changes how imports are spelled.
    ///
    /// A manifest that cannot be read or parsed is skipped rather than failing
    /// the index: the imports it would have explained stay unresolved, which is
    /// the same outcome as before it existed.
    pub fn discover(root: &Path) -> Self {
        let mut resolver = Self::default();

        let walker = ignore::WalkBuilder::new(root)
            .max_depth(Some(MANIFEST_DEPTH))
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .build();

        for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Some(name) = entry.file_name().to_str() else {
                continue;
            };
            let path = entry.path();
            let Some(relative) = relative_to(root, path) else {
                continue;
            };
            let dir = parent_dir(&relative);

            match name {
                "tsconfig.json" | "jsconfig.json" => {
                    if let Some(project) = read_ts_project(path, &dir) {
                        resolver.ts_projects.push(project);
                    }
                }
                "pubspec.yaml" => {
                    if let Some(package) = read_dart_package(path, &dir) {
                        resolver.dart_packages.push(package);
                    }
                }
                "go.mod" => {
                    if let Some(module) = read_go_module(path, &dir) {
                        resolver.go_modules.push(module);
                    }
                }
                _ => {}
            }
        }

        // Deepest manifest first, so a nested project wins over the one that
        // merely contains it.
        resolver
            .ts_projects
            .sort_by_key(|p| std::cmp::Reverse(p.dir.len()));
        resolver
            .dart_packages
            .sort_by_key(|p| std::cmp::Reverse(p.lib_dir.len()));
        // Longest module path first: a nested module's packages are not part of
        // the module that merely contains its directory.
        resolver
            .go_modules
            .sort_by_key(|m| std::cmp::Reverse(m.path.len()));
        resolver
    }

    /// Whether any manifest was found. Used to skip work, not to claim coverage.
    pub fn is_empty(&self) -> bool {
        self.ts_projects.is_empty() && self.dart_packages.is_empty() && self.go_modules.is_empty()
    }

    /// Repository paths `target`, written in `from_file`, could name.
    ///
    /// Returns candidates most specific first, and an empty vector when the
    /// import is one this resolver has no manifest for — a third-party package,
    /// a Dart SDK URI, a bare Node specifier. Empty means unknown, not absent.
    pub fn candidates(&self, from_file: &str, target: &str) -> Vec<String> {
        if target.starts_with("./") || target.starts_with("../") {
            return join_relative(&parent_dir(from_file), target)
                .into_iter()
                .collect();
        }

        if let Some(rest) = target.strip_prefix("package:") {
            return self.dart_candidates(rest);
        }

        let aliased = self.alias_candidates(from_file, target);
        if !aliased.is_empty() {
            return aliased;
        }

        self.go_candidates(target)
    }

    /// The import path a Go package directory answers to, if any module owns it.
    ///
    /// The inverse of `go_candidates`, and the reason a package node can be
    /// named the way source refers to it rather than by its directory.
    pub fn go_import_path(&self, directory: &str) -> Option<String> {
        // The innermost module owns the directory. A repository can nest one
        // module inside another's tree, and the outer one does not build it.
        let module = self
            .go_modules
            .iter()
            .filter(|m| covers(&m.dir, directory) || m.dir == directory)
            .max_by_key(|m| m.dir.len())?;

        let rest = if module.dir.is_empty() {
            directory
        } else if module.dir == directory {
            ""
        } else {
            directory.strip_prefix(&format!("{}/", module.dir))?
        };

        Some(if rest.is_empty() {
            module.path.clone()
        } else {
            format!("{}/{}", module.path, rest)
        })
    }

    /// A Go import path against the modules this repository defines.
    ///
    /// The result is a *directory*, because a Go import names a package rather
    /// than a file. Turning that into files is the caller's job.
    fn go_candidates(&self, target: &str) -> Vec<String> {
        for module in &self.go_modules {
            let rest = if target == module.path {
                ""
            } else if let Some(rest) = target.strip_prefix(&format!("{}/", module.path)) {
                rest
            } else {
                continue;
            };
            let joined = if module.dir.is_empty() {
                rest.to_string()
            } else if rest.is_empty() {
                module.dir.clone()
            } else {
                format!("{}/{}", module.dir, rest)
            };
            return normalise(&joined).into_iter().collect();
        }
        Vec::new()
    }

    /// `package:<name>/<path>` against the packages this repository defines.
    ///
    /// A `package:` URI naming someone else's package resolves to nothing here,
    /// which is correct: its source is not in the repository.
    fn dart_candidates(&self, rest: &str) -> Vec<String> {
        let Some((name, path)) = rest.split_once('/') else {
            return Vec::new();
        };
        self.dart_packages
            .iter()
            .filter(|p| p.name == name)
            .filter_map(|p| normalise(&format!("{}/{}", p.lib_dir, path)))
            .collect()
    }

    /// Alias patterns from the nearest enclosing `tsconfig.json`.
    fn alias_candidates(&self, from_file: &str, target: &str) -> Vec<String> {
        let mut out = Vec::new();
        for project in &self.ts_projects {
            if !covers(&project.dir, from_file) {
                continue;
            }
            for alias in &project.aliases {
                out.extend(alias.apply(target).iter().filter_map(|t| {
                    normalise(&if project.dir.is_empty() {
                        t.clone()
                    } else {
                        format!("{}/{}", project.dir, t)
                    })
                }));
            }
            // The nearest project that defines any alias decides; an outer
            // manifest is a different compilation with different mappings.
            if !out.is_empty() {
                break;
            }
        }
        out
    }
}

impl TsAlias {
    /// Substitute `target` into this alias, or nothing when it does not match.
    fn apply(&self, target: &str) -> Vec<String> {
        if !self.wildcard {
            if target != self.prefix {
                return Vec::new();
            }
            return self.targets.clone();
        }

        // `@/*` must not swallow `@/`: TypeScript requires the `*` to match at
        // least the empty string, but a bare prefix is a different specifier.
        let Some(rest) = target.strip_prefix(&self.prefix) else {
            return Vec::new();
        };
        let Some(matched) = rest.strip_suffix(&self.suffix) else {
            return Vec::new();
        };
        if matched.is_empty() && !self.suffix.is_empty() {
            return Vec::new();
        }

        self.targets
            .iter()
            .map(|t| t.replacen('*', matched, 1))
            .collect()
    }
}

/// Parse a `tsconfig.json`, following `extends` for inherited path mappings.
fn read_ts_project(path: &Path, dir: &str) -> Option<TsProject> {
    let mut options = BTreeMap::new();
    collect_ts_options(path, &mut options, 0);

    let base_url = options
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    let paths = options.get("paths").and_then(|v| v.as_object())?;

    let mut aliases = Vec::new();
    for (pattern, targets) in paths {
        let Some(list) = targets.as_array() else {
            continue;
        };
        let resolved: Vec<String> = list
            .iter()
            .filter_map(|t| t.as_str())
            .map(|t| format!("{base_url}/{t}"))
            .collect();
        if resolved.is_empty() {
            continue;
        }
        match pattern.split_once('*') {
            Some((prefix, suffix)) => aliases.push(TsAlias {
                prefix: prefix.to_string(),
                suffix: suffix.to_string(),
                wildcard: true,
                targets: resolved,
            }),
            None => aliases.push(TsAlias {
                prefix: pattern.clone(),
                suffix: String::new(),
                wildcard: false,
                targets: resolved,
            }),
        }
    }

    if aliases.is_empty() {
        return None;
    }
    // Longest literal prefix first, so `@/lib/*` beats `@/*`.
    aliases.sort_by_key(|a| std::cmp::Reverse(a.prefix.len()));
    Some(TsProject {
        dir: dir.to_string(),
        aliases,
    })
}

/// Merge `compilerOptions` from `path` and whatever it extends.
///
/// A child's own value wins, so the base is read first and then overwritten.
fn collect_ts_options(path: &Path, into: &mut BTreeMap<String, serde_json::Value>, depth: usize) {
    if depth >= MAX_EXTENDS {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else {
        return;
    };

    if let Some(base) = root.get("extends").and_then(|v| v.as_str()) {
        // Only relative extends can be followed; a package name lives in
        // node_modules, which is not indexed and may not be installed.
        if base.starts_with('.') {
            let mut candidate = path.parent().unwrap_or(Path::new(".")).join(base);
            if candidate.extension().is_none() {
                candidate.set_extension("json");
            }
            collect_ts_options(&candidate, into, depth + 1);
        }
    }

    if let Some(options) = root.get("compilerOptions").and_then(|v| v.as_object()) {
        for (key, value) in options {
            into.insert(key.clone(), value.clone());
        }
    }
}

/// Strip comments and trailing commas so `serde_json` accepts a tsconfig.
///
/// tsconfig files are JSONC: the TypeScript compiler allows `//` and `/* */`
/// comments and a trailing comma, and real ones use them. Characters are
/// replaced with spaces rather than removed so any parse error still reports a
/// usable offset.
fn strip_jsonc(text: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        Str,
        Line,
        Block,
    }

    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut state = State::Code;
    let mut escaped = false;
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        match state {
            State::Code => match (b, next) {
                (b'/', Some(b'/')) => {
                    state = State::Line;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                (b'/', Some(b'*')) => {
                    state = State::Block;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                (b'"', _) => {
                    state = State::Str;
                    out.push(b);
                }
                _ => out.push(b),
            },
            State::Str => {
                out.push(b);
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    state = State::Code;
                }
            }
            State::Line => {
                if b == b'\n' {
                    state = State::Code;
                    out.push(b);
                } else {
                    out.push(b' ');
                }
            }
            State::Block => {
                if b == b'*' && next == Some(b'/') {
                    state = State::Code;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                out.push(if b == b'\n' { b'\n' } else { b' ' });
            }
        }
        i += 1;
    }

    let mut text = String::from_utf8(out).unwrap_or_default();
    drop_trailing_commas(&mut text);
    text
}

/// Blank a comma that is followed only by whitespace and a closing bracket.
fn drop_trailing_commas(text: &mut String) {
    let bytes = text.clone().into_bytes();
    let mut out = bytes.clone();
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        if b == b'"' {
            in_string = true;
            continue;
        }
        if b != b',' {
            continue;
        }
        let trailing = bytes[i + 1..]
            .iter()
            .find(|c| !c.is_ascii_whitespace())
            .is_some_and(|c| *c == b'}' || *c == b']');
        if trailing {
            out[i] = b' ';
        }
    }

    *text = String::from_utf8(out).unwrap_or_default();
}

/// Read the package name a `pubspec.yaml` declares.
///
/// Only the top-level `name` matters for import resolution, and it is a plain
/// scalar, so this reads lines rather than taking on a YAML dependency.
fn read_dart_package(path: &Path, dir: &str) -> Option<DartPackage> {
    let text = std::fs::read_to_string(path).ok()?;
    let name = text.lines().find_map(|line| {
        // A nested `name:` is indented and belongs to a dependency entry.
        if line.starts_with(char::is_whitespace) {
            return None;
        }
        let value = line.strip_prefix("name:")?.trim();
        let value = value.trim_matches(['"', '\'']);
        (!value.is_empty()).then(|| value.to_string())
    })?;

    let lib_dir = if dir.is_empty() {
        "lib".to_string()
    } else {
        format!("{dir}/lib")
    };
    Some(DartPackage { name, lib_dir })
}

/// Read the module path a `go.mod` declares.
///
/// Only the `module` directive matters here, and it is a single unquoted token
/// on its own line, so the file is read as lines rather than parsed.
fn read_go_module(path: &Path, dir: &str) -> Option<GoModule> {
    let text = std::fs::read_to_string(path).ok()?;
    let module = text.lines().find_map(|line| {
        let value = line.strip_prefix("module ")?.trim();
        let value = value.trim_matches('"');
        (!value.is_empty()).then(|| value.to_string())
    })?;
    Some(GoModule {
        path: module,
        dir: dir.to_string(),
    })
}

/// Path of `path` relative to `root`, in the forward-slash form the graph uses.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let text = relative.to_str()?;
    Some(text.replace('\\', "/"))
}

/// Whether `dir` contains `file`. An empty `dir` is the repository root.
fn covers(dir: &str, file: &str) -> bool {
    dir.is_empty() || file.starts_with(&format!("{dir}/"))
}

/// Directory part of a repository-relative path, empty at the root.
fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

/// Resolve `target` against `dir`, both repository-relative.
fn join_relative(dir: &str, target: &str) -> Option<String> {
    normalise(&if dir.is_empty() {
        target.to_string()
    } else {
        format!("{dir}/{target}")
    })
}

/// Collapse `.` and `..` segments, rejecting a path that climbs out of the
/// repository. Escaping the root means the import leaves indexed territory,
/// and inventing a path for it would be a guess.
fn normalise(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(dir: &str, pattern: &str, target: &str) -> ImportResolver {
        ImportResolver {
            ts_projects: vec![TsProject {
                dir: dir.to_string(),
                aliases: vec![TsAlias {
                    prefix: pattern.trim_end_matches('*').to_string(),
                    suffix: String::new(),
                    wildcard: pattern.contains('*'),
                    targets: vec![target.to_string()],
                }],
            }],
            ..Default::default()
        }
    }

    fn go(modules: &[(&str, &str)]) -> ImportResolver {
        let mut resolver = ImportResolver {
            go_modules: modules
                .iter()
                .map(|(path, dir)| GoModule {
                    path: path.to_string(),
                    dir: dir.to_string(),
                })
                .collect(),
            ..Default::default()
        };
        resolver
            .go_modules
            .sort_by_key(|m| std::cmp::Reverse(m.path.len()));
        resolver
    }

    #[test]
    fn relative_import_resolves_against_the_importing_directory() {
        let resolver = ImportResolver::default();
        assert_eq!(
            resolver.candidates("frontend/src/pages/Home.tsx", "../lib/axios"),
            vec!["frontend/src/lib/axios".to_string()]
        );
        assert_eq!(
            resolver.candidates("frontend/src/pages/Home.tsx", "./Header"),
            vec!["frontend/src/pages/Header".to_string()]
        );
    }

    /// The old resolver dotted the target and compared names, so a relative
    /// import in a subdirectory could only ever match a file at the root.
    #[test]
    fn a_relative_import_keeps_its_directory() {
        let resolver = ImportResolver::default();
        let resolved = resolver.candidates("a/b/c.ts", "./d");
        assert_eq!(resolved, vec!["a/b/d".to_string()]);
    }

    #[test]
    fn climbing_out_of_the_repository_resolves_to_nothing() {
        let resolver = ImportResolver::default();
        assert!(resolver.candidates("src/a.ts", "../../outside").is_empty());
    }

    #[test]
    fn alias_substitutes_the_wildcard_under_the_manifest_directory() {
        let resolver = ts("frontend", "@/*", "./src/*");
        assert_eq!(
            resolver.candidates("frontend/src/App.tsx", "@/types"),
            vec!["frontend/src/types".to_string()]
        );
    }

    #[test]
    fn alias_does_not_apply_outside_its_project() {
        let resolver = ts("frontend", "@/*", "./src/*");
        assert!(resolver
            .candidates("mobile/lib/main.dart", "@/types")
            .is_empty());
    }

    #[test]
    fn a_bare_third_party_import_resolves_to_nothing() {
        let resolver = ts("frontend", "@/*", "./src/*");
        assert!(resolver
            .candidates("frontend/src/App.tsx", "react")
            .is_empty());
    }

    #[test]
    fn dart_package_uri_resolves_into_the_lib_directory() {
        let resolver = ImportResolver {
            dart_packages: vec![DartPackage {
                name: "goinfracloud".to_string(),
                lib_dir: "mobile/lib".to_string(),
            }],
            ..Default::default()
        };
        assert_eq!(
            resolver.candidates(
                "mobile/lib/app.dart",
                "package:goinfracloud/models/user.dart"
            ),
            vec!["mobile/lib/models/user.dart".to_string()]
        );
        assert!(resolver
            .candidates("mobile/lib/app.dart", "package:flutter/material.dart")
            .is_empty());
    }

    #[test]
    fn a_go_import_resolves_to_the_package_directory() {
        let resolver = go(&[("github.com/bodsink/olt-management", "backend")]);
        assert_eq!(
            resolver.candidates(
                "backend/cmd/api/main.go",
                "github.com/bodsink/olt-management/internal/handlers"
            ),
            vec!["backend/internal/handlers".to_string()]
        );
    }

    /// This repository has four `go.mod` files. An import must land in the
    /// module that declares it, not in whichever one was read first.
    #[test]
    fn the_owning_module_wins_in_a_multi_module_repository() {
        let resolver = go(&[
            ("github.com/bodsink/olt-management", "backend"),
            ("github.com/bodsink/edge-gateway", "edge-gateway"),
            ("github.com/bodsink/goinfracloud/proto", "proto"),
        ]);
        assert_eq!(
            resolver.candidates("x/y.go", "github.com/bodsink/edge-gateway/tunnel"),
            vec!["edge-gateway/tunnel".to_string()]
        );
        assert_eq!(
            resolver.candidates("x/y.go", "github.com/bodsink/goinfracloud/proto/v1"),
            vec!["proto/v1".to_string()]
        );
    }

    #[test]
    fn the_standard_library_and_third_party_go_imports_resolve_to_nothing() {
        let resolver = go(&[("github.com/bodsink/olt-management", "backend")]);
        assert!(resolver.candidates("backend/main.go", "fmt").is_empty());
        assert!(resolver
            .candidates("backend/main.go", "github.com/gin-gonic/gin")
            .is_empty());
    }

    /// A module path that is a prefix of another as a *string* but not as a
    /// path segment must not match.
    #[test]
    fn a_partial_segment_match_is_not_a_module() {
        let resolver = go(&[("github.com/bodsink/olt", "olt")]);
        assert!(resolver
            .candidates("x/y.go", "github.com/bodsink/olt-management/internal")
            .is_empty());
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_are_tolerated() {
        let text = r#"{
  // leading
  "compilerOptions": {
    /* block */
    "paths": { "@/*": ["./src/*"], },
  },
}"#;
        let value: serde_json::Value =
            serde_json::from_str(&strip_jsonc(text)).expect("jsonc parses");
        assert!(value["compilerOptions"]["paths"]["@/*"].is_array());
    }

    /// A comment marker inside a string is data, not a comment.
    #[test]
    fn a_url_in_a_string_survives_comment_stripping() {
        let text = r#"{"a": "https://example.com/x", "b": 1}"#;
        let value: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).expect("parses");
        assert_eq!(value["a"], "https://example.com/x");
    }

    #[test]
    fn stripping_preserves_byte_offsets() {
        let text = "{\n  // c\n  \"a\": 1\n}";
        assert_eq!(strip_jsonc(text).len(), text.len());
    }

    #[test]
    fn a_longer_alias_prefix_is_tried_first() {
        let mut resolver = ts("", "@/*", "./src/*");
        resolver.ts_projects[0].aliases.insert(
            0,
            TsAlias {
                prefix: "@/lib/".to_string(),
                suffix: String::new(),
                wildcard: true,
                targets: vec!["./vendor/*".to_string()],
            },
        );
        resolver.ts_projects[0]
            .aliases
            .sort_by_key(|a| std::cmp::Reverse(a.prefix.len()));
        let resolved = resolver.candidates("app.ts", "@/lib/axios");
        assert_eq!(resolved.first().map(String::as_str), Some("vendor/axios"));
    }
}
