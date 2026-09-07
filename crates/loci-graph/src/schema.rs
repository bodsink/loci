use loci_core::LanguageId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Node ids start at 1. Zero is reserved as the "no such node" sentinel used by
/// unresolved call edges, which record a callee *name* because no node exists.
pub const UNRESOLVED_NODE_ID: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NodeLabel {
    Project,
    File,
    Package,
    Module,
    Function,
    Method,
    Class,
    Type,
    Interface,
    Trait,
    Struct,
    Enum,
    Field,
    Route,
    Adr,
    TraceSpan,
}

impl NodeLabel {
    pub const ALL: &'static [NodeLabel] = &[
        NodeLabel::Project,
        NodeLabel::File,
        NodeLabel::Package,
        NodeLabel::Module,
        NodeLabel::Function,
        NodeLabel::Method,
        NodeLabel::Class,
        NodeLabel::Type,
        NodeLabel::Interface,
        NodeLabel::Trait,
        NodeLabel::Struct,
        NodeLabel::Enum,
        NodeLabel::Field,
        NodeLabel::Route,
        NodeLabel::Adr,
        NodeLabel::TraceSpan,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::File => "File",
            Self::Package => "Package",
            Self::Module => "Module",
            Self::Function => "Function",
            Self::Method => "Method",
            Self::Class => "Class",
            Self::Type => "Type",
            Self::Interface => "Interface",
            Self::Trait => "Trait",
            Self::Struct => "Struct",
            Self::Enum => "Enum",
            Self::Field => "Field",
            Self::Route => "Route",
            Self::Adr => "Adr",
            Self::TraceSpan => "TraceSpan",
        }
    }

    pub fn from_str_label(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|l| l.as_str() == s)
    }

    /// Callable definitions, used by trace_path and call-graph resolution.
    pub const fn is_callable(self) -> bool {
        matches!(self, Self::Function | Self::Method)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EdgeType {
    Contains,
    Defines,
    Calls,
    CallUnresolved,
    Imports,
    Inherits,
    Implements,
    HasField,
    RoutesTo,
    ConfigRef,
    ProtoRef,
    OpenApiRef,
    Impacts,
}

impl EdgeType {
    pub const ALL: &'static [EdgeType] = &[
        EdgeType::Contains,
        EdgeType::Defines,
        EdgeType::Calls,
        EdgeType::CallUnresolved,
        EdgeType::Imports,
        EdgeType::Inherits,
        EdgeType::Implements,
        EdgeType::HasField,
        EdgeType::RoutesTo,
        EdgeType::ConfigRef,
        EdgeType::ProtoRef,
        EdgeType::OpenApiRef,
        EdgeType::Impacts,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "CONTAINS",
            Self::Defines => "DEFINES",
            Self::Calls => "CALLS",
            Self::CallUnresolved => "CALL_UNRESOLVED",
            Self::Imports => "IMPORTS",
            Self::Inherits => "INHERITS",
            Self::Implements => "IMPLEMENTS",
            Self::HasField => "HAS_FIELD",
            Self::RoutesTo => "ROUTES_TO",
            Self::ConfigRef => "CONFIG_REF",
            Self::ProtoRef => "PROTO_REF",
            Self::OpenApiRef => "OPENAPI_REF",
            Self::Impacts => "IMPACTS",
        }
    }

    pub fn from_str_type(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.as_str() == s)
    }

    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Contains => 0,
            Self::Defines => 1,
            Self::Calls => 2,
            Self::CallUnresolved => 3,
            Self::Imports => 4,
            Self::Inherits => 5,
            Self::Implements => 6,
            Self::HasField => 7,
            Self::RoutesTo => 8,
            Self::ConfigRef => 9,
            Self::ProtoRef => 10,
            Self::OpenApiRef => 11,
            Self::Impacts => 12,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.code() == code)
    }
}

/// How a node or edge was established. Nothing is ever recorded as `Lsp` unless
/// a language server actually answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Evidence {
    Ast,
    Lsp,
    Hybrid,
    Trace,
}

impl Evidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ast => "ast",
            Self::Lsp => "lsp",
            Self::Hybrid => "hybrid",
            Self::Trace => "trace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub label: NodeLabel,
    pub name: String,
    pub qualified_name: String,
    /// Repository-relative. Empty only for the synthetic Project node.
    pub file_path: String,
    /// 1-based, inclusive.
    pub start_line: u32,
    /// 1-based, inclusive.
    pub end_line: u32,
    pub language: Option<LanguageId>,
    pub source: Evidence,
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Label-specific facts with hard evidence, e.g. a Route's method and path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl Node {
    pub fn new(
        label: NodeLabel,
        name: impl Into<String>,
        qualified_name: impl Into<String>,
        file_path: impl Into<String>,
        start_line: u32,
        end_line: u32,
        language: Option<LanguageId>,
    ) -> Self {
        Self {
            id: 0,
            label,
            name: name.into(),
            qualified_name: qualified_name.into(),
            file_path: file_path.into(),
            start_line,
            end_line,
            language,
            source: Evidence::Ast,
            confidence: 1.0,
            signature: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: u32,
    pub src: u64,
    /// [`UNRESOLVED_NODE_ID`] when `edge_type` is [`EdgeType::CallUnresolved`].
    pub dst: u64,
    pub edge_type: EdgeType,
    pub source: Evidence,
    pub confidence: f32,
    /// For unresolved calls: the callee name as written in source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    /// Why an edge could not be resolved, or how it was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl Edge {
    pub fn new(src: u64, dst: u64, edge_type: EdgeType) -> Self {
        Self {
            id: 0,
            src,
            dst,
            edge_type,
            source: Evidence::Ast,
            confidence: 1.0,
            target_name: None,
            detail: None,
            line: None,
        }
    }

    pub fn unresolved_call(src: u64, callee: impl Into<String>, line: u32, reason: &str) -> Self {
        Self {
            id: 0,
            src,
            dst: UNRESOLVED_NODE_ID,
            edge_type: EdgeType::CallUnresolved,
            source: Evidence::Ast,
            confidence: 1.0,
            target_name: Some(callee.into()),
            detail: Some(reason.to_string()),
            line: Some(line),
        }
    }
}

/// Packs an adjacency entry into a single value so traversal can filter by edge
/// type without a second table lookup.
pub(crate) fn pack_adjacency(edge_type: EdgeType, other: u64, edge_id: u32) -> u128 {
    (u128::from(edge_type.code()) << 96) | (u128::from(edge_id) << 64) | u128::from(other)
}

pub(crate) fn unpack_adjacency(packed: u128) -> Option<(EdgeType, u64, u32)> {
    let edge_type = EdgeType::from_code(((packed >> 96) & 0xff) as u8)?;
    let edge_id = ((packed >> 64) & 0xffff_ffff) as u32;
    let other = (packed & 0xffff_ffff_ffff_ffff) as u64;
    Some((edge_type, other, edge_id))
}

/// A call site as written in source, kept so call resolution can be rerun
/// without re-parsing the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredCall {
    /// Qualified name of the definition the call sits inside, or the file's
    /// module when the call is at top level.
    pub from_qualified_name: String,
    pub callee_name: String,
    /// Receiver of a qualified call such as `service.create_order()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    pub line: u32,
    /// Zero-based UTF-16 column of the callee identifier, kept so a language
    /// server can be asked about this exact call site during a later pass.
    #[serde(default)]
    pub character: u32,
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredImport {
    pub from_file: String,
    /// Import target exactly as written in source.
    pub target: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredTypeRel {
    pub subtype_qualified_name: String,
    pub supertype_name: String,
    /// Either `INHERITS` or `IMPLEMENTS`.
    pub edge_type: EdgeType,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRouteLink {
    pub route_qualified_name: String,
    pub handler_name: String,
    /// Object a method handler hangs off, so `h.Login` resolves as a method
    /// rather than as a free function that does not exist.
    #[serde(default)]
    pub handler_receiver: Option<String>,
    pub file_path: String,
}

/// Everything a file contributes that can only be turned into edges once the
/// whole project's symbols are known. Persisted so an incremental run can redo
/// resolution for untouched files without re-parsing them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileFacts {
    #[serde(default)]
    pub calls: Vec<StoredCall>,
    #[serde(default)]
    pub imports: Vec<StoredImport>,
    #[serde(default)]
    pub type_relations: Vec<StoredTypeRel>,
    #[serde(default)]
    pub route_links: Vec<StoredRouteLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// Parsed with no recorded gap. Not a completeness guarantee.
    Indexed,
    /// Parsed, but the parser reported error ranges; constructs there may be missing.
    ParsePartial,
    /// Not indexed at all.
    Skipped,
    /// Deliberately not indexed (gitignore and friends). By design, not a failure.
    Excluded,
}

impl CoverageStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::ParsePartial => "parse_partial",
            Self::Skipped => "skipped",
            Self::Excluded => "excluded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageReason {
    Gitignore,
    Binary,
    Oversized,
    UnsupportedLanguage,
    ParseError,
    ReadError,
    SymlinkEscape,
}

impl CoverageReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gitignore => "gitignore",
            Self::Binary => "binary",
            Self::Oversized => "oversized",
            Self::UnsupportedLanguage => "unsupported_language",
            Self::ParseError => "parse_error",
            Self::ReadError => "read_error",
            Self::SymlinkEscape => "symlink_escape",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    /// blake3 of file bytes; empty when the file could not be read.
    pub hash: String,
    pub size: u64,
    pub language: Option<LanguageId>,
    pub status: CoverageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<CoverageReason>,
    /// Human-readable specifics, e.g. "parse errors at lines 12-19".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub node_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub schema_version: u32,
    pub name: String,
    pub root: String,
    pub indexed_at_unix: i64,
    pub duration_ms: u64,
    pub node_count: u64,
    pub edge_count: u64,
    pub file_count: u64,
    #[serde(default)]
    pub languages: BTreeMap<String, u64>,
    #[serde(default)]
    pub bundled_languages: Vec<String>,
    pub next_node_id: u64,
    pub next_edge_id: u32,
    /// Structural edges (DEFINES, CONTAINS) are numbered from the top half of
    /// the id space, resolved edges from the bottom half.
    #[serde(default = "default_structural_edge_id")]
    pub next_structural_edge_id: u32,
}

/// First id in the structural half of the edge id space.
pub const STRUCTURAL_EDGE_ID_BASE: u32 = 0x8000_0000;

fn default_structural_edge_id() -> u32 {
    STRUCTURAL_EDGE_ID_BASE
}

impl ProjectMeta {
    pub fn new(name: String, root: String) -> Self {
        Self {
            schema_version: loci_core::SCHEMA_VERSION,
            name,
            root,
            indexed_at_unix: 0,
            duration_ms: 0,
            node_count: 0,
            edge_count: 0,
            file_count: 0,
            languages: BTreeMap::new(),
            bundled_languages: Vec::new(),
            next_node_id: 1,
            next_edge_id: 1,
            next_structural_edge_id: STRUCTURAL_EDGE_ID_BASE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacency_packing_round_trips() {
        let packed = pack_adjacency(EdgeType::Calls, u64::MAX >> 1, 4_294_967_295);
        let (ty, other, edge_id) = unpack_adjacency(packed).unwrap();
        assert_eq!(ty, EdgeType::Calls);
        assert_eq!(other, u64::MAX >> 1);
        assert_eq!(edge_id, 4_294_967_295);
    }

    #[test]
    fn every_edge_type_has_a_unique_code() {
        let mut codes: Vec<u8> = EdgeType::ALL.iter().map(|e| e.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), EdgeType::ALL.len());
    }

    #[test]
    fn labels_and_edge_types_parse_from_their_names() {
        for label in NodeLabel::ALL {
            assert_eq!(NodeLabel::from_str_label(label.as_str()), Some(*label));
        }
        for edge in EdgeType::ALL {
            assert_eq!(EdgeType::from_str_type(edge.as_str()), Some(*edge));
        }
    }

    #[test]
    fn unresolved_calls_carry_a_name_and_no_destination() {
        let edge = Edge::unresolved_call(7, "handle_request", 12, "no definition in file scope");
        assert_eq!(edge.dst, UNRESOLVED_NODE_ID);
        assert_eq!(edge.target_name.as_deref(), Some("handle_request"));
    }
}
