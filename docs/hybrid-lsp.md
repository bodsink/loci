# Hybrid LSP

## What it is for

The AST resolver answers most calls correctly and takes microseconds. It fails on one specific
shape: a call whose target depends on a *type* rather than a name.

```go
func (svc *Service) Save(value string) {
	svc.store.Persist(value)
}
```

Two types in this repository declare `Persist`. Syntax alone cannot say which one `svc.store` is,
and picking one would be a guess that reads exactly like a fact. So the AST pass records
`CALL_UNRESOLVED` with reason `ambiguous_name_in_project` and moves on.

Hybrid LSP exists to close that gap, and only that gap.

## How it runs

Indexing has three phases:

1. Parse changed files in parallel and write their symbols.
2. Rebuild cross-file edges from the stored facts. Calls that resolve become `CALLS` with
   `source: "ast"`; the rest become `CALL_UNRESOLVED` and are collected.
3. **Optional.** For each language that has an installed server, start it once, and ask
   `textDocument/definition` about *only* the calls from phase 2. Each answer that lands inside an
   indexed symbol becomes a `CALLS` edge with `source: "lsp"`.

Phase 3 never touches a call the AST already resolved. That ordering is what keeps the cost
proportional to the hard cases rather than to the repository.

## Turning it on

```bash
loci index /path/to/repo --lsp
```

or, over MCP:

```json
{ "name": "index_repository", "arguments": { "repo_path": "/path/to/repo", "hybrid_lsp": true } }
```

It is **off by default**. On the Loci repository itself an AST-only index takes about 300 ms;
starting even one language server costs seconds. Paying that on every run, to improve a small
minority of edges, is the wrong default.

## What it reports

The index report contains a per-language breakdown, so the pass can never be credited with work it
did not do:

```
  lsp    : go resolved 1/1 via gopls in 1559 ms (0 unanswered, 0 outside graph)
```

| Field | Meaning |
| --- | --- |
| `attempted` | Unresolved calls the server was asked about. |
| `resolved` | Answers that landed on a symbol in the graph and became `CALLS` edges. |
| `unanswered` | The server ran but had no definition, or timed out on that request. |
| `outside_graph` | A real answer pointing outside the indexed tree, e.g. into a dependency. |
| `skipped_reason` | The language was not attempted at all, and why. |

## Failure is never fatal

A missing server, a server that will not start, a server that hangs: all of these leave the AST
result exactly as it was and add a `skipped_reason`. There is no configuration in which enabling
this flag can make an index fail or make an edge disappear.

Being on `PATH` is not proof a server works. A `rustup` shim for an uninstalled `rust-analyzer`
component is on `PATH` and fails on first use, which is why `get_graph_schema` reports `on_path`
rather than `installed`, and why the truthful record of what happened is the index report and not
the detection list.

## Budgets

| Limit | Default | Why |
| --- | --- | --- |
| `request_timeout` | 20 s | A single definition request that hangs must not hang the index. |
| `total_budget` | 120 s | Bounds the whole pass across every language. |
| `max_calls_per_language` | 2000 | A repository with pathological ambiguity cannot run forever. |

When the total budget runs out the pass stops early and sets `budget_exhausted`, keeping every
edge it had already resolved.

## Servers

| Language | Executable | Verified against a real server |
| --- | --- | --- |
| Go | `gopls` | yes |
| Rust | `rust-analyzer` | yes |
| Python | `pyright-langserver` | not yet |
| TypeScript / JavaScript / TSX / JSX | `typescript-language-server` | not yet |
| C / C++ | `clangd` | not yet |
| Java | `jdtls` | not yet |
| C# | `omnisharp` | not yet |
| Kotlin | `kotlin-language-server` | not yet |
| Perl | `perlnavigator` | not yet |

"Verified" means an integration test in `crates/loci-lsp/tests/real_servers.rs` starts that server,
asks it a question the AST cannot answer, and asserts the exact definition line it returns. The
rest use the same client and the same protocol, but no one has run them here, so they are listed as
unverified rather than assumed to work.

Run the verified ones in strict mode, where a missing server is a failure instead of a skip:

```bash
LOCI_REQUIRE_LSP=1 cargo test -p loci-lsp --test real_servers
```
