# How each language is extracted

These notes sit behind the language list in the [README](../README.md). They record why a
construct is labelled the way it is, and the measurements that decided it. They are not a
guarantee that every file in that language will parse.

An extracted import still has to land on a path; that half is in
[Import resolution](../README.md#import-resolution).

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
through the name in `pubspec.yaml` — see [Import resolution](../README.md#import-resolution).
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

On a 4269-file Go and React project the first two TypeScript passes took `parse_partial` from 33
files to 4. The four that remained were a `Makefile` with a target named `export`, and three files
using `import('...').T[]` inside a type argument.

`array_type` in the bundled grammar only wraps a `primary_type`, and `import('mod').T` is not one,
so the `[]` is read as a tuple or a subscript and a generic `<{ data: import('m').T[] }>` becomes a
comparison. The import call is overwritten with an identifier of the same length; `.T` and every
definition around it keep their letters. A runtime `import('./mod')` is not followed by `.Ident[]`
and is left alone.

`export:` is a legal Make target. The grammar only has `export` as a directive, so the recipe
becomes ERROR nodes. The keyword is replaced with underscores of the same length for the parse, then
put back from the original bytes so the graph still records a target named `export`. A real
`export FOO = bar` is not followed by `:` and is not touched.
