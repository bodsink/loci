//! Hybrid LSP layer.
//!
//! The AST resolver answers most calls. This layer exists for the ones it
//! cannot: a call through a variable or field, where knowing the target needs
//! the type of the receiver. For those, loci asks a real language server
//! `textDocument/definition` and records the answer as `lsp` evidence.
//!
//! Everything here is optional and best-effort. If no server is installed, or
//! it is too slow, or it has no answer, indexing continues with AST-only
//! evidence and says so rather than guessing.

mod client;

pub use client::{Location, LspClient};

use loci_core::{LanguageId, LociError, Result};
use serde::Serialize;
use std::time::Duration;

/// A language server loci knows how to look for.
#[derive(Debug, Clone, Copy)]
pub struct ServerSpec {
    pub language: LanguageId,
    /// Executable name looked up on PATH.
    pub executable: &'static str,
    /// Arguments required to make the server speak LSP over stdio.
    pub args: &'static [&'static str],
    /// The `languageId` this server expects in `didOpen`, which is LSP's own
    /// vocabulary and does not always match loci's language id.
    pub lsp_language_id: &'static str,
}

/// Languages in scope for Hybrid LSP type resolution.
///
/// This is the product's allow-list. PHP is deliberately absent.
pub const SERVERS: &[ServerSpec] = &[
    ServerSpec {
        language: LanguageId::Python,
        executable: "pyright-langserver",
        args: &["--stdio"],
        lsp_language_id: "python",
    },
    ServerSpec {
        language: LanguageId::TypeScript,
        executable: "typescript-language-server",
        args: &["--stdio"],
        lsp_language_id: "typescript",
    },
    ServerSpec {
        language: LanguageId::Tsx,
        executable: "typescript-language-server",
        args: &["--stdio"],
        lsp_language_id: "typescriptreact",
    },
    ServerSpec {
        language: LanguageId::JavaScript,
        executable: "typescript-language-server",
        args: &["--stdio"],
        lsp_language_id: "javascript",
    },
    ServerSpec {
        language: LanguageId::Jsx,
        executable: "typescript-language-server",
        args: &["--stdio"],
        lsp_language_id: "javascriptreact",
    },
    ServerSpec {
        language: LanguageId::CSharp,
        executable: "omnisharp",
        args: &["-lsp"],
        lsp_language_id: "csharp",
    },
    ServerSpec {
        language: LanguageId::Go,
        executable: "gopls",
        args: &[],
        lsp_language_id: "go",
    },
    ServerSpec {
        language: LanguageId::C,
        executable: "clangd",
        args: &[],
        lsp_language_id: "c",
    },
    ServerSpec {
        language: LanguageId::Cpp,
        executable: "clangd",
        args: &[],
        lsp_language_id: "cpp",
    },
    ServerSpec {
        language: LanguageId::Java,
        executable: "jdtls",
        args: &[],
        lsp_language_id: "java",
    },
    ServerSpec {
        language: LanguageId::Kotlin,
        executable: "kotlin-language-server",
        args: &[],
        lsp_language_id: "kotlin",
    },
    ServerSpec {
        language: LanguageId::Rust,
        executable: "rust-analyzer",
        args: &[],
        lsp_language_id: "rust",
    },
    ServerSpec {
        language: LanguageId::Perl,
        executable: "perlnavigator",
        args: &["--stdio"],
        lsp_language_id: "perl",
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub language: String,
    pub executable: String,
    /// The executable exists on PATH. This is not proof that it runs: a rustup
    /// shim for an uninstalled component is on PATH and fails on first use.
    pub on_path: bool,
    /// True when loci will attempt this server during indexing. Whether it
    /// answers is only known once it is started, and a failure falls back to
    /// AST rather than aborting the index.
    pub will_attempt: bool,
}

fn on_path(executable: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(executable);
        candidate.is_file()
    })
}

/// Report which servers are installed. Never starts a process.
pub fn detect() -> Vec<ServerStatus> {
    SERVERS
        .iter()
        .map(|spec| ServerStatus {
            language: spec.language.as_str().to_string(),
            executable: spec.executable.to_string(),
            on_path: on_path(spec.executable),
            will_attempt: on_path(spec.executable),
        })
        .collect()
}

pub fn is_eligible(language: LanguageId) -> bool {
    SERVERS.iter().any(|s| s.language == language)
}

pub fn spec_for(language: LanguageId) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.language == language)
}

/// Start the language server for `language`, rooted at `root`.
///
/// Fails with `lsp_unavailable` when no server is defined for the language or
/// the executable is not on PATH, so callers can degrade to AST cleanly.
pub fn start(
    language: LanguageId,
    root: &std::path::Path,
    timeout: Duration,
) -> Result<(LspClient, &'static ServerSpec)> {
    let spec = spec_for(language)
        .ok_or_else(|| LociError::LspUnavailable(format!("{language}: no server is defined")))?;

    if !on_path(spec.executable) {
        return Err(LociError::LspUnavailable(format!(
            "{language}: '{}' is not on PATH",
            spec.executable
        )));
    }

    let client = LspClient::start(spec.executable, spec.args, root, timeout)?;
    Ok((client, spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_is_not_in_the_allow_list() {
        assert!(!SERVERS
            .iter()
            .any(|s| s.executable.to_lowercase().contains("php")));
    }

    #[test]
    fn detection_reports_every_scoped_language() {
        let statuses = detect();
        assert_eq!(statuses.len(), SERVERS.len());
        // Presence on PATH is the only thing detection can honestly claim.
        for status in &statuses {
            assert_eq!(status.will_attempt, status.on_path, "{status:?}");
        }
    }

    /// A language with no server, or a server that is not installed, must be an
    /// explicit `lsp_unavailable` so the caller falls back rather than stalls.
    #[test]
    fn starting_an_absent_server_fails_loudly() {
        let root = tempfile::tempdir().expect("temp root");
        let missing = SERVERS
            .iter()
            .find(|s| !on_path(s.executable))
            .map(|s| s.language);

        if let Some(language) = missing {
            match start(language, root.path(), Duration::from_secs(2)) {
                Ok(_) => panic!("{language} has no installed server but start() succeeded"),
                Err(error) => assert_eq!(error.code(), "lsp_unavailable"),
            }
        }
    }

    #[test]
    fn every_server_can_be_looked_up_by_language() {
        for spec in SERVERS {
            assert_eq!(
                spec_for(spec.language).map(|s| s.executable),
                Some(spec.executable)
            );
        }
    }

    #[test]
    fn every_scoped_language_is_eligible() {
        for language in [
            LanguageId::Python,
            LanguageId::TypeScript,
            LanguageId::CSharp,
            LanguageId::Go,
            LanguageId::C,
            LanguageId::Cpp,
            LanguageId::Java,
            LanguageId::Kotlin,
            LanguageId::Rust,
            LanguageId::Perl,
        ] {
            assert!(is_eligible(language), "{language} must be LSP-eligible");
        }
    }
}
