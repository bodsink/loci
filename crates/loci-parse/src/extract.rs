use crate::registry;
use crate::spec::{self, LanguageSpec, HTTP_VERBS, ROUTE_REGISTRARS};
use loci_core::{LanguageId, LociError, Result};
use loci_graph::NodeLabel;
use std::collections::BTreeMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

/// A symbol definition found in one file.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub label: NodeLabel,
    pub name: String,
    pub qualified_name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: usize,
    pub end_byte: usize,
    pub signature: Option<String>,
    /// Project type this returns, when it returns exactly one nameable type.
    ///
    /// Read from the tree rather than from `signature`, which keeps only the
    /// first line and caps at 200 characters — a constructor whose parameters
    /// span several lines would otherwise lose its return type entirely, and
    /// that is how most of them are written.
    pub returns: Option<String>,
}

/// A call site. The callee is a *name as written*; resolution happens later.
#[derive(Debug, Clone, PartialEq)]
pub struct CallSite {
    pub callee_name: String,
    /// Receiver text for a qualified call such as `service.create_order()`.
    /// `None` for a bare call. Lets resolution tell a method call apart from a
    /// same-named free function, which name-only matching gets wrong.
    pub receiver: Option<String>,
    pub line: u32,
    /// Zero-based UTF-16 column of the callee identifier. LSP addresses
    /// positions this way, and it is the only thing a language server can be
    /// asked about, so it is captured even though the AST resolver ignores it.
    pub character: u32,
    pub byte: usize,
}

impl CallSite {
    /// True when the receiver refers to the enclosing object rather than another one.
    pub fn receiver_is_self(&self) -> bool {
        matches!(self.receiver.as_deref(), Some("self") | Some("this"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportRef {
    pub target: String,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRelKind {
    Inherits,
    Implements,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeRel {
    pub subtype: String,
    pub supertype: String,
    pub kind: TypeRelKind,
    pub line: u32,
}

/// An HTTP route backed by an actual AST pattern. Never inferred from a name.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDef {
    pub method: String,
    pub path: String,
    pub handler_name: Option<String>,
    /// Object a method handler hangs off, as written: the `h` of `h.Login`.
    ///
    /// Kept apart from the name so resolution can tell a method handler from a
    /// free function, the same way it tells `svc.save()` from `save()`.
    pub handler_receiver: Option<String>,
    pub line: u32,
    pub end_line: u32,
    pub framework_hint: String,
}

/// A local variable that takes its type from what a function returns.
///
/// `zoneHandler := handlers.NewZoneHandler(…)` says nothing about a type on its
/// own, but the constructor's signature does, and that signature is already in
/// the graph. This is what lets `zoneHandler.List` pick one method out of the
/// 72 named `List`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiverBinding {
    pub variable: String,
    pub constructor: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtractedFile {
    pub definitions: Vec<Definition>,
    pub calls: Vec<CallSite>,
    pub imports: Vec<ImportRef>,
    pub type_relations: Vec<TypeRel>,
    pub routes: Vec<RouteDef>,
    pub receiver_bindings: Vec<ReceiverBinding>,
    /// Inclusive 1-based line ranges the parser could not understand.
    pub error_ranges: Vec<(u32, u32)>,
}

impl ExtractedFile {
    /// Innermost definition containing a byte offset, if any.
    pub fn enclosing_definition(&self, byte: usize) -> Option<&Definition> {
        self.definitions
            .iter()
            .filter(|d| d.start_byte <= byte && byte < d.end_byte)
            .min_by_key(|d| d.end_byte - d.start_byte)
    }
}

/// Turn a repository-relative path into a dotted module prefix.
///
/// The separator is `.` for every language so agents can predict qualified
/// names without knowing the language's own path syntax.
pub fn module_prefix(relative_path: &str) -> String {
    let without_ext = relative_path
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(relative_path);

    let mut segments: Vec<&str> = without_ext
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();

    // A package initialiser names the package, not a module inside it.
    if matches!(
        segments.last(),
        Some(&"__init__") | Some(&"index") | Some(&"mod")
    ) {
        segments.pop();
    }
    segments.join(".")
}

fn line_of(node: TsNode) -> u32 {
    node.start_position().row as u32 + 1
}

fn end_line_of(node: TsNode) -> u32 {
    node.end_position().row as u32 + 1
}

/// Tree-sitter counts columns in bytes; LSP counts them in UTF-16 code units.
/// They only agree on ASCII lines, so the conversion is done from the real
/// line text rather than assumed away.
fn utf16_column(node: TsNode, source: &str) -> u32 {
    let start = node.start_byte();
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    source[line_start..start].encode_utf16().count() as u32
}

fn text<'a>(node: TsNode, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

/// Strip quotes from a string literal token, including Go backticks.
fn unquote(raw: &str) -> String {
    raw.trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string()
}

/// First line of a definition, used as a human-readable signature.
fn signature_of(node: TsNode, source: &str) -> Option<String> {
    let full = text(node, source);
    let first = full.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let trimmed = first.trim_end_matches(['{', ':']).trim();
    let capped: String = trimmed.chars().take(200).collect();
    (!capped.is_empty()).then_some(capped)
}

/// The single project type a declaration returns, read from its `result`.
///
/// A type from another package is spelled `qualified_type` and is deliberately
/// not returned: its methods are not this project's to claim. Slices, maps and
/// builtins own no methods here either, so they yield nothing rather than a
/// name that would match the wrong thing.
fn returns_of(node: TsNode, source: &str) -> Option<String> {
    const BUILTIN: &[&str] = &[
        "error",
        "string",
        "bool",
        "byte",
        "rune",
        "any",
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "float32",
        "float64",
        "complex64",
        "complex128",
        "uintptr",
    ];

    fn named_type(node: TsNode, source: &str) -> Option<String> {
        match node.kind() {
            "type_identifier" => Some(text(node, source).to_string()),
            "pointer_type" | "parenthesized_type" => {
                let mut cursor = node.walk();
                let children: Vec<TsNode> = node.named_children(&mut cursor).collect();
                children
                    .into_iter()
                    .find_map(|child| named_type(child, source))
            }
            // Multiple results: the first is what the variable is used as, and
            // the rest are conventionally an error.
            "parameter_list" => {
                let mut cursor = node.walk();
                let first = node.named_children(&mut cursor).next()?;
                named_type(first, source)
            }
            "parameter_declaration" => named_type(node.child_by_field_name("type")?, source),
            _ => None,
        }
    }

    let named = named_type(node.child_by_field_name("result")?, source)?;
    (!BUILTIN.contains(&named.as_str())).then_some(named)
}

fn has_ancestor_of_kind(node: TsNode, kinds: &[&str]) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if kinds.contains(&parent.kind()) {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// Collect 1-based line ranges of ERROR and MISSING nodes.
///
/// Their presence downgrades a file to `parse_partial`: it was indexed, but
/// constructs inside those ranges may be absent from the graph.
fn collect_error_ranges(root: TsNode) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    if !root.has_error() {
        return ranges;
    }
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            ranges.push((line_of(node), end_line_of(node)));
            // Children of an error node add no information.
            continue;
        }
        if node.has_error() {
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
    }
    ranges.sort_unstable();
    ranges.dedup();
    ranges
}

fn compile(
    query_source: &str,
    language: &tree_sitter::Language,
    what: &str,
) -> Result<Option<Query>> {
    if query_source.trim().is_empty() {
        return Ok(None);
    }
    Query::new(language, query_source)
        .map(Some)
        .map_err(|e| LociError::Storage(format!("built-in {what} query failed to compile: {e}")))
}

/// Parse one file and extract every graph fact the AST can support.
pub fn extract(language: LanguageId, relative_path: &str, source: &str) -> Result<ExtractedFile> {
    let grammar = registry::grammar(language)
        .ok_or_else(|| LociError::LanguageUnsupported(language.as_str().to_string()))?;
    let spec = spec::spec_for(language)
        .ok_or_else(|| LociError::LanguageUnsupported(language.as_str().to_string()))?;

    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|e| LociError::Storage(format!("cannot load {language} grammar: {e}")))?;
    let tree = parser.parse(source, None).ok_or_else(|| {
        LociError::Storage(format!("tree-sitter returned no tree for {relative_path}"))
    })?;
    let root = tree.root_node();

    let mut extracted = ExtractedFile {
        error_ranges: collect_error_ranges(root),
        ..Default::default()
    };

    let prefix = module_prefix(relative_path);
    extract_definitions(&mut extracted, &grammar, spec, root, source, &prefix)?;
    extract_references(&mut extracted, &grammar, spec, root, source)?;
    if language == LanguageId::Bash {
        extract_shell_references(&mut extracted, root, source, relative_path);
    }
    if language == LanguageId::Cmake {
        extract_cmake_references(&mut extracted, root, source, relative_path);
    }
    if language == LanguageId::Html {
        extract_html(&mut extracted, root, source, relative_path, &prefix);
    }
    // Go is the only language here that both hides the receiver type behind a
    // constructor and names methods without their type, so it is the only one
    // that needs the binding to resolve a receiver.
    if language == LanguageId::Go {
        extract_receiver_bindings(&mut extracted, root, source);
    }
    extract_routes(&mut extracted, &grammar, spec, root, source, language)?;

    Ok(extracted)
}

fn extract_definitions(
    out: &mut ExtractedFile,
    grammar: &tree_sitter::Language,
    spec: &LanguageSpec,
    root: TsNode,
    source: &str,
    prefix: &str,
) -> Result<()> {
    let Some(query) = compile(spec.definitions, grammar, "definitions")? else {
        return Ok(());
    };

    // Raw hits first; qualified names need the full set to resolve nesting.
    struct Raw {
        label: NodeLabel,
        name: String,
        /// Type this definition hangs off when the grammar states it beside the
        /// definition rather than around it — a Go method's receiver.
        owner: Option<String>,
        start_line: u32,
        end_line: u32,
        start_byte: usize,
        end_byte: usize,
        signature: Option<String>,
        returns: Option<String>,
    }

    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source.as_bytes());
    let mut raws: Vec<Raw> = Vec::new();

    while let Some(m) = matches.next() {
        let mut label: Option<NodeLabel> = None;
        let mut def_node: Option<TsNode> = None;
        let mut name_node: Option<TsNode> = None;
        let mut owner_node: Option<TsNode> = None;

        for capture in m.captures {
            let capture_name = capture_names[capture.index as usize];
            if capture_name == "name" {
                name_node = Some(capture.node);
            } else if capture_name == "owner" {
                owner_node = Some(capture.node);
            } else if let Some(l) = spec::label_for_capture(capture_name) {
                label = Some(l);
                def_node = Some(capture.node);
            }
        }

        let (Some(mut label), Some(def_node), Some(name_node)) = (label, def_node, name_node)
        else {
            continue;
        };

        // A free function nested in a class body is a method.
        if label == NodeLabel::Function && has_ancestor_of_kind(def_node, spec.method_parents) {
            label = NodeLabel::Method;
        }

        raws.push(Raw {
            label,
            name: text(name_node, source).to_string(),
            owner: owner_node.map(|node| text(node, source).to_string()),
            start_line: line_of(def_node),
            end_line: end_line_of(def_node),
            start_byte: def_node.start_byte(),
            end_byte: def_node.end_byte(),
            signature: signature_of(def_node, source),
            returns: returns_of(def_node, source),
        });
    }

    raws.sort_by_key(|r| (r.start_byte, std::cmp::Reverse(r.end_byte)));

    // Qualified name = module prefix + enclosing definition names + own name.
    for i in 0..raws.len() {
        let mut chain: Vec<&str> = Vec::new();
        for j in 0..raws.len() {
            if i == j {
                continue;
            }
            let encloses =
                raws[j].start_byte <= raws[i].start_byte && raws[i].end_byte <= raws[j].end_byte;
            if encloses && raws[j].label != NodeLabel::Field {
                chain.push(&raws[j].name);
            }
        }

        let mut parts: Vec<&str> = Vec::new();
        if !prefix.is_empty() {
            parts.push(prefix);
        }
        parts.extend(chain);
        // Go states the owning type beside the method rather than around it, so
        // it never appears in the enclosure chain. Without it every `List` in a
        // repository shares one name — 72 of them in the project measured here,
        // and 92 called `Create` — and no receiver can pick between them.
        if let Some(owner) = &raws[i].owner {
            parts.push(owner);
        }
        parts.push(&raws[i].name);

        out.definitions.push(Definition {
            label: raws[i].label,
            name: raws[i].name.clone(),
            qualified_name: parts.join("."),
            start_line: raws[i].start_line,
            end_line: raws[i].end_line,
            start_byte: raws[i].start_byte,
            end_byte: raws[i].end_byte,
            signature: raws[i].signature.clone(),
            returns: raws[i].returns.clone(),
        });
    }

    Ok(())
}

/// Turn an included path into a repository-relative one.
///
/// A shell script sources relative to its own directory, and a CMake file
/// includes the same way, while a File node is named from the repository root.
/// So `source ./lib.sh` inside `scripts/build.sh` has to become
/// `scripts/lib.sh` or the edge lands nowhere. Targets that escape the
/// repository, or that interpolate a variable, are returned as written: they
/// will simply fail to match, which is better than inventing a path.
fn resolve_relative_target(relative_path: &str, target: &str) -> String {
    if target.starts_with('/') || target.contains('$') {
        return target.to_string();
    }
    let mut parts: Vec<&str> = relative_path.split('/').collect();
    parts.pop();
    for segment in target.split('/') {
        match segment {
            "." | "" => {}
            ".." => {
                if parts.pop().is_none() {
                    return target.to_string();
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Shell calls and `source` lines, resolved by walking rather than by query.
///
/// In shell, `source lib.sh` and `. lib.sh` are ordinary commands. Only the
/// command's own text separates an import from a call, and the query engine
/// here has no text predicates: writing `#eq?` would parse but never be
/// applied, so every command would be filed as an import. Walking is the
/// honest way to tell them apart, and it keeps `source` from becoming a call
/// edge to a function that does not exist.
fn extract_shell_references(
    out: &mut ExtractedFile,
    root: TsNode,
    source: &str,
    relative_path: &str,
) {
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
        if node.kind() != "command" {
            continue;
        }
        let Some(name_node) = node.child_by_field_name("name") else {
            continue;
        };
        let name = text(name_node, source);

        if name == "source" || name == "." {
            if let Some(argument) = node.child_by_field_name("argument") {
                out.imports.push(ImportRef {
                    target: resolve_relative_target(
                        relative_path,
                        &unquote(text(argument, source)),
                    ),
                    line: line_of(argument),
                });
            }
            continue;
        }

        out.calls.push(CallSite {
            callee_name: name.to_string(),
            receiver: None,
            line: line_of(name_node),
            character: utf16_column(name_node, source),
            byte: name_node.start_byte(),
        });
    }
}

/// CMake commands, split into the two that pull in another file and the rest.
///
/// Every CMake command is a `normal_command`, so `include(other.cmake)` and
/// `message(STATUS ...)` differ only in the identifier's text — the same
/// problem shell has, and the same reason a query cannot solve it here.
///
/// `add_subdirectory(src)` names a directory, and the file it actually pulls
/// in is that directory's `CMakeLists.txt`, so the target is spelled out
/// before resolution or the edge would point at a directory that is not a
/// node. Command names are matched without regard to case because CMake
/// treats them that way and real projects shout them.
fn extract_cmake_references(
    out: &mut ExtractedFile,
    root: TsNode,
    source: &str,
    relative_path: &str,
) {
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
        if node.kind() != "normal_command" {
            continue;
        }
        let Some(identifier) = node.child(0).filter(|n| n.kind() == "identifier") else {
            continue;
        };
        let name = text(identifier, source);

        let includes = name.eq_ignore_ascii_case("include");
        let descends = name.eq_ignore_ascii_case("add_subdirectory");
        if includes || descends {
            if let Some(argument) = first_cmake_argument(node) {
                let raw = unquote(text(argument, source));
                let target = if descends {
                    format!("{}/CMakeLists", raw.trim_end_matches('/'))
                } else {
                    raw
                };
                out.imports.push(ImportRef {
                    target: resolve_relative_target(relative_path, &target),
                    line: line_of(argument),
                });
            }
            continue;
        }

        out.calls.push(CallSite {
            callee_name: name.to_string(),
            receiver: None,
            line: line_of(identifier),
            character: utf16_column(identifier, source),
            byte: identifier.start_byte(),
        });
    }
}

/// Tags whose `src` or `href` names a file the project ships.
///
/// `<a href>` is absent on purpose. A link is navigation, not a dependency,
/// and in the templates this was measured on every one of the 144 of them held
/// either an external URL or a `{{ }}` expression, so importing them would
/// have added 144 edges to nodes that do not exist.
const HTML_LINKING_TAGS: &[&str] = &["script", "link", "img", "iframe", "source", "embed"];

/// What HTML contributes to the graph: the names a document exposes, and the
/// files it pulls in.
///
/// Walked rather than queried for the reason CMake is. Every attribute in this
/// grammar is an `attribute` holding an `attribute_name`, with nothing in the
/// node type to separate `src` from `charset`, and this query engine has no
/// text predicates.
///
/// An element carrying `id` is a definition because that is the name the rest
/// of the codebase addresses it by — `<div id="root">` is what the entry point
/// mounts onto. Ids are flat: HTML nesting is layout, not scope, and an id is
/// unique across the whole document by definition.
fn extract_html(
    out: &mut ExtractedFile,
    root: TsNode,
    source: &str,
    relative_path: &str,
    prefix: &str,
) {
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
        if !matches!(node.kind(), "start_tag" | "self_closing_tag") {
            continue;
        }
        let Some(tag) = node.child(1).filter(|n| n.kind() == "tag_name") else {
            continue;
        };
        let links = HTML_LINKING_TAGS.contains(&text(tag, source).to_ascii_lowercase().as_str());

        let mut attributes = node.walk();
        for attribute in node.children(&mut attributes) {
            if attribute.kind() != "attribute" {
                continue;
            }
            let Some(name) = attribute.child(0).filter(|n| n.kind() == "attribute_name") else {
                continue;
            };
            let name = text(name, source).to_ascii_lowercase();
            let Some(value) = html_attribute_value(attribute, source) else {
                continue;
            };

            if name == "id" {
                out.definitions.push(Definition {
                    label: NodeLabel::Field,
                    name: value.to_string(),
                    // `index.html` names no module of its own, by the same rule
                    // that makes `index.js` the package rather than a file in
                    // it, so its ids are top-level names with nothing to
                    // prefix.
                    qualified_name: if prefix.is_empty() {
                        value.to_string()
                    } else {
                        format!("{prefix}.{value}")
                    },
                    start_line: line_of(node),
                    end_line: end_line_of(node),
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    signature: None,
                    returns: None,
                });
            } else if links && matches!(name.as_str(), "src" | "href") {
                if let Some(target) = shipped_asset(relative_path, value) {
                    out.imports.push(ImportRef {
                        target,
                        line: line_of(attribute),
                    });
                }
            }
        }
    }
}

fn html_attribute_value<'a>(attribute: TsNode, source: &'a str) -> Option<&'a str> {
    let mut cursor = attribute.walk();
    let value = attribute.children(&mut cursor).find(|c| {
        matches!(
            c.kind(),
            "quoted_attribute_value" | "attribute_value" | "unquoted_attribute_value"
        )
    })?;
    if value.kind() != "quoted_attribute_value" {
        return Some(text(value, source));
    }
    let mut inner = value.walk();
    let inner = value
        .children(&mut inner)
        .find(|c| c.kind() == "attribute_value")?;
    Some(text(inner, source))
}

/// The path a link points at, or `None` when it does not name a file in this
/// project.
///
/// A root-absolute path is resolved against the document's own directory,
/// which is where the web root sits for the entry point that carries these
/// links. That is a convention, not a fact the source states, so it is the one
/// guess here and it is confined to this function.
fn shipped_asset(relative_path: &str, target: &str) -> Option<String> {
    let target = target.trim();
    // A template expression is not a path, and neither is an anchor, an
    // external URL, a protocol-relative host, or inline data.
    if target.is_empty()
        || target.contains("{{")
        || target.starts_with('#')
        || target.starts_with("//")
        || target.contains("://")
        || target.split_once(':').is_some_and(|(s, _)| {
            s.chars()
                .all(|c| c.is_ascii_alphabetic() || matches!(c, '+' | '-' | '.'))
        })
    {
        return None;
    }
    let target = target.split(['?', '#']).next()?;
    Some(resolve_relative_target(
        relative_path,
        target.trim_start_matches('/'),
    ))
}

fn first_cmake_argument(command: TsNode) -> Option<TsNode> {
    let mut cursor = command.walk();
    let arguments = command
        .children(&mut cursor)
        .find(|c| c.kind() == "argument_list")?;
    let mut inner = arguments.walk();
    let first = arguments
        .children(&mut inner)
        .find(|c| c.kind() == "argument");
    first
}

fn extract_references(
    out: &mut ExtractedFile,
    grammar: &tree_sitter::Language,
    spec: &LanguageSpec,
    root: TsNode,
    source: &str,
) -> Result<()> {
    let Some(query) = compile(spec.references, grammar, "references")? else {
        return Ok(());
    };

    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source.as_bytes());

    while let Some(m) = matches.next() {
        let mut callee: Option<TsNode> = None;
        let mut receiver: Option<TsNode> = None;
        let mut import_target: Option<TsNode> = None;
        let mut subtype: Option<TsNode> = None;
        let mut extends: Option<TsNode> = None;
        let mut implements: Option<TsNode> = None;

        for capture in m.captures {
            match capture_names[capture.index as usize] {
                "call.name" => callee = Some(capture.node),
                "call.receiver" => receiver = Some(capture.node),
                "import.name" => import_target = Some(capture.node),
                "subtype" => subtype = Some(capture.node),
                "extends" => extends = Some(capture.node),
                "implements" => implements = Some(capture.node),
                _ => {}
            }
        }

        if let Some(node) = callee {
            out.calls.push(CallSite {
                callee_name: text(node, source).to_string(),
                receiver: receiver.map(|r| text(r, source).to_string()),
                line: line_of(node),
                character: utf16_column(node, source),
                byte: node.start_byte(),
            });
        }
        if let Some(node) = import_target {
            out.imports.push(ImportRef {
                target: unquote(text(node, source)),
                line: line_of(node),
            });
        }
        if let Some(sub) = subtype {
            let sub_name = text(sub, source).to_string();
            if let Some(node) = extends {
                out.type_relations.push(TypeRel {
                    subtype: sub_name.clone(),
                    supertype: text(node, source).to_string(),
                    kind: TypeRelKind::Inherits,
                    line: line_of(node),
                });
            }
            if let Some(node) = implements {
                out.type_relations.push(TypeRel {
                    subtype: sub_name,
                    supertype: text(node, source).to_string(),
                    kind: TypeRelKind::Implements,
                    line: line_of(node),
                });
            }
        }
    }

    out.calls.sort_by_key(|c| c.byte);
    out.calls.dedup();
    out.imports.dedup();
    out.type_relations.dedup();
    Ok(())
}

/// Recognise framework route registrations that are visible in the AST.
///
/// A match must carry both an HTTP verb (or a known registrar) and a literal
/// path. Anything else produces no Route node.
fn extract_routes(
    out: &mut ExtractedFile,
    grammar: &tree_sitter::Language,
    spec: &LanguageSpec,
    root: TsNode,
    source: &str,
    language: LanguageId,
) -> Result<()> {
    let Some(query) = compile(spec.routes, grammar, "routes")? else {
        return Ok(());
    };

    let prefixes = group_prefixes(root, source);

    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source.as_bytes());

    while let Some(m) = matches.next() {
        let mut method_node: Option<TsNode> = None;
        let mut path_node: Option<TsNode> = None;
        let mut handler_node: Option<TsNode> = None;
        let mut route_node: Option<TsNode> = None;
        let mut router_node: Option<TsNode> = None;

        for capture in m.captures {
            match capture_names[capture.index as usize] {
                "route.method" => method_node = Some(capture.node),
                // Only the first literal argument is the path.
                "route.path" => {
                    path_node.get_or_insert(capture.node);
                }
                "route.handler" => handler_node = Some(capture.node),
                "route.router" => router_node = Some(capture.node),
                "route" => route_node = Some(capture.node),
                _ => {}
            }
        }

        let (Some(method_node), Some(path_node)) = (method_node, path_node) else {
            continue;
        };

        // ASP.NET spells the verb as an attribute name (`HttpGet`), so the
        // prefix is stripped before the verb is recognised.
        let raw_method = text(method_node, source).to_lowercase();
        let raw_method = match raw_method.strip_prefix("http") {
            Some(rest) if HTTP_VERBS.contains(&rest) => rest.to_string(),
            _ => raw_method,
        };
        let is_verb = HTTP_VERBS.contains(&raw_method.as_str());
        let is_registrar = ROUTE_REGISTRARS.contains(&raw_method.as_str());
        if !is_verb && !is_registrar {
            continue;
        }

        let path = unquote(text(path_node, source));
        let prefix = router_node
            .map(|node| text(node, source))
            .and_then(|router| prefixes.get(router))
            .map(String::as_str);

        // Route paths are literal URL paths; anything else is a false positive.
        // The exception is an empty path on a group, which is how a framework
        // spells the group's own root — `customers.GET("", …)` is
        // `/v1/customers`, not a stray string.
        if !path.starts_with('/') && !(path.is_empty() && prefix.is_some()) {
            continue;
        }
        let path = join_route(prefix, &path);

        let anchor = route_node.unwrap_or(method_node);

        // A registrar such as axum's `.route(path, get(handler))` carries the
        // verb on the wrapper call around the handler, not on the registrar.
        let (handler, wrapped_verb) = match handler_node {
            Some(node) => (Some(handler_reference(node, source)), None),
            None => handler_from_arguments(source, path_node),
        };
        let (handler_name, handler_receiver) = match handler {
            Some(reference) => (Some(reference.name), reference.receiver),
            None => (None, None),
        };

        let method = match (is_verb, wrapped_verb.as_deref()) {
            (true, _) => raw_method.to_uppercase(),
            (false, Some(verb)) => verb.to_uppercase(),
            // A registrar with no verb in sight really does accept any method.
            (false, None) => "ANY".to_string(),
        };

        let framework_hint = framework_hint(language, &raw_method);

        // A chained builder (`Router::new().route(..).route(..)`) makes the
        // matched call span the whole chain, so anchor to the path literal
        // unless the anchor genuinely starts on the same line.
        let (line, end_line) = if line_of(anchor) == line_of(path_node) {
            (line_of(anchor), end_line_of(anchor))
        } else {
            (line_of(path_node), end_line_of(path_node))
        };

        out.routes.push(RouteDef {
            method,
            path,
            handler_name,
            handler_receiver,
            line,
            end_line,
            framework_hint,
        });
    }

    out.routes.dedup();
    Ok(())
}

/// Path prefix each router variable in the file carries.
///
/// A service registers its routes on nested groups — `v1 := router.Group("/v1")`
/// then `auth := v1.Group("/auth")` — and stores only the last segment against
/// the route. Without the prefix, `/login` is what lands in the graph, and in
/// one real project 760 routes collapsed onto 477 names with `/:id` repeated
/// 116 times. Routes that cannot be told apart cannot answer anything.
///
/// Declarations are read in source order, so a group's own prefix is already
/// known by the time a group nested inside it is read. A router built any other
/// way simply has no prefix, which leaves the path exactly as it was before.
fn group_prefixes(root: TsNode, source: &str) -> BTreeMap<String, String> {
    let mut prefixes: BTreeMap<String, String> = BTreeMap::new();

    for node in assignments(root) {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            continue;
        };

        let Some(variable) = first_identifier(left, source) else {
            continue;
        };
        let Some((parent, prefix)) = group_call(right, source) else {
            continue;
        };

        let inherited = prefixes.get(&parent).map(String::as_str);
        prefixes.insert(variable, join_route(inherited, &prefix));
    }

    prefixes
}

/// Assignments in source order, so a value is read before anything built on it.
fn assignments(root: TsNode) -> Vec<TsNode> {
    let mut stack = vec![root];
    let mut ordered = Vec::new();
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "short_var_declaration" | "assignment_statement" | "variable_declaration"
        ) {
            ordered.push(node);
        }
        let mut cursor = node.walk();
        let children: Vec<TsNode> = node.named_children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    ordered
}

/// Record which function each local variable takes its type from.
///
/// A variable that is assigned twice in one file is dropped rather than
/// guessed at: scope is not tracked here, so two functions each declaring their
/// own `h` are indistinguishable, and a wrong receiver is worse than none.
fn extract_receiver_bindings(out: &mut ExtractedFile, root: TsNode, source: &str) {
    let mut seen: BTreeMap<String, Option<String>> = BTreeMap::new();

    for node in assignments(root) {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            continue;
        };
        let Some(variable) = first_identifier(left, source) else {
            continue;
        };
        let Some(call) = find_call_expression(right) else {
            continue;
        };
        let Some(function) = call.child_by_field_name("function") else {
            continue;
        };
        // `NewZoneHandler(…)` and `handlers.NewZoneHandler(…)` name the same
        // function; the package qualifier is not part of its name in the graph.
        let constructor = handler_reference(function, source).name;

        match seen.get(&variable) {
            Some(Some(existing)) if existing != &constructor => {
                seen.insert(variable, None);
            }
            Some(None) => {}
            _ => {
                seen.insert(variable, Some(constructor));
            }
        }
    }

    out.receiver_bindings = seen
        .into_iter()
        .filter_map(|(variable, constructor)| {
            Some(ReceiverBinding {
                variable,
                constructor: constructor?,
            })
        })
        .collect();
}

/// Read `parent.Group("/prefix")`, returning the parent and the literal prefix.
fn group_call(node: TsNode, source: &str) -> Option<(String, String)> {
    let call = find_call_expression(node)?;
    let function = call.child_by_field_name("function")?;
    if !matches!(
        function.kind(),
        "selector_expression" | "member_expression" | "field_expression"
    ) {
        return None;
    }

    let field = ["field", "property"]
        .iter()
        .find_map(|name| function.child_by_field_name(name))?;
    if !spec::ROUTE_GROUPERS.contains(&text(field, source).to_lowercase().as_str()) {
        return None;
    }

    let operand = ["operand", "object", "value"]
        .iter()
        .find_map(|name| function.child_by_field_name(name))?;
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    // A group can be opened with an empty path and only middleware, which
    // contributes nothing of its own but still inherits its parent.
    let prefix = arguments
        .named_children(&mut cursor)
        .next()
        .map(|first| unquote(text(first, source)))
        .filter(|first| first.starts_with('/'))
        .unwrap_or_default();

    Some((text(operand, source).to_string(), prefix))
}

/// The call inside an expression list, or the node itself when it is one.
fn find_call_expression(node: TsNode) -> Option<TsNode> {
    if node.kind() == "call_expression" {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<TsNode> = node.named_children(&mut cursor).collect();
    children.into_iter().find_map(find_call_expression)
}

/// Join a group prefix to a route path without doubling or dropping slashes.
fn join_route(prefix: Option<&str>, path: &str) -> String {
    let prefix = prefix.unwrap_or_default().trim_end_matches('/');
    if prefix.is_empty() {
        return path.to_string();
    }
    // A group's own root is the prefix itself, not the prefix with a trailing
    // slash bolted on.
    if path == "/" {
        return prefix.to_string();
    }
    format!("{prefix}{path}")
}

/// A handler as the registration writes it.
pub struct HandlerRef {
    pub name: String,
    pub receiver: Option<String>,
}

/// Read a handler expression, keeping the method apart from its receiver.
///
/// `handlers.Login` and `h.Login` both name `Login`. Taking the first
/// identifier instead yields `handlers` or `h` — an object, which resolution
/// then hunts for among functions and never finds. That single confusion left
/// 758 routes in one project with no edge to the code that serves them.
fn handler_reference(node: TsNode, source: &str) -> HandlerRef {
    let selector = matches!(
        node.kind(),
        "selector_expression" | "member_expression" | "field_expression" | "attribute"
    );

    if selector {
        // Grammars disagree on the field name, so try each and fall back to the
        // last identifier, which is the method in all of these shapes.
        let field = ["field", "property", "attribute"]
            .iter()
            .find_map(|name| node.child_by_field_name(name))
            .or_else(|| last_identifier_node(node));
        let object = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("operand"))
            .or_else(|| node.child_by_field_name("value"));

        if let Some(field) = field {
            return HandlerRef {
                name: text(field, source).to_string(),
                receiver: object.map(|o| text(o, source).to_string()),
            };
        }
    }

    HandlerRef {
        name: first_identifier(node, source).unwrap_or_else(|| text(node, source).to_string()),
        receiver: None,
    }
}

/// Deepest-last identifier under a node, which is the member being selected.
fn last_identifier_node(node: TsNode) -> Option<TsNode> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).last().and_then(|last| {
        if matches!(
            last.kind(),
            "identifier" | "field_identifier" | "property_identifier"
        ) {
            Some(last)
        } else {
            last_identifier_node(last)
        }
    })
}

/// Find the handler passed alongside a route path.
///
/// Returns the handler and, when it is wrapped in a verb call such as axum's
/// `get(handler)`, the verb that wrapper names.
fn handler_from_arguments(source: &str, path_node: TsNode) -> (Option<HandlerRef>, Option<String>) {
    let Some(args) = path_node.parent() else {
        return (None, None);
    };
    let mut cursor = args.walk();
    let children: Vec<TsNode> = args.named_children(&mut cursor).collect();
    let Some(path_at) = children.iter().position(|c| c.id() == path_node.id()) else {
        return (None, None);
    };
    let after = &children[path_at + 1..];

    // A verb wrapper such as axum's `get(handler)` names both the verb and the
    // handler, so it wins wherever it sits.
    for child in after {
        if let Some((verb, handler)) = verb_wrapped_handler(*child, source) {
            return (Some(handler), Some(verb));
        }
    }

    // Otherwise the handler is the last argument. Everything between it and the
    // path is middleware — `POST(path, RequirePermission(…), h.Create)` — and
    // reading the first argument instead pointed 8 routes at their rate limiter.
    let Some(last) = after.last() else {
        return (None, None);
    };
    // A closure is a handler with no name to link to; the route still stands on
    // its own.
    if last.kind().contains("func_literal") || last.kind().contains("closure") {
        return (None, None);
    }
    (Some(handler_reference(*last, source)), None)
}

/// Recognise `get(handler)` / `post(handler)` style wrappers.
fn verb_wrapped_handler(node: TsNode, source: &str) -> Option<(String, HandlerRef)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let function = node.child_by_field_name("function")?;
    let verb = text(function, source).to_lowercase();
    if !HTTP_VERBS.contains(&verb.as_str()) {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let handler = arguments.named_children(&mut cursor).next()?;
    Some((verb, handler_reference(handler, source)))
}

fn first_identifier(node: TsNode, source: &str) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "property_identifier" | "type_identifier"
    ) {
        return Some(text(node, source).to_string());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = first_identifier(child, source) {
            return Some(found);
        }
    }
    None
}

/// Name the framework only when the evidence supports it.
fn framework_hint(language: LanguageId, method: &str) -> String {
    match language {
        // The decorator shape is identical across FastAPI, Flask and APIRouter,
        // so naming a specific framework here would be a guess.
        LanguageId::Python => "python_decorator_route".to_string(),
        LanguageId::JavaScript | LanguageId::Jsx | LanguageId::TypeScript | LanguageId::Tsx => {
            "express_style_route".to_string()
        }
        LanguageId::Go => {
            if method == "handlefunc" || method == "handle" {
                "go_net_http".to_string()
            } else {
                "go_router_verb".to_string()
            }
        }
        LanguageId::Rust => "axum_style_route".to_string(),
        other => format!("{other}_route"),
    }
}

/// Count definitions by label, for coverage reporting and tests.
pub fn label_histogram(extracted: &ExtractedFile) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for def in &extracted.definitions {
        *counts.entry(def.label.as_str()).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod shell_target_tests {
    use super::resolve_relative_target;

    #[test]
    fn a_sibling_resolves_against_the_script_directory() {
        assert_eq!(
            resolve_relative_target("scripts/build.sh", "./lib.sh"),
            "scripts/lib.sh"
        );
        assert_eq!(
            resolve_relative_target("scripts/build.sh", "lib.sh"),
            "scripts/lib.sh"
        );
    }

    #[test]
    fn parent_segments_are_followed() {
        assert_eq!(
            resolve_relative_target("packaging/deb/postinst", "../common/env.sh"),
            "packaging/common/env.sh"
        );
    }

    #[test]
    fn a_script_at_the_root_keeps_a_bare_name() {
        assert_eq!(resolve_relative_target("build.sh", "./lib.sh"), "lib.sh");
    }

    /// Nothing in the repository can match these, and guessing a path would be
    /// worse than recording the target as written.
    #[test]
    fn targets_that_cannot_be_resolved_are_left_alone() {
        assert_eq!(
            resolve_relative_target("scripts/build.sh", "/etc/profile"),
            "/etc/profile"
        );
        assert_eq!(
            resolve_relative_target("scripts/build.sh", "$HOME/lib.sh"),
            "$HOME/lib.sh"
        );
        assert_eq!(
            resolve_relative_target("build.sh", "../outside.sh"),
            "../outside.sh"
        );
    }
}
