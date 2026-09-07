# Loci

A local-first code intelligence engine for AI coding agents. Loci indexes a repository into a
persistent graph on disk and serves that graph to Cursor over the Model Context Protocol, so an
agent can ask *"what calls this?"* instead of grepping and guessing.

Everything runs on your machine. No cloud, no API key, no language runtime. Your source is read,
parsed, and left where it is — only structure (names, paths, line ranges, relationships) is stored.

**Status: milestone 1, plus the follow-up work.** Indexing, the graph, all 15 MCP tools, and the
Cursor integration work today. Hybrid LSP is implemented and off by default; every edge records
whether it came from the AST or from a language server. See [Honest limits](#honest-limits).

## Install

Requires a Linux x86_64 machine and a Rust toolchain to build.

```bash
git clone https://github.com/bodsink/loci loci && cd loci
cargo build --release
./target/release/loci install
```

`install` does two things:

- Copies the binary to `~/.local/bin/loci`, so `loci` works as a command and keeps working after
  the build directory is cleaned. If `~/.local/bin` is not on your `PATH`, it says so and prints the
  line to add.
- Adds a `loci` entry to `~/.cursor/mcp.json`, merging into that file rather than overwriting it:
  your other MCP servers are preserved and the previous config is backed up.

Then index a repository and reload MCP servers in Cursor (Settings → MCP → refresh):

```bash
loci index /path/to/your/repo
```

Ask the agent something structural — *"what calls `create_order`?"* — and it will reach for
`list_projects` and `trace_path` on its own.

## CLI

| Command | What it does |
| --- | --- |
| `loci install` | Copy the binary to `~/.local/bin` and register the server in Cursor's `mcp.json` |
| `loci index <path>` | Index or re-index a repository (incremental by default, `--full` to force) |
| `loci status [project]` | Show what is indexed; with no argument, list every project |
| `loci query --project <id>` | Search the graph for symbols |
| `loci changes --project <id>` | Show which files changed since the last index run |
| `loci delete <project>` | Delete a project's graph; the source repository is untouched |
| `loci mcp` | Serve MCP over stdio (Cursor starts this for you) |

Add `--json` to any command for machine-readable output.

## Where data lives

Under `$XDG_DATA_HOME/loci` (default `~/.local/share/loci`), never inside your repository:

```
~/.local/share/loci/
├── catalog.json                    # which projects are indexed, and where
├── agent_calls.jsonl               # local journal of MCP tool calls
└── projects/<project-id>/graph.redb
```

The graph stores symbol names, file paths, and line ranges — not file contents.
`get_code_snippet` re-reads the file from disk at call time, which is also why a snippet is never
stale relative to the graph. `loci delete` removes a project's store; deleting the whole directory
resets Loci completely.

Reads are confined to the project root you indexed. Symlinks that resolve outside it are rejected,
and `.gitignore` is honoured.

## The 15 MCP tools

**Orientation** — `list_projects`, `index_repository`, `index_status`, `delete_project`,
`get_graph_schema`, `get_architecture`

**Structural discovery** — `search_graph` (find symbols), `query_graph` (multi-hop edge walks),
`trace_path` (callers and callees), `get_code_snippet` (exact source for a symbol)

**Trust and freshness** — `check_index_coverage` (was this path actually indexed?),
`detect_changes` (what moved since the last index?)

**Everything else** — `search_code` (literal and regex text), `manage_adr` (decision records bound
to graph symbols), `ingest_traces` (attach runtime call data)

Every response carries concrete file paths and line ranges, paginates with `has_more`/`cursor`, and
reports empty results as empty.

### Graph schema

16 node labels (`Function`, `Method`, `Class`, `Route`, `Field`, `Adr`, …) and 13 edge types
(`CALLS`, `CALL_UNRESOLVED`, `IMPORTS`, `INHERITS`, `IMPLEMENTS`, `ROUTES_TO`, `IMPACTS`, …). Call
`get_graph_schema` for the authoritative list — it is generated from the same constants the engine
uses, so it cannot drift from the implementation.

Qualified names are dot-separated in every language: the module path from the file, then each
enclosing definition, then the symbol. For example
`services.orders-py.app.service.OrderService.create_order`.

### Import resolution

Source almost never spells an import the way the graph stores a file. TypeScript writes `@/types`,
Dart writes `package:app/models/user.dart`, Go writes `github.com/you/svc/internal/domain`. Matching
that text against file names resolves close to nothing, and the failure is silent: on the project
this was measured against, `frontend/src/types/index.ts` was imported by 941 files and had an
in-degree of **zero**. "Who uses this, what breaks if I change it" answered empty, which looks the
same as "nothing uses this".

So the indexer reads the manifests that define those spellings — `tsconfig.json` (or `jsconfig.json`)
for path aliases, `pubspec.yaml` for the Dart package name, `go.mod` for the Go module path — and
resolves an import to a repository path before looking it up. Relative imports are resolved against
the importing file's own directory, an omitted extension is filled back in, and a directory import
lands on its entry point (`index`, `mod`, `main`, `__init__`). A `tsconfig.json` is read as JSONC,
because real ones carry comments and trailing commas, and its `extends` chain is followed for
inherited mappings.

An alias only applies inside the project whose manifest declares it, and a repository may hold
several: the one measured here has four `go.mod` files, and an import lands in the module that
declares it rather than whichever was read first. Targets outside the repository — `react`,
`dart:async`, `fmt`, `github.com/gin-gonic/gin` — produce no edge at all.

Go is the one language whose import names a package rather than a file, so it points at a `Package`
node named by its import path. Pointing it at every file of the package instead was tried and
measured: one package of 141 files with 456 importers produced 64,296 edges on its own, claiming a
dependency on each file when the importer used a single type. A package node is the only node no
file owns, so it is removed when its directory stops holding Go files.

On that project the change added 16,378 import edges and 93 package nodes. `internal/domain` now has
an in-degree of 456, matching its importer count in source exactly; `frontend/src/types/index.ts`
has 971, being the 941 alias imports plus relative ones. Full-index edge building went from 2,414 ms
to 2,576 ms, and an incremental run from 35 ms to 72 ms.

Languages whose imports genuinely name a module rather than a path — Python, Rust, Java — keep
resolving by dotted module name, which was already correct for them.

## Languages

Twenty-one language IDs have a real tree-sitter grammar linked into the binary.

Code: Python · JavaScript · JSX · TypeScript · TSX · Go · Rust · C · C++ · Java · C# · Kotlin ·
Perl · Shell · Dart

Configuration: TOML · YAML · INI

Build: Make · CMake

Markup: HTML

Every language named for Hybrid LSP is in that list, so no language the engine advertises can turn
out to be unparseable. The reverse no longer holds: shell, configuration, build and markup files are
parsed but have no server in scope, and `hybrid_lsp_eligible` reports false for them.
`get_graph_schema` reports the list with the upstream crate behind each grammar so it can be
audited. PHP is out of scope by design and CI fails if it reappears.

Shell is included because packaging trees are mostly shell: build scripts, and Debian maintainer
scripts like `postinst` that carry no extension at all. Those are found by their `#!` line, which is
read only for extensionless files. `source lib.sh` and `. lib.sh` become import edges rather than
calls to a function named `source`, with the target resolved against the script's own directory.

### Dart

Dart is here because a Flutter application is application code, not
configuration: 244 files in one project, 202 of them under `mobile/lib`, and
none of it visible. It is parsed only — Dart is not in the Hybrid LSP scope, so
`hybrid_lsp_eligible` reports false.

Dart spells a method and a top-level function with the same `function_signature`
node, so the two are told apart by the body they sit in, through the same
`method_parents` mechanism the other languages use. Getters and setters count as
methods, because a Dart model class exposes most of itself through them and
leaving them out would lose the larger half of a type. A `mixin` is a `Trait`,
which is what the label means here; an `extension` is a `Class`, being a named
container of methods with nothing closer available. `extends`, `with` and
`implements` all produce type edges, since all three are ways of taking a type
on and "what implements this" has to walk every one.

On the project this was measured against, 244 files parse with zero errors and
contribute 5,869 nodes: 3,082 fields, 1,490 methods, 700 classes, 555 functions
and 34 enums.

Dart's own package is addressed as `package:goinfracloud/...`, which is resolved
through the name in `pubspec.yaml` — see [Import resolution](#import-resolution).
`dart:` and third-party `package:` targets produce no edge rather than an
invented one.

### Configuration formats

Config says which binary a service runs and which job a pipeline executes, so leaving it out makes
those questions unanswerable. A section becomes a `Module` and a key a `Field`, reusing labels that
already exist rather than inventing a category. A systemd unit is INI, and is recognised by its unit
suffix (`.service`, `.socket`, `.timer`, and the rest) rather than by an `.ini` extension.

YAML has no sections, only nesting, so a key is classified by its value: a block value makes it a
`Module`, a scalar makes it a `Field`. Without that split every `runs-on` in a workflow would share
one qualified name.

Dotted directories are still pruned, with a short allowlist — `.github`, `.gitlab`, `.circleci` —
because a CI workflow is tracked source that says how the project is built. `.git` and `.venv` stay
out.

### Build files

`Makefile` has no extension and `CMakeLists.txt` has a useless one, so both are recognised by whole
file name — `.txt` goes on meaning nothing. `.mk`, `.mak` and `.cmake` are matched by extension as
usual.

A make target is a named unit that other targets invoke by naming it as a prerequisite, so targets
become `Function` nodes and prerequisites become call edges. That makes a Makefile a real dependency
graph rather than a list of strings: `trace_path` answers "what breaks if this target changes".
Variables become `Field` nodes and `include` becomes an import.

CMake contributes `function()` and `macro()` definitions, and every other command as a call, so a
call to a locally defined command resolves to it. `include()` and `add_subdirectory()` are imports
instead — the latter resolved to that directory's `CMakeLists.txt`, since that is the file it
actually pulls in. Both are matched without regard to case, as CMake itself does.

### Markup

HTML was the largest single group one real project reported as unsupported: nineteen files in a
Go and React repository. What it contributes is narrow on purpose. An element carrying `id` becomes
a `Field`, because that is the name the rest of the codebase addresses it by — `<div id="root">` is
what the entry point mounts onto. A `src` or `href` on `script`, `link`, `img`, `iframe`, `source`
or `embed` becomes an import, which is what makes `index.html` reach the module that boots the
application. Everything else on a page is layout, and layout is not a question the graph answers.

`<a href>` is deliberately not an import. A link is navigation, not a dependency, and on the project
this was measured against all 144 of them held either an external URL or a `{{ }}` expression, so
importing them would have added 144 edges to nodes that cannot exist. External URLs, protocol
relative hosts, `mailto:`, `data:`, bare fragments and template expressions are all excluded for the
same reason. A root-absolute path is resolved against the document's own directory, which is where
the web root sits for the entry point that carries these links; that is the one convention assumed
here.

Attributes are read by walking rather than by query, for the reason CMake is: every attribute in
this grammar is an `attribute` node holding an `attribute_name`, with nothing in the node type to
separate `src` from `charset`.

Seventeen of those nineteen files were Go templates rather than documents — `{{ }}` throughout, and
in one case no `<html>` at all. The grammar reads template actions as text, which is the right
answer: all twenty files parse with zero errors. Be clear about the size of the win, though. Indexing
them added one node and two edges to a 35,107-node graph, because email templates expose no ids and
link to nothing local. What it removed was twenty files' worth of `unsupported_language`.

Route extraction currently recognises FastAPI, Express, net/http, Gin and Echo, axum, and ASP.NET
attribute routes. Other frameworks produce no `Route` nodes rather than guessed ones.

### HTTP routes

A service registers most of its routes on nested groups — `v1 := router.Group("/v1")`, then
`auth := v1.Group("/auth")`, then `auth.POST("/login", …)` — and only the last segment is written
next to the route. Storing that segment alone made the route nodes useless: on the project measured
here, none of 760 Go routes carried `/v1`, 360 were bare fragments, and `/:id` appeared 116 times as
the same name. Routes that cannot be told apart cannot answer anything. Group prefixes are now
tracked per file, in source order, so a route carries every prefix above it. A router built any
other way has no prefix and its path is left exactly as written.

The handler was wrong in a quieter way. For `authHandler.Login` the extractor took the first
identifier under the expression, which is the object — `authHandler` — so resolution went looking
for a function by that name and found none. It now reads the field as the name and keeps the object
as a receiver, which is how ordinary method calls are already resolved. Together these took the
project from 1 route with a handler edge to 494, and from 0 routes carrying `/v1` to 829.

Middleware sits between the path and the handler — `POST(path, RequirePermission(…), h.Create)` —
so the handler is the last argument, not the first one after the path.

### Receiver types

Naming the method was not enough. `zoneHandler.List` has to pick between the 72 methods named `List`
in that repository, and 92 are named `Create`; resolution correctly refused to guess, which left 355
routes unlinked. What settles it is the type of `zoneHandler`, and the evidence for that is already
in the source: `zoneHandler := handlers.NewZoneHandler(…)`, and `NewZoneHandler` returns
`*ZoneHandler`.

Three things make that usable. Go states a method's owning type beside the method rather than around
it, so the receiver is captured explicitly and the type now appears in the qualified name —
`…handlers.zone_handler.ZoneHandler.List`. Each local variable is recorded against the function it
takes its value from, and a variable assigned two different constructors in one file is dropped
rather than guessed at, since scope is not tracked. And the return type is read from the tree, not
from `signature`: a stored signature keeps only the first line and caps at 200 characters, so a
constructor whose parameters span several lines — which is most of them — would lose its type
entirely.

Resolution by receiver type is tried before any name-based path and is the only one that can
separate one `List` from 71 others. When the type is unknown, behaviour is unchanged and no edge is
written. In the project measured here this took Go routes with a handler from 494 to 847 of 849, and
resolved 359 ordinary method calls that name matching could not settle. The two routes still
unlinked are inline closures, which have no name to point at.

A further 215 unlinked "routes" are the frontend's own `api.get('/customers')` calls: outbound
requests with no local handler. Linking those to the backend routes they reach is not done yet.

### C, C++ and Qt

A `.h` file gives no clue whether it is C or C++, so the extension is not trusted. Both grammars are
tried and the one with fewer parse errors wins, which keeps C++ headers off the C grammar without
pushing C headers onto a grammar that reserves `class` and `new`.

The bundled grammar is standards C++, so several ordinary constructs would otherwise shred a file.
Qt's moc keywords (`Q_OBJECT`, `signals:`, `emit`) are macros a compliant parser never sees.
`QTEST_MAIN(T)` and `Q_ARG(int, x)` are not parseable calls, the latter because its first argument
is a type. And `= {}` as a default argument is rejected outright. When a direct parse fails, the
source is rewritten in memory with those neutralised — byte lengths preserved, so reported lines and
offsets still point at the real file — and the result is kept only if it parses better. Files the
grammar already handles are never rewritten, and nothing on disk is touched.

A side effect is fewer false edges: `Q_ARG` and friends were previously read as function calls, and
they are macros, not functions.

A preprocessor conditional is the last case, and the hardest, because it is chosen *inside* a
declaration:

```c
const QStringList names =
#ifdef Q_OS_WIN
    {QStringLiteral("neighbor.exe")};
#else
    {QStringLiteral("neighbor")};
#endif
```

Nothing there is a construct on its own, and the grammar reports the lost brace balance far away —
in the file this came from, at a closing brace 117 lines below. So the conditional is resolved the
way one compiler pass would: the first branch is kept, the directives and the branches not taken are
erased. Keeping both branches instead was measured and is worse; it leaves a stray `{...};` behind
and does not survive a conditional that splits a signature. `#if 0` is the one condition actually
read, since it is the idiom for commenting out a block.

The cost is that symbols reachable only through `#else` go unindexed in that file. It is bounded the
same way as the rest: only files that already failed to parse are touched, and only when the rewrite
lowers the error count.

On a 94-file Qt codebase these passes took the files reported as `parse_partial` from 59 to 0, with
no change in node count.

### TypeScript and JSX

Two places in the bundled grammar let a keyword win over an identifier, and each one truncates the
file from that point on.

The first is `&` in JSX. The lexer reads it as the start of a character reference and fails when no
`;` closes it, so `accounting & session control` breaks an element while `&amp;` is fine. The second
is an interface member whose name begins with `in` or `instanceof`, when members are separated by
newlines rather than semicolons: `in` is taken as the operator continuing the type on the line
above, the interface closes early, and its remaining members become top-level labelled statements.
Only those two keywords do this, out of twenty-seven tried.

Both are repaired the same way as the C++ passes, and only after a direct parse has already failed:
a `&` in markup becomes a space, and two bytes of a member's indentation become `; `. Line counts,
columns and byte lengths are unchanged, and the second pass writes over whitespace only, so no name
the graph records can be altered.

Which bytes to touch is read from the parse tree rather than matched in the text, because the same
characters are ordinary code elsewhere. A text-level pass was measured first and it corrupted type
intersections (`A & B`) and put semicolons into object literals that were already correct — and
because the total error count still fell, the guard above would have accepted the damage. What
matters is the *nearest* enclosing node, not overlapping spans: an element written inside
`{cond ? (...) : null}` sits within an expression while still being markup itself.

On a 4269-file Go and React project these passes took `parse_partial` from 33 files to 4, again with
no change in node count. What they buy is not more symbols but honest ones: a truncated interface
was still recorded, ending at the member the parse died on, so `get_code_snippet` returned half a
type. The four that remain are a `Makefile` with a target named `export`, and three files using
`import('...').T[]` inside a type argument — the same class of grammar fault, left alone rather than
guessed at.

## Honest limits

These are stated because an agent that trusts a wrong answer is worse than one that knows it needs
to check.

- **Hybrid LSP is off by default.** Indexing is AST-first, and with the default settings every edge
  carries `source: "ast"`. A call that needs type information stays `CALL_UNRESOLVED` rather than
  being guessed: in the fixture, C#'s `_store.Persist(invoice)` resolves to nothing because two
  types declare `Persist`, and only a type checker can say which one `_store` is. Pass `--lsp`
  (CLI) or `hybrid_lsp: true` (MCP) to ask an installed language server about exactly those calls;
  the resulting edges carry `source: "lsp"`. It stays off by default because starting a server
  turns a 300 ms run into a multi-second one. Only servers you already have on PATH are used, and a
  missing one is reported, never fatal.
- **Unresolved calls are labelled, not hidden.** When a call site cannot be pinned to a definition,
  it becomes a `CALL_UNRESOLVED` edge with a reason. Absence of a `CALLS` edge is not evidence that
  no call exists.
- **An unresolved import is silent, unlike an unresolved call.** An import that points outside the
  repository, or that a manifest does not explain, produces no edge and no marker. That is right for
  a third-party package, but it means a missing `IMPORTS` edge does not prove the dependency is
  absent — only that nothing indexed matched it. Aliases declared somewhere other than
  `tsconfig.json`, `jsconfig.json`, `pubspec.yaml` or `go.mod` (a bundler config, for instance) are
  not read.
- **Coverage is best-effort.** `check_index_coverage` tells you what was indexed and what was
  skipped and why. It does not prove a file was fully understood. The skipped sample shows one
  representative per directory and reason, largest group first, with a count of what it stands for,
  so a folder of twenty icons cannot crowd out every other reason a file was left out and the
  biggest gap cannot fall off the end of the list.
- **Linux x86_64 only.** macOS and Windows are not built or tested; CI covers Linux alone rather
  than listing platforms it does not verify.

## Performance

Measured on the maintainer's machine, release build. These are observations, not guarantees.

Indexing this repository itself (40 source files, 573 nodes, 3897 edges):

| | Time |
| --- | --- |
| Cold full index | 291 ms |
| Re-index, nothing changed | 56 ms |
| Re-index after editing one file | one file re-parsed, rest skipped by content hash |

Warm query latency on the sample fixture, 2000 iterations each:

| Query | p50 | p95 |
| --- | --- | --- |
| `search_graph` by exact name | 0.005 ms | 0.009 ms |
| `search_graph` by qualified name | 0.005 ms | 0.005 ms |
| `trace_path`, depth 3 | 0.013 ms | 0.018 ms |
| `search_graph` by prefix regex | 0.031 ms | 0.047 ms |

Structural queries are comfortably under the 1 ms target, but latency scales with the number of
matches returned — a broad pattern is slower than an exact name. Run `cargo run --release -p
loci-bench` to reproduce on your own hardware.

### At scale

Measured on a real 35k-file tree (a Go module cache: 34,631 Go files plus C, C++, Java, JavaScript,
Perl and Python), producing 608,713 nodes and 3,759,839 edges:

| | Before | Now |
| --- | --- | --- |
| Cold full index, 52,299 files parsed | 200 s | 189 s |
| Re-index with 121 of 52,299 files changed | 80 s | 10 s |
| — of which cross-file edge rebuild | 58 s | 0.33 s |

`loci index` prints a per-phase breakdown, so a slow run can be attributed rather than guessed at:

```
phases : walk 594 ms, hash 1123 ms, parse 36 ms, load 5789 ms, edges 327 ms, lsp 0 ms, report 407 ms
```

The re-index used to rebuild all 3.7M edges on every run. It now rebuilds only the files whose own
facts changed plus the files that reference a symbol those files defined, which is what correctness
actually requires. `crates/loci-index/tests/incremental_equivalence.rs` walks a fixture through a
body edit, a new definition, a deleted definition, a rename and a file removal, asserting after each
step that the incrementally maintained graph is identical to one rebuilt from scratch. On the 35k
tree, an incremental run and two full rebuilds all produce the same 3,759,839 edges.

Two limits remain, both measured rather than assumed:

- **Full-index throughput is about 277 files/s**, dominated by parsing (155 s of the 189 s).
  Extrapolated to the 75k-file
  Linux kernel that is roughly 4.5 minutes, not the 3 minutes the product target asks for. The kernel
  itself has still not been indexed here, so that remains an estimate, not a result.
- **Incremental runs have a ~6 s floor on this tree**, almost all of it loading 608,713 nodes and
  every file's facts to build the symbol table. That is independent of how little changed.

#### A bug this uncovered

Comparing runs exposed something worse than slowness: two full indexes of the *same* unchanged tree
disagreed on the edge count (3,759,761 then 3,759,780). Structural edge ids were a 31-bit hash of
`(src, dst, type)`. At 608k structural edges the birthday bound makes collisions certain, and each
collision silently overwrote a `DEFINES` or `CONTAINS` edge, so a symbol lost its parent. Node ids
shift between runs, so a different set of edges was lost each time. Structural ids now come from a
counter, and two full rebuilds of that tree produce byte-identical edge counts.

Because ids are now allocated rather than derived, the schema version moved to 2; graphs written by
an older build are rebuilt automatically on the next index.

## Development

```bash
cargo test --workspace                     # 139 tests
cargo clippy --workspace --all-targets -- -D warnings
cargo run --release -p loci-bench          # indexing and query benchmarks

# Exercise all 15 tools against the real binary over stdio, as Cursor does
LOCI_DATA_DIR=$(mktemp -d) python3 scripts/mcp_smoke.py \
  ./target/release/loci "$PWD/fixtures/sample" smoke
```

`fixtures/sample` is a small multi-language repository (Python, TypeScript, Go, Rust) with HTTP
routes and cross-file calls, used by both the tests and the benchmark.

### Layout

| Crate | Responsibility |
| --- | --- |
| `loci-core` | Errors, language IDs, sandboxed paths, data directories |
| `loci-graph` | redb-backed store, schema, queries, coverage, catalog |
| `loci-parse` | tree-sitter grammars and per-language extraction queries |
| `loci-index` | Walking, hashing, symbol and import resolution, incremental indexing |
| `loci-lsp` | Language server detection (resolution not yet implemented) |
| `loci-mcp` | JSON-RPC over stdio, the 15 tools, usage journal |
| `loci-cli` | The `loci` binary |

See [`docs/cursor-agent.md`](docs/cursor-agent.md) for how the tools are shaped around the way
Cursor's agent actually behaves.

## License

Apache-2.0. See [LICENSE](LICENSE).
