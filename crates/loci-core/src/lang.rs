use serde::{Deserialize, Serialize};

/// Languages loci can name. A variant here means the id is real and stable;
/// it does NOT promise a bundled grammar. Ask `loci_parse::registry` for that.
///
/// PHP is deliberately absent and must not be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageId {
    Python,
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
    Go,
    Rust,
    C,
    Cpp,
    Java,
    CSharp,
    Kotlin,
    Perl,
    /// AST only. There is no shell entry in the Hybrid LSP scope.
    Bash,
    /// Configuration formats. Parsed for structure, never LSP-resolved.
    Toml,
    Yaml,
    /// INI, which is also the shape of a systemd unit.
    Ini,
    /// Build definitions. Also structure rather than code, and named by file
    /// name more often than by extension.
    Make,
    Cmake,
}

impl LanguageId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Jsx => "jsx",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::Kotlin => "kotlin",
            Self::Perl => "perl",
            Self::Bash => "bash",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
            Self::Ini => "ini",
            Self::Make => "make",
            Self::Cmake => "cmake",
        }
    }

    pub fn from_str_id(s: &str) -> Option<Self> {
        Some(match s {
            "python" => Self::Python,
            "javascript" => Self::JavaScript,
            "jsx" => Self::Jsx,
            "typescript" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "go" => Self::Go,
            "rust" => Self::Rust,
            "c" => Self::C,
            "cpp" => Self::Cpp,
            "java" => Self::Java,
            "csharp" => Self::CSharp,
            "kotlin" => Self::Kotlin,
            "perl" => Self::Perl,
            "bash" => Self::Bash,
            "toml" => Self::Toml,
            "yaml" => Self::Yaml,
            "ini" => Self::Ini,
            "make" => Self::Make,
            "cmake" => Self::Cmake,
            _ => return None,
        })
    }

    /// Languages eligible for Hybrid LSP type resolution.
    ///
    /// No longer every bundled language: shell is parsed but has no server in
    /// the product scope. PHP is not and will not be a member.
    pub const fn hybrid_lsp_eligible(self) -> bool {
        !matches!(
            self,
            Self::Bash | Self::Toml | Self::Yaml | Self::Ini | Self::Make | Self::Cmake
        )
    }

    /// Best-effort language detection from a file extension.
    ///
    /// `.h` maps to C, but the extension does not settle it: the indexer parses
    /// ambiguous headers with both grammars and keeps whichever reports fewer
    /// errors, so a C++ header is not condemned to the C grammar.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext {
            "py" | "pyi" => Self::Python,
            "js" | "mjs" | "cjs" => Self::JavaScript,
            "jsx" => Self::Jsx,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "go" => Self::Go,
            "rs" => Self::Rust,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Self::Cpp,
            "java" => Self::Java,
            "cs" => Self::CSharp,
            "kt" | "kts" => Self::Kotlin,
            "pl" | "pm" => Self::Perl,
            "sh" | "bash" => Self::Bash,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            // systemd unit files are INI with a fixed set of suffixes.
            "ini" | "cfg" | "service" | "socket" | "timer" | "target" | "mount" | "path"
            | "slice" => Self::Ini,
            "mk" | "mak" => Self::Make,
            "cmake" => Self::Cmake,
            _ => return None,
        })
    }

    /// Build files that are known by name, because their extension either
    /// says nothing (`CMakeLists.txt`) or does not exist (`Makefile`).
    ///
    /// `.txt` must keep meaning nothing in general, so the whole name has to
    /// be matched rather than the suffix.
    pub fn from_file_name(name: &str) -> Option<Self> {
        match name {
            "Makefile" | "makefile" | "GNUmakefile" => return Some(Self::Make),
            "CMakeLists.txt" => return Some(Self::Cmake),
            _ => {}
        }
        // `.gitignore` has no stem, so `Path::extension` would call it a file
        // with no suffix; splitting on the last dot treats it as "gitignore",
        // which maps to nothing either way.
        Self::from_extension(name.rsplit_once('.')?.1)
    }

    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        Self::from_file_name(path.file_name()?.to_str()?)
    }

    /// Language named by a `#!` line, for files that carry no extension.
    ///
    /// Package maintainer scripts (`postinst`, `prerm`) and hooks are real code
    /// with no suffix to go on, so the interpreter is the only honest signal.
    /// Only the first line is considered, and only shells are recognised: the
    /// point is to stop losing scripts, not to guess at every interpreter.
    pub fn from_shebang(first_line: &str) -> Option<Self> {
        let line = first_line.strip_prefix("#!")?.trim();
        if line.is_empty() {
            return None;
        }
        // `#!/usr/bin/env bash -e` names the interpreter in the second word.
        let mut words = line.split_whitespace();
        let command = words.next()?;
        let mut interpreter = command.rsplit('/').next()?;
        if interpreter == "env" {
            interpreter = words.find(|w| !w.starts_with('-'))?;
        }
        match interpreter {
            "sh" | "bash" | "dash" | "zsh" | "ash" => Some(Self::Bash),
            _ => None,
        }
    }
}

impl std::fmt::Display for LanguageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn extension_mapping_covers_m1_languages() {
        assert_eq!(LanguageId::from_extension("py"), Some(LanguageId::Python));
        assert_eq!(LanguageId::from_extension("tsx"), Some(LanguageId::Tsx));
        assert_eq!(LanguageId::from_extension("go"), Some(LanguageId::Go));
        assert_eq!(LanguageId::from_extension("rs"), Some(LanguageId::Rust));
    }

    #[test]
    fn php_is_out_of_scope() {
        assert_eq!(LanguageId::from_extension("php"), None);
        assert_eq!(LanguageId::from_str_id("php"), None);
    }

    /// Build files are the only ones known by whole name, and the risk is that
    /// their names leak: `.txt` must go on meaning nothing.
    #[test]
    fn build_files_are_recognised_by_name_without_claiming_their_extension() {
        assert_eq!(
            LanguageId::from_file_name("Makefile"),
            Some(LanguageId::Make)
        );
        assert_eq!(
            LanguageId::from_file_name("CMakeLists.txt"),
            Some(LanguageId::Cmake)
        );
        assert_eq!(LanguageId::from_file_name("notes.txt"), None);
        assert_eq!(LanguageId::from_extension("txt"), None);
    }

    /// A dotted name has no stem, so anything reading the "extension" has to
    /// treat the whole tail as one. `.air.toml` is the case that matters.
    #[test]
    fn a_dotted_file_name_still_resolves_by_its_last_suffix() {
        assert_eq!(
            LanguageId::from_path(Path::new("/repo/.air.toml")),
            Some(LanguageId::Toml)
        );
        assert_eq!(LanguageId::from_path(Path::new("/repo/.gitignore")), None);
    }

    #[test]
    fn round_trips_through_string_id() {
        for lang in [
            LanguageId::Python,
            LanguageId::TypeScript,
            LanguageId::Cpp,
            LanguageId::Perl,
        ] {
            assert_eq!(LanguageId::from_str_id(lang.as_str()), Some(lang));
        }
    }

    /// Debian maintainer scripts carry no extension, so without this they are
    /// skipped as an unknown language despite being ordinary shell.
    #[test]
    fn detects_shell_from_a_shebang() {
        for line in [
            "#!/bin/sh",
            "#!/bin/bash",
            "#!/usr/bin/env bash",
            "#!/usr/bin/env -S bash -e",
            "#! /bin/sh",
            "#!/bin/bash -eu",
        ] {
            assert_eq!(
                LanguageId::from_shebang(line),
                Some(LanguageId::Bash),
                "{line}"
            );
        }
    }

    #[test]
    fn a_shebang_for_anything_else_is_not_guessed_at() {
        for line in [
            "#!/usr/bin/env python3",
            "#!/usr/bin/perl",
            "#!/usr/bin/awk -f",
            "not a shebang",
            "#!",
            "",
        ] {
            assert_eq!(LanguageId::from_shebang(line), None, "{line}");
        }
    }

    #[test]
    fn shell_is_parsed_but_is_not_an_lsp_language() {
        assert_eq!(LanguageId::from_extension("sh"), Some(LanguageId::Bash));
        assert!(!LanguageId::Bash.hybrid_lsp_eligible());
        assert!(LanguageId::Go.hybrid_lsp_eligible());
    }

    #[test]
    fn detects_language_from_path() {
        assert_eq!(
            LanguageId::from_path(Path::new("src/app/main.py")),
            Some(LanguageId::Python)
        );
        assert_eq!(LanguageId::from_path(Path::new("README")), None);
    }
}
