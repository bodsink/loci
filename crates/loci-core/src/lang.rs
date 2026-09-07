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
            _ => return None,
        })
    }

    /// Languages eligible for Hybrid LSP type resolution once `loci-lsp` ships.
    /// Matches the product scope exactly; PHP is not and will not be a member.
    pub const fn hybrid_lsp_eligible(self) -> bool {
        true
    }

    /// Best-effort language detection from a file extension.
    ///
    /// Header files map to C; a C++ project's `.h` files are re-tagged by the
    /// indexer only when a sibling C++ translation unit exists, which M1 does
    /// not attempt. Returning C here is a documented, conservative choice.
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
            _ => return None,
        })
    }

    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        Self::from_extension(ext)
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

    #[test]
    fn detects_language_from_path() {
        assert_eq!(
            LanguageId::from_path(Path::new("src/app/main.py")),
            Some(LanguageId::Python)
        );
        assert_eq!(LanguageId::from_path(Path::new("README")), None);
    }
}
