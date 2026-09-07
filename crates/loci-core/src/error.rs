use std::path::PathBuf;

/// Every failure surfaced to a CLI user or an MCP client.
///
/// Each variant maps to a stable machine-readable `code()` so agents can branch
/// on the failure instead of parsing prose.
#[derive(Debug, thiserror::Error)]
pub enum LociError {
    #[error("project '{0}' is not in the catalog; run list_projects to see indexed projects")]
    ProjectNotFound(String),

    #[error("project '{0}' has a catalog entry but no graph on disk; run index_repository first")]
    IndexMissing(String),

    #[error("language '{0}' has no bundled grammar; see get_graph_schema for bundled languages")]
    LanguageUnsupported(String),

    #[error("no language server available for '{0}'; loci falls back to AST-only extraction")]
    LspUnavailable(String),

    #[error("path '{}' resolves outside the indexed project root", .0.display())]
    PathOutOfRoot(PathBuf),

    #[error("'{name}' matches {count} symbols; pass an exact qualified_name")]
    AmbiguousSymbol { name: String, count: usize },

    #[error("symbol '{0}' is not in the graph")]
    SymbolNotFound(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("cursor is stale or malformed; re-run the original query without a cursor")]
    BadCursor,

    #[error("storage error: {0}")]
    Storage(String),

    #[error("io error at {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<serde_json::Error> for LociError {
    fn from(source: serde_json::Error) -> Self {
        Self::Storage(format!("json error: {source}"))
    }
}

impl LociError {
    /// Stable identifier for programmatic handling. Never localised, never reworded.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProjectNotFound(_) => "project_not_found",
            Self::IndexMissing(_) => "index_missing",
            Self::LanguageUnsupported(_) => "language_unsupported",
            Self::LspUnavailable(_) => "lsp_unavailable",
            Self::PathOutOfRoot(_) => "path_out_of_root",
            Self::AmbiguousSymbol { .. } => "ambiguous_symbol",
            Self::SymbolNotFound(_) => "symbol_not_found",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::BadCursor => "bad_cursor",
            Self::Storage(_) => "storage_error",
            Self::Io { .. } => "io_error",
        }
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, LociError>;
