use serde_json::{json, Value};

/// One MCP tool: the name the agent calls, the contract it reads, and the input
/// schema it must satisfy.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
}

fn project_property() -> Value {
    json!({
        "type": "string",
        "description": "Project id from list_projects. Not a directory name."
    })
}

fn pagination_properties(default_limit: u32) -> (Value, Value) {
    (
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 1000,
            "default": default_limit,
            "description": "Maximum results in this page."
        }),
        json!({
            "type": "string",
            "description": "Cursor from a previous response's `cursor` field. Pass every other argument unchanged."
        }),
    )
}

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "list_projects",
        description: "List the repositories loci has indexed, with each project's id, absolute \
                      root and store path. CALL THIS FIRST in a new session: every other tool \
                      needs the exact `project` id, and guessing it from a folder name will fail \
                      with project_not_found. If the repository you need is absent, index it with \
                      index_repository.",
        schema: || {
            let (limit, cursor) = pagination_properties(50);
            json!({
                "type": "object",
                "properties": { "limit": limit, "cursor": cursor },
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "index_repository",
        description: "Parse a repository into the persistent graph. Incremental by default: files \
                      whose content hash is unchanged are not re-parsed, and cross-file edges are \
                      rebuilt every run so callers of a moved symbol stay correct. Pass full=true \
                      to force a complete rebuild. The response reports what was indexed, what was \
                      parse_partial (indexed, but the parser hit errors in the listed line ranges) \
                      and what was skipped, with reasons. Files excluded by .gitignore are not \
                      enumerated by design; classify one with check_index_coverage.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "repo_path": {
                        "type": "string",
                        "description": "Absolute path to the repository root. loci reads only under this path."
                    },
                    "name": {
                        "type": "string",
                        "description": "Override the project id. Defaults to the directory basename, \
                                        suffixed with a hash of the root if that name is taken."
                    },
                    "full": {
                        "type": "boolean",
                        "default": false,
                        "description": "Re-parse every file, ignoring content hashes."
                    },
                    "hybrid_lsp": {
                        "type": "boolean",
                        "default": false,
                        "description": "Ask installed language servers about calls the AST could \
                                        not resolve, such as a method reached through a variable. \
                                        Adds seconds to the run and needs the server on PATH; the \
                                        report says exactly what it resolved. Edges it settles \
                                        carry source=\"lsp\"."
                    }
                },
                "required": ["repo_path"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "delete_project",
        description: "Remove a project's graph from disk and drop its catalog entry. The source \
                      repository is never touched. Irreversible: re-indexing is the only way back.",
        schema: || {
            json!({
                "type": "object",
                "properties": { "project": project_property() },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "index_status",
        description: "Health of one project's index: node and edge counts, languages, root path, \
                      when it was built, and the coverage report. Use this to decide whether the \
                      graph is fresh enough to trust before relying on it, and detect_changes to \
                      see exactly which files drifted.",
        schema: || {
            json!({
                "type": "object",
                "properties": { "project": project_property() },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "check_index_coverage",
        description: "Authoritative coverage for exact paths or path prefixes. CALL THIS BEFORE ANY \
                      NEGATIVE OR EXHAUSTIVE CLAIM: a fully skipped file cannot appear in graph \
                      results, so its absence there proves nothing. Returns one of indexed, \
                      parse_partial, skipped or excluded, plus the reason and what to do instead \
                      (usually: read the source directly). Omit paths and scopes to classify the \
                      whole project (scope \".\"). Best-effort by design; `indexed` is not \
                      a guarantee of completeness.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": 128,
                        "description": "Repository-relative file paths to classify exactly."
                    },
                    "scopes": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": 32,
                        "description": "Repository-relative directory prefixes; use \".\" for the whole project."
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200 },
                    "cursor": { "type": "string" }
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "detect_changes",
        description: "Compare the working tree against the hashes recorded at the last index run \
                      and report added, modified and removed files. For modified and removed files \
                      it lists the symbols the graph still attributes to them, which is the set at \
                      risk of being stale. Filesystem-based, so it works without git.",
        schema: || {
            let (limit, cursor) = pagination_properties(200);
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "limit": limit,
                    "cursor": cursor
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "search_graph",
        description: "Find symbols in the code graph by name, qualified name, regex, label or file \
                      path. USE THIS INSTEAD OF GREP to locate definitions. This is exact \
                      structural lookup over an index, not ranked text search: `name` is an exact \
                      case-insensitive match, `name_pattern` is a regex. Every hit carries the file \
                      path and line range, plus in/out degree over graph edges. Results are \
                      paginated; page with `cursor` while has_more is true.",
        schema: || {
            let (limit, cursor) = pagination_properties(50);
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "name": {
                        "type": "string",
                        "description": "Exact simple name, case-insensitive. The cheapest lookup."
                    },
                    "query": {
                        "type": "string",
                        "description": "Alias for name, or for name_pattern when the value looks like a regex."
                    },
                    "qualified_name": {
                        "type": "string",
                        "description": "Exact fully qualified name, e.g. app.service.OrderService.create_order."
                    },
                    "name_pattern": {
                        "type": "string",
                        "description": "Regex over the simple name, e.g. \"^handle_.*\"."
                    },
                    "label": {
                        "type": "string",
                        "enum": ["Project", "File", "Package", "Module", "Function", "Method",
                                 "Class", "Type", "Interface", "Trait", "Struct", "Enum", "Field",
                                 "Route", "Adr", "TraceSpan"],
                        "description": "Restrict to one node label."
                    },
                    "file_pattern": {
                        "type": "string",
                        "description": "Regex over the repository-relative file path."
                    },
                    "limit": limit,
                    "cursor": cursor
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "query_graph",
        description: "Run a multi-hop pattern over the graph: pick start nodes the same way \
                      search_graph does, then walk named edge types. This is an explicit pattern \
                      walker, NOT Cypher and NOT a query language. Example — routes and the \
                      handlers they reach: start={\"label\":\"Route\"}, hops=[{\"edge\":\"ROUTES_TO\"}]. \
                      Each result row is the full path, so you can see every node the walk passed \
                      through.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "start": {
                        "type": "object",
                        "description": "Start-node selector; same fields as search_graph.",
                        "properties": {
                            "name": { "type": "string" },
                            "query": { "type": "string" },
                            "qualified_name": { "type": "string" },
                            "name_pattern": { "type": "string" },
                            "label": { "type": "string" },
                            "file_pattern": { "type": "string" }
                        },
                        "additionalProperties": false
                    },
                    "hops": {
                        "type": "array",
                        "description": "Edges to walk, in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "edge": {
                                    "type": "string",
                                    "enum": ["CONTAINS", "DEFINES", "CALLS", "CALL_UNRESOLVED",
                                             "IMPORTS", "INHERITS", "IMPLEMENTS", "HAS_FIELD",
                                             "ROUTES_TO", "CONFIG_REF", "PROTO_REF", "OPENAPI_REF",
                                             "IMPACTS"]
                                },
                                "edge_type": {
                                    "type": "string",
                                    "description": "Alias for edge; this is the name get_graph_schema uses."
                                },
                                "direction": {
                                    "type": "string",
                                    "enum": ["out", "in", "outbound", "inbound"],
                                    "default": "out",
                                    "description": "out follows the edge forward; in follows it backward."
                                },
                                "label": {
                                    "type": "string",
                                    "description": "Filter the node reached by this hop."
                                }
                            },
                            "required": [],
                            "additionalProperties": false
                        }
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 },
                    "cursor": { "type": "string" }
                },
                "required": ["project", "start"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "trace_path",
        description: "Walk the call graph from one symbol: inbound gives transitive callers (the \
                      blast radius of changing it), outbound gives what it reaches. USE THIS \
                      INSTEAD OF GREPPING for callers. Also returns `unresolved`: calls that were \
                      seen in the source but could not be pinned to a definition, with the reason. \
                      Read that list before concluding nothing calls something.",
        schema: || {
            let (limit, cursor) = pagination_properties(100);
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "qualified_name": {
                        "type": "string",
                        "description": "Exact qualified name from search_graph. Preferred: a bare \
                                        name that matches several symbols returns ambiguous_symbol."
                    },
                    "from": {
                        "type": "string",
                        "description": "Alias for qualified_name."
                    },
                    "name": {
                        "type": "string",
                        "description": "Simple name, used only when it is unique in the project."
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["inbound", "outbound", "both", "in", "out", "callers", "callees"],
                        "default": "both"
                    },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 10, "default": 3 },
                    "limit": limit,
                    "cursor": cursor
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "get_code_snippet",
        description: "Read the exact source of a symbol. This is a READ tool, not a search tool: \
                      get the qualified_name from search_graph first. Source is read live from the \
                      file under the project root, so it is always current even if the index is \
                      stale. An ambiguous name returns the candidate list instead of a guess.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "qualified_name": {
                        "type": "string",
                        "description": "Exact qualified name from search_graph, or a simple name if unique."
                    },
                    "name": {
                        "type": "string",
                        "description": "Alias for qualified_name when the simple name is unique."
                    },
                    "context_lines": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 50,
                        "default": 0,
                        "description": "Extra lines to include before and after the symbol."
                    }
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "get_graph_schema",
        description: "The graph's vocabulary: node labels, edge types, evidence values, and the \
                      exact list of tree-sitter grammars compiled into this binary. Call it when \
                      you need to know which languages are actually covered or which edge names \
                      query_graph accepts. Languages absent from bundled_languages are not parsed \
                      at all.",
        schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    },
    ToolDef {
        name: "get_architecture",
        description: "Counted overview of one project: languages, node and edge totals, HTTP \
                      routes with their handlers, entry points, and the largest files by symbol \
                      count. Every number comes from the graph; nothing is inferred. Good for \
                      orienting in an unfamiliar repository before drilling in with search_graph.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "path": {
                        "type": "string",
                        "description": "Optional repository-relative directory prefix to scope the report."
                    }
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "search_code",
        description: "Literal or regex text search across the indexed files. Use this only for \
                      things the graph does not model — string constants, comments, config values, \
                      TODO markers — or when check_index_coverage shows graph coverage is \
                      insufficient. For finding definitions or callers, search_graph and trace_path \
                      are both faster and more precise. Respects .gitignore and stays under the \
                      project root.",
        schema: || {
            let (limit, cursor) = pagination_properties(50);
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "pattern": { "type": "string", "description": "Literal text, or a regex when regex=true." },
                    "query": { "type": "string", "description": "Alias for pattern." },
                    "regex": { "type": "boolean", "default": false },
                    "case_sensitive": { "type": "boolean", "default": true },
                    "file_pattern": {
                        "type": "string",
                        "description": "Regex over the repository-relative file path, e.g. \"\\\\.go$\"."
                    },
                    "path": {
                        "type": "string",
                        "description": "Alias for file_pattern."
                    },
                    "limit": limit,
                    "cursor": cursor
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "manage_adr",
        description: "Architecture decision records stored beside the graph and linked to real \
                      symbols. `related_qualified_names` are validated against the graph on write: \
                      names that do not resolve are returned in `unresolved` rather than silently \
                      accepted, so an ADR cannot point at a symbol that does not exist.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "mode": {
                        "type": "string",
                        "enum": ["list", "get", "upsert", "delete"],
                        "default": "list"
                    },
                    "id": { "type": "string", "description": "Required for get, upsert and delete." },
                    "title": { "type": "string" },
                    "status": {
                        "type": "string",
                        "enum": ["proposed", "accepted", "superseded", "rejected"],
                        "default": "proposed"
                    },
                    "body": { "type": "string", "description": "Markdown body of the decision." },
                    "related_qualified_names": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Symbols this decision governs. Checked against the graph."
                    }
                },
                "required": ["project"],
                "additionalProperties": false
            })
        },
    },
    ToolDef {
        name: "ingest_traces",
        description: "Add observed runtime caller/callee pairs to the graph as IMPACTS edges. A \
                      pair is only linked when BOTH endpoints already resolve to indexed symbols; \
                      unmatched pairs are returned in `unmatched` rather than creating placeholder \
                      nodes. Use it to record call paths that static analysis left as \
                      CALL_UNRESOLVED, such as dynamic dispatch.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": project_property(),
                    "traces": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "caller": { "type": "string", "description": "Qualified name or unique simple name." },
                                "callee": { "type": "string", "description": "Qualified name or unique simple name." },
                                "count": { "type": "integer", "minimum": 1, "default": 1 }
                            },
                            "required": ["caller", "callee"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["project", "traces"],
                "additionalProperties": false
            })
        },
    },
];

pub fn tool_list() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
            })
        })
        .collect();
    json!({ "tools": tools })
}

pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|tool| tool.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_fifteen_tools_are_exposed() {
        assert_eq!(TOOLS.len(), 15);
    }

    #[test]
    fn tool_names_match_the_product_contract() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "check_index_coverage",
                "delete_project",
                "detect_changes",
                "get_architecture",
                "get_code_snippet",
                "get_graph_schema",
                "index_repository",
                "index_status",
                "ingest_traces",
                "list_projects",
                "manage_adr",
                "query_graph",
                "search_code",
                "search_graph",
                "trace_path",
            ]
        );
    }

    #[test]
    fn every_schema_is_a_valid_object_schema() {
        for tool in TOOLS {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{} schema", tool.name);
            assert!(schema["properties"].is_object(), "{} properties", tool.name);
        }
    }

    #[test]
    fn every_tool_but_the_global_ones_requires_a_project() {
        for tool in TOOLS {
            if matches!(
                tool.name,
                "list_projects" | "index_repository" | "get_graph_schema"
            ) {
                continue;
            }
            let schema = (tool.schema)();
            let required = schema["required"].as_array().expect(tool.name);
            assert!(
                required.iter().any(|r| r == "project"),
                "{} must require project",
                tool.name
            );
        }
    }

    #[test]
    fn descriptions_are_substantial_enough_to_steer_an_agent() {
        for tool in TOOLS {
            assert!(
                tool.description.len() > 80,
                "{} description is too thin to act as a contract",
                tool.name
            );
        }
    }

    #[test]
    fn paginated_tools_document_a_cursor() {
        for name in [
            "list_projects",
            "search_graph",
            "trace_path",
            "search_code",
            "detect_changes",
        ] {
            let schema = (find(name).unwrap().schema)();
            assert!(
                schema["properties"]["cursor"].is_object(),
                "{name} must expose a cursor"
            );
        }
    }
}
