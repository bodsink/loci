# loci sample fixture

Seven tiny services that reference each other, used by the integration tests and
the benchmark. Nothing here is meant to run; it exists so the indexer has real
syntax to parse in every bundled language.

| Service | Language | Framework pattern |
| --- | --- | --- |
| `services/orders-py` | Python | FastAPI decorators |
| `services/checkout-ts` | TypeScript | Express `app.get` / `app.post` |
| `services/inventory-go` | Go | `net/http` `HandleFunc` |
| `services/gateway-rs` | Rust | axum `.route` |
| `services/billing-cs` | C# | ASP.NET `[HttpGet]` / `[HttpPost]` attributes |
| `services/notify-kt` | Kotlin | none; interface, override and delegation |
| `services/reports-pl` | Perl | none; subs and `$self->method` calls |

`billing-cs` deliberately calls `_store.Persist(...)` where two types declare
`Persist`. AST alone cannot pick one, so it stays `CALL_UNRESOLVED`; that call is
the fixture's marker for what Hybrid LSP is supposed to fix.

`ignored/` is excluded by `.gitignore` on purpose: the coverage tests assert
that loci reports it as `excluded`, not as missing.

There is no PHP here, and none should be added.
