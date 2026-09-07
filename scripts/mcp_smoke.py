#!/usr/bin/env python3
"""Drive the loci MCP server over stdio and exercise every advertised tool.

This is the integration check that matters for Cursor: it speaks the same
JSON-RPC framing Cursor does, against the real binary, and fails loudly if a
tool is missing, errors unexpectedly, or returns a shape an agent cannot use.
"""

import json
import subprocess
import sys

EXPECTED_TOOLS = [
    "list_projects",
    "index_repository",
    "delete_project",
    "index_status",
    "check_index_coverage",
    "detect_changes",
    "search_graph",
    "query_graph",
    "trace_path",
    "get_code_snippet",
    "get_graph_schema",
    "get_architecture",
    "search_code",
    "manage_adr",
    "ingest_traces",
]


class Server:
    def __init__(self, binary):
        self.proc = subprocess.Popen(
            [binary, "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.next_id = 0

    def send(self, method, params=None):
        self.next_id += 1
        message = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            message["params"] = params
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise SystemExit(f"server closed the stream during {method}")
        return json.loads(line)

    def call(self, tool, arguments):
        response = self.send("tools/call", {"name": tool, "arguments": arguments})
        result = response.get("result")
        if result is None:
            raise SystemExit(f"{tool}: transport error {response.get('error')}")
        payload = json.loads(result["content"][0]["text"])
        return payload, bool(result.get("isError"))

    def close(self):
        self.proc.stdin.close()
        self.proc.terminate()
        self.proc.wait(timeout=10)


def main():
    binary, fixture, project = sys.argv[1], sys.argv[2], sys.argv[3]
    server = Server(binary)
    checked = set()

    def ok(tool, arguments, predicate, why):
        payload, is_error = server.call(tool, arguments)
        if is_error:
            raise SystemExit(f"{tool}: unexpected error {payload}")
        if not predicate(payload):
            raise SystemExit(f"{tool}: {why}\n{json.dumps(payload, indent=2)[:800]}")
        checked.add(tool)
        print(f"  ok  {tool}")

    init = server.send(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0"},
        },
    )
    caps = init["result"]
    assert "tools" in caps["capabilities"], "server must advertise tool capability"
    assert caps.get("instructions"), "server must ship usage instructions for the agent"
    print(f"  ok  initialize (protocol {caps['protocolVersion']})")

    listed = server.send("tools/list")["result"]["tools"]
    names = sorted(t["name"] for t in listed)
    if names != sorted(EXPECTED_TOOLS):
        raise SystemExit(f"tool list mismatch\nadvertised: {names}")
    for tool in listed:
        if not tool.get("description"):
            raise SystemExit(f"{tool['name']} has no description; agents need one")
        schema = tool["inputSchema"]
        if schema.get("type") != "object":
            raise SystemExit(f"{tool['name']} input schema is not an object")
    print(f"  ok  tools/list ({len(listed)} tools, all described)")

    ok(
        "index_repository",
        {"repo_path": fixture, "name": project},
        lambda p: p["nodes"] > 0 and p["files_indexed"] > 0,
        "indexing must produce a non-empty graph",
    )
    ok(
        "list_projects",
        {},
        lambda p: any(x["project"] == project for x in p["projects"]),
        "the freshly indexed project must be listed",
    )
    ok(
        "index_status",
        {"project": project},
        lambda p: p["nodes"] > 0 and p["languages"],
        "status must report real graph counts",
    )
    ok(
        "check_index_coverage",
        {"project": project, "paths": ["services/orders-py/app/service.py"]},
        lambda p: p["paths"][0]["status"] == "indexed",
        "a known-indexed file must classify as indexed",
    )
    ok(
        "check_index_coverage",
        {"project": project, "paths": ["ignored/secret.py"]},
        lambda p: p["paths"][0]["status"] != "indexed",
        "a gitignored file must not be reported as indexed",
    )
    ok(
        "detect_changes",
        {"project": project},
        lambda p: p["added"] == 0 and p["modified"] == 0 and p["removed"] == 0,
        "a just-indexed tree must be clean",
    )
    ok(
        "search_graph",
        {"project": project, "name": "create_order"},
        lambda p: p["results"]
        and all(r["file_path"] and r["start_line"] > 0 for r in p["results"]),
        "results must carry a concrete file path and start line",
    )
    ok(
        "search_graph",
        {"project": project, "name": "definitely_no_such_symbol_xyz"},
        lambda p: p["results"] == [],
        "a miss must be empty, not invented",
    )
    ok(
        "query_graph",
        {
            "project": project,
            "start": {"label": "Route"},
            "hops": [{"edge": "ROUTES_TO", "direction": "out"}],
        },
        lambda p: p["rows"] and all(len(r["path"]) == 2 for r in p["rows"]),
        "the fixture's routes must reach their handlers",
    )

    # An ambiguous simple name must produce candidates, never a guess.
    payload, _ = server.call(
        "trace_path",
        {"project": project, "name": "create_order", "direction": "outbound"},
    )
    if not payload.get("candidates") or not payload.get("error"):
        raise SystemExit(f"ambiguous symbol must return candidates, got {payload}")
    print("  ok  ambiguity is reported, not guessed")

    ok(
        "trace_path",
        {
            "project": project,
            "qualified_name": "services.orders-py.app.service.OrderService.create_order",
            "direction": "outbound",
            "depth": 3,
        },
        lambda p: {n["name"] for n in p["callees"]}
        >= {"validate_order", "price_order", "save_order", "_allocate_id"},
        "outbound trace must cross files and reach the second hop",
    )
    ok(
        "get_code_snippet",
        {
            "project": project,
            "qualified_name": "services.orders-py.app.service.OrderService.create_order",
        },
        lambda p: "def create_order" in p["source"],
        "snippet must return the real source of that symbol",
    )
    ok(
        "get_graph_schema",
        {},
        lambda p: p["node_labels"] and p["edge_types"],
        "schema must enumerate labels and edges",
    )
    ok(
        "get_architecture",
        {"project": project},
        lambda p: p["languages"],
        "architecture must summarise the indexed languages",
    )
    ok(
        "search_code",
        {"project": project, "pattern": "create_order"},
        lambda p: p["matches"] and all(m["line"] > 0 and m["text"] for m in p["matches"]),
        "literal search must return located matches",
    )
    ok(
        "manage_adr",
        {
            "project": project,
            "mode": "upsert",
            "id": "0001-storage",
            "title": "Use redb",
            "body": "Embedded and ACID.",
            "related_qualified_names": [
                "services.orders-py.app.service.OrderService.create_order"
            ],
        },
        lambda p: p["saved"]["id"] == "0001-storage"
        and p["saved"]["related"][0]["start_line"] > 0,
        "upsert must return the stored ADR with its relation resolved to a line",
    )
    ok(
        "manage_adr",
        {"project": project, "mode": "list"},
        lambda p: len(p["adrs"]) == 1,
        "the created ADR must be listed",
    )
    ok(
        "ingest_traces",
        {
            "project": project,
            "traces": [
                {
                    "caller": "services.orders-py.app.service.OrderService.create_order",
                    "callee": "services.orders-py.app.repository.save_order",
                    "count": 3,
                }
            ],
        },
        lambda p: p["linked_count"] == 1 and p["unmatched_count"] == 0,
        "a trace between two indexed symbols must attach an edge",
    )

    # Error paths must be explicit rather than silently empty.
    payload, is_error = server.call("index_status", {"project": "no-such-project"})
    if not is_error or not payload.get("error") or not payload.get("recovery"):
        raise SystemExit(f"unknown project must be an explicit, recoverable error: {payload}")
    print(f"  ok  error path is explicit ({payload['error']})")

    ok(
        "delete_project",
        {"project": project},
        lambda p: p["deleted"],
        "delete must confirm removal",
    )

    missing = set(EXPECTED_TOOLS) - checked
    if missing:
        raise SystemExit(f"tools never exercised: {sorted(missing)}")

    server.close()
    print(f"\nAll {len(EXPECTED_TOOLS)} tools exercised over real stdio JSON-RPC.")


if __name__ == "__main__":
    main()
