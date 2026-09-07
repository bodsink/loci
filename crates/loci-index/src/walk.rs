use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use loci_core::{LanguageId, Sandbox, MAX_FILE_BYTES};
use std::path::{Path, PathBuf};

/// A file the walker decided to look at, with the reason if it will not be parsed.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Repository-relative path with `/` separators.
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub size: u64,
    pub language: Option<LanguageId>,
}

/// Walk the project root honouring `.gitignore`, `.ignore` and hidden-file rules.
///
/// Ignored directories are pruned rather than visited, so a repository with a
/// large `node_modules` costs nothing. That also means ignored files are never
/// enumerated; classify one on demand with [`IgnoreMatcher`].
pub fn collect(sandbox: &Sandbox) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    let walker = WalkBuilder::new(sandbox.root())
        // Dotted paths are filtered below rather than by `hidden`, so that a
        // few tracked configuration directories can be let back in.
        .hidden(false)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            // Never judge the project root by its own name; it may well be
            // inside a dotted directory.
            if entry.depth() == 0 || !name.starts_with('.') {
                return true;
            }
            if !entry.file_type().is_some_and(|t| t.is_dir()) {
                // An ignore file is an input to this walk, not a subject of
                // it: reporting `.gitignore` as an unindexable file crowds the
                // coverage sample with something that could never be code.
                if name.ends_with("ignore") {
                    return false;
                }
                // Any other dotted file that survived .gitignore is deliberate
                // project content: `.air.toml`, `.gitlab-ci.yml`. Hiding it
                // second-guesses a decision the repository already made.
                return true;
            }
            TRACKED_DOT_DIRECTORIES.contains(&name.as_ref())
        })
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        // Honour .gitignore even when the project root is not itself a git
        // repository; users expect the file to be respected either way.
        .require_git(false)
        .follow_links(false)
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let absolute = entry.path();
        // follow_links(false) means a symlink is not a file, but a hard-linked
        // path could still escape; verify before recording it.
        if !sandbox.contains(absolute) {
            continue;
        }
        let Ok(relative) = sandbox.relativize(absolute) else {
            continue;
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);

        candidates.push(Candidate {
            relative_path: to_slash(&relative),
            absolute_path: absolute.to_path_buf(),
            size,
            language: detect_language(absolute, size),
        });
    }

    candidates.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    candidates
}

/// Dot-directories that hold version-controlled project configuration.
///
/// Hiding every dotted path keeps `.git` and `.venv` out, which is right, but
/// it also hides CI definitions: a workflow under `.github` is tracked source
/// that says how the project is built. Only these names are re-admitted, and
/// `.git` is deliberately not among them.
const TRACKED_DOT_DIRECTORIES: &[&str] = &[".github", ".gitlab", ".circleci"];

/// Language for a path, falling back to its `#!` line when the name carries no
/// extension.
///
/// Only extensionless files are sniffed, and only their first line is read, so
/// the walk stays a stat-and-name pass for everything else. Package maintainer
/// scripts and git hooks are real code that would otherwise be skipped as an
/// unknown language purely for lacking a suffix.
fn detect_language(absolute: &Path, size: u64) -> Option<LanguageId> {
    if let Some(language) = LanguageId::from_path(absolute) {
        return Some(language);
    }
    if absolute.extension().is_some() || size == 0 || is_oversized(size) {
        return None;
    }
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(absolute).ok()?;
    let mut first = String::new();
    // A shebang is at most a couple of hundred bytes; a binary's first "line"
    // may be enormous, so cap what is read rather than trusting the file.
    BufRead::read_line(&mut std::io::BufReader::new(file.take(512)), &mut first).ok()?;
    LanguageId::from_shebang(first.trim_end())
}

pub fn to_slash(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// True when a file is too large to parse within the memory budget.
pub fn is_oversized(size: u64) -> bool {
    size > MAX_FILE_BYTES
}

/// A NUL byte in the first block is the same heuristic `grep` uses.
pub fn looks_binary(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(8000)];
    window.contains(&0)
}

/// Answers "would the walker have skipped this path?" for coverage questions
/// about files that were never enumerated.
pub struct IgnoreMatcher {
    matcher: Gitignore,
}

impl IgnoreMatcher {
    pub fn build(root: &Path) -> Self {
        let mut builder = GitignoreBuilder::new(root);
        // Errors here mean a malformed ignore file; the matcher still works for
        // the rules it did parse, and coverage stays best-effort either way.
        let _ = builder.add(root.join(".gitignore"));
        let _ = builder.add(root.join(".ignore"));
        let matcher = builder.build().unwrap_or_else(|_| Gitignore::empty());
        Self { matcher }
    }

    pub fn is_excluded(&self, relative_path: &str, is_dir: bool) -> bool {
        if relative_path.split('/').any(|segment| segment == ".git") {
            return true;
        }
        self.matcher
            .matched_path_or_any_parents(relative_path, is_dir)
            .is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/\n*.log\n").unwrap();
        std::fs::write(root.join("src/main.py"), "def main():\n    pass\n").unwrap();
        std::fs::write(root.join("src/util.ts"), "export const x = 1;\n").unwrap();
        std::fs::write(root.join("debug.log"), "noise\n").unwrap();
        std::fs::write(
            root.join("node_modules/pkg/index.js"),
            "module.exports={};\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn walk_respects_gitignore() {
        let dir = fixture();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let found: Vec<String> = collect(&sandbox)
            .into_iter()
            .map(|c| c.relative_path)
            .collect();

        assert!(found.contains(&"src/main.py".to_string()));
        assert!(found.contains(&"src/util.ts".to_string()));
        assert!(!found.iter().any(|p| p.contains("node_modules")));
        assert!(!found.contains(&"debug.log".to_string()));
    }

    #[test]
    fn walk_tags_languages_it_recognises() {
        let dir = fixture();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let candidates = collect(&sandbox);

        let python = candidates
            .iter()
            .find(|c| c.relative_path == "src/main.py")
            .unwrap();
        assert_eq!(python.language, Some(LanguageId::Python));
    }

    #[test]
    fn ignore_matcher_classifies_pruned_paths() {
        let dir = fixture();
        let matcher = IgnoreMatcher::build(dir.path());

        assert!(matcher.is_excluded("node_modules/pkg/index.js", false));
        assert!(matcher.is_excluded("debug.log", false));
        assert!(matcher.is_excluded(".git/config", false));
        assert!(!matcher.is_excluded("src/main.py", false));
    }

    #[test]
    fn binary_detection_uses_nul_bytes() {
        assert!(looks_binary(b"\x7fELF\x00\x01"));
        assert!(!looks_binary(b"def main():\n    pass\n"));
    }
}
