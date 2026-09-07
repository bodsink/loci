# Designing for Cursor's agent

Loci exists to be used by a model, not a person. That changes what a good API looks like. This
document records the agent behaviours the tool surface is built around, and the specific design
decision each one produced.

Everything here is a design rationale, not a claim about Cursor's internals. Where a behaviour was
observed rather than documented, it says so.

## How Cursor reaches the server

Cursor starts `loci mcp` as a subprocess and speaks newline-delimited JSON-RPC 2.0 over stdin and
stdout. There are no HTTP headers and no `Content-Length` framing: one JSON object per line.

Two consequences shape the implementation:

- **stdout is the protocol.** Anything else written there corrupts the stream. All diagnostics go
  to stderr, and the server never prints to stdout except as a response.
- **Notifications must not be answered.** A message without an `id` (such as
  `notifications/initialized`) gets no reply. Answering it desynchronises the client.

Both are covered by tests in `crates/loci-mcp/src/protocol.rs`, and `scripts/mcp_smoke.py` drives
the real binary the same way Cursor does.

## Six behaviours, and what each one changed

### 1. The agent starts with no idea what is indexed

It has your open files and its own search tools. It does not know Loci exists until it reads the
tool list, and it does not know which repositories have graphs.

**Design response.** The `initialize` response ships `instructions` that state the order of
operations explicitly: `list_projects` first, then index if needed, then structural discovery. Every
tool that needs a project says in its schema that the id comes from `list_projects` and is *not* a
directory name. When `list_projects` returns nothing, the response includes a `next_step` string
telling the agent to call `index_repository` with an absolute path.

### 2. The agent defaults to grep

Text search is the tool it has always had, so it reaches for it first even when the question is
structural.

**Design response.** Tool descriptions position `search_graph`, `query_graph`, and `trace_path` as
the default and scope `search_code` narrowly to literals, regex, and cases where graph coverage is
insufficient. The server instructions say "Prefer these over grep" in as many words. This is a
nudge, not a guarantee — the model still chooses.

### 3. The agent will state confident negatives

"Nothing calls this function" is a very expensive thing to be wrong about, and an empty result set
looks identical to a genuine absence.

**Design response.** Three reinforcing measures:

- Unresolved calls become `CALL_UNRESOLVED` edges carrying a reason, so a call that was *seen* but
  not *pinned down* is visible instead of silently missing.
- `check_index_coverage` answers "was this path actually indexed?" per path or scope.
- Any structural result that could support a negative claim carries a `note` saying what it does
  not prove, and the server instructions require `check_index_coverage` before any exhaustive
  claim.

That last one applies to the empty cases specifically: a `search_graph` that matched nothing, a
`query_graph` walk that found no path, and a `trace_path` that found no callers. The zero-caller
case is caveated even when there is nothing unresolved to point at, because "nothing calls this"
is the most expensive wrong answer the tool can give. The caveat is suppressed for an
outbound-only trace, which never looked for callers — a warning that fires when it doesn't apply
is one the model learns to skip.

`every_empty_structural_result_carries_a_caveat` in `crates/loci-mcp/tests/agent_flow.rs` locks
this in. The notes say coverage is best-effort and never proof of completeness, because it isn't.

### 4. The agent treats the first page as the whole answer

If a response looks complete, it will be used as though it is.

**Design response.** Every list response carries `has_more`, `total`, and a `cursor`. The server
instructions require paging until `has_more` is false before concluding anything. Totals are
included so the agent can see the gap between what it received and what exists.

### 5. The agent guesses when a name is ambiguous

Ask for `create_order` in a repository with a module-level function *and* a method of that name,
and a tool that silently returns one of them teaches the agent something false.

**Design response.** `trace_path` refuses. It returns an `error`, a message, and a `candidates`
array with the qualified name, file, and line of each match, so the agent can pick and retry.

This is verified end to end: `scripts/mcp_smoke.py` asserts that the ambiguous call produces
candidates, then repeats the call with a qualified name and checks the trace crosses files and
reaches the second hop.

### 6. The agent recovers better from a specific error than a generic one

"Something went wrong" produces a retry of the same mistake.

**Design response.** Every failure carries a stable machine-readable `error` code, a human-readable
`message`, and a `recovery` string saying what to do instead. An unknown project returns
`project_not_found` with "Call list_projects for the exact ids, or index_repository if the
repository is not indexed yet." Bad arguments return `invalid_argument` naming the accepted values.

Failures come back as tool results with `isError: true` rather than JSON-RPC transport errors, so
the model reads them and can act, instead of the client swallowing them.

## Knowing whether any of this works

Design intent is not evidence. The `agent_calls.jsonl` journal in the data directory records one
line per tool call so the guesses above can be checked against what Cursor's agent actually does.

Each entry holds the tool name, **argument keys only** (never values), the project id, duration,
whether it succeeded, any error code, and whether the result was truncated. No source code, no
argument values, no query strings. It is local, it is never transmitted, and deleting the file is
harmless.

What it is for:

- **Does the agent call `list_projects` first?** If it guesses project ids instead, the schema
  descriptions are not landing.
- **Does it reach for `search_code` when it should use `search_graph`?** A high ratio means the
  tool descriptions need work.
- **Does it page?** Repeated first-page calls with `has_more: true` and no cursor follow-up means
  the pagination instruction is being ignored.
- **Which errors recur?** A recurring `invalid_argument` on one tool is a schema problem, not a
  model problem.

Reading it is a plain `jq` over the file. It exists to make the next milestone's tool descriptions
empirical rather than speculative.

## What is not yet verified

- Whether the `instructions` field measurably changes Cursor's tool selection. It is part of the
  MCP spec and Cursor accepts it, but its influence on the model has not been measured here.
- Long-session behaviour: whether the agent re-checks coverage after an edit, or trusts a graph
  that has gone stale. `detect_changes` exists for this, but no data yet shows the agent using it
  unprompted.
- Behaviour in MCP clients other than Cursor. The protocol implementation is spec-conformant and
  client-agnostic, but only Cursor has been exercised.
