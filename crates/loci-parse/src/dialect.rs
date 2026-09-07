//! Making sources parseable by grammars that do not quite cover them.
//!
//! Six gaps cost real symbols on real repositories:
//!
//! 1. tree-sitter-cpp implements C++, not Qt. Qt's moc keywords — `Q_OBJECT`,
//!    `signals:`, `emit` — are macros that never reach a standards compliant
//!    parser, so a Qt codebase parses as a field of syntax errors.
//! 2. The grammar rejects `= {}` as a default argument, which is ordinary
//!    C++11. One project here used it 42 times across 10 files.
//! 3. A preprocessor conditional can choose part of one declaration, which no
//!    grammar can represent because neither branch is a construct on its own.
//! 4. tree-sitter-typescript lets a keyword win over an identifier in two
//!    places, and both truncate the file: a `&` in JSX that is not a character
//!    reference, and an interface member whose name begins with `in` or
//!    `instanceof` when the members are separated by newlines alone.
//! 5. The same TypeScript grammar accepts `import('mod').T` as a type in some
//!    positions, but `import('mod').T[]` is not a `primary_type`, so the `[]`
//!    becomes a tuple or a subscript and a generic argument is read as a
//!    comparison. Three files in one project died on that shape alone.
//! 6. tree-sitter-make treats `export`, `unexport`, `override` and `include` as
//!    directives, so a target of that name (`export:`) is not a rule. The
//!    recipe becomes ERROR nodes and the target never reaches the graph.
//!
//! All are handled by rewriting the source in place, preserving byte length
//! exactly, so every offset, line and column the parser reports still points at
//! the original file. Only structure is touched, never a name or a literal, and
//! callers keep reading real source from disk. The indexer applies these only
//! after a direct parse has already failed, and keeps the result only if it
//! parses better, so a file the grammar already handles is never rewritten.
//!
//! The Make pass is the one exception that must restore a name: the keyword
//! *is* the target, so it is overwritten with underscores of the same length
//! for the parse and put back from the original bytes afterwards.
//!
//! Measured on a Qt project of 94 tracked C/C++ files: 52 files and 187 error
//! nodes before, 21 files and 66 error nodes after the Qt pass alone. On a
//! 4269-file Go/React project, 32 TypeScript files parsed partially before the
//! two TypeScript passes.

use loci_core::LanguageId;

/// Qt keywords that are safe to erase outright, and their replacement.
///
/// `signals` and `Q_SIGNALS` become `public` because they introduce an access
/// section: erasing them would leave a bare `:` that is itself a syntax error.
/// `slots` can be erased because it only ever follows a real access specifier
/// (`public slots:`), which already carries the colon.
const REWRITES: &[(&str, &str)] = &[
    ("signals", "public"),
    ("Q_SIGNALS", "public"),
    ("slots", ""),
    ("Q_SLOTS", ""),
    ("emit", ""),
    ("Q_OBJECT", ""),
    ("Q_GADGET", ""),
    ("Q_INVOKABLE", ""),
];

/// Qt macros that take arguments, and what to leave in their place. The whole
/// invocation goes, parentheses included, since a dangling argument list parses
/// no better than the macro.
///
/// Most sit where a declaration is expected and can vanish. `Q_ARG` and
/// `Q_RETURN_ARG` cannot: they sit inside an argument list, so erasing one
/// would leave an empty slot between commas. They become `0` instead, a valid
/// expression of no interest to the graph. They need handling at all because
/// their first argument is a type — `Q_ARG(int, 0)` is not a parseable call,
/// while `Q_ARG(QJsonObject, e)` happens to be.
const CALL_MACROS: &[(&str, &str)] = &[
    ("Q_PROPERTY", ""),
    ("Q_ENUM", ""),
    ("Q_ENUMS", ""),
    ("Q_FLAG", ""),
    ("Q_FLAGS", ""),
    ("Q_DECLARE_METATYPE", ""),
    ("Q_DECLARE_FLAGS", ""),
    ("Q_CLASSINFO", ""),
    ("Q_INTERFACES", ""),
    ("QTEST_MAIN", ""),
    ("QTEST_APPLESS_MAIN", ""),
    ("QTEST_GUILESS_MAIN", ""),
    ("Q_ARG", "0"),
    ("Q_RETURN_ARG", "0"),
];

/// Rewrite C++ the bundled grammar cannot parse, or `None` if nothing applies.
pub fn prepare_cpp(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut out = source.to_string();
    // Safe because every replacement is ASCII of identical byte length.
    let buffer = unsafe { out.as_bytes_mut() };
    let mut changed = false;

    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(next) = skip_uninteresting(bytes, i) {
            i = next;
            continue;
        }

        if bytes[i] == b'=' {
            if let Some(brace) = empty_brace_initialiser(bytes, i) {
                // `= {}` becomes `= 0 `: two bytes swapped, nothing shifts.
                buffer[brace] = b'0';
                buffer[brace + 1] = b' ';
                changed = true;
                i = brace + 2;
                continue;
            }
        }

        if !is_word_start(bytes, i) {
            i += 1;
            continue;
        }
        let end = word_end(bytes, i);
        let word = &source[i..end];

        if let Some((_, replacement)) = REWRITES.iter().find(|(name, _)| *name == word) {
            blank_with(buffer, i, end, replacement);
            changed = true;
        } else if let Some((_, replacement)) = CALL_MACROS.iter().find(|(name, _)| *name == word) {
            let stop = macro_invocation_end(bytes, end).unwrap_or(end);
            blank_with(buffer, i, stop, replacement);
            changed = true;
            i = stop;
            continue;
        }
        i = end;
    }

    changed.then_some(out)
}

/// Resolve `#if` / `#else` / `#endif` the way one compiler pass would: keep
/// the first branch, erase the directives and the branches not taken.
///
/// A conditional usually parses fine, because each branch is a complete
/// construct. The case that does not is a conditional chosen *inside* one
/// declaration:
///
/// ```text
/// const QStringList names =
/// #ifdef Q_OS_WIN
///     {QStringLiteral("sagara-neighbor.exe")};
/// #else
///     {QStringLiteral("sagara-neighbor")};
/// #endif
/// ```
///
/// Nothing here is a construct on its own: not the text before `#ifdef`, not
/// either branch. The grammar loses brace balance and reports the damage far
/// away — in the file this came from, at the closing brace of a function 117
/// lines below.
///
/// Erasing only the directives and keeping both branches leaves a stray
/// `{...};` behind, which on that file removed one parse error of three and
/// left two. Keeping one branch is what a build actually compiles, and it
/// survives the harder shape where the branches split a signature:
/// `#ifdef WIN` / `void f() {` / `#else` / `void g() {` / `#endif`.
///
/// The cost is that symbols reachable only through `#else` go unindexed on
/// this file. That is bounded: the caller reaches for this only after a plain
/// parse has already failed, and keeps the result only if it lowers the error
/// count, so a file the grammar already reads is never touched.
///
/// `#if 0` is the one condition read rather than assumed, because it is the
/// idiom for commenting out a block; there the `#else` branch is the one a
/// build sees.
pub fn flatten_conditionals(source: &str) -> Option<String> {
    let mut out = source.to_string();
    // Safe because every byte written is a space, and only over ASCII.
    let buffer = unsafe { out.as_bytes_mut() };
    let mut changed = false;
    let mut offset = 0usize;
    let mut continuing = false;
    let mut groups: Vec<Group> = Vec::new();

    for line in source.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        let (start, end) = (offset, offset + text.len());
        offset += line.len();

        if continuing {
            blank(buffer, start, end);
            changed = true;
            continuing = text.ends_with('\\');
            continue;
        }

        match directive(text) {
            Some(Directive::Open { plausible }) => {
                let outer = groups.last().is_none_or(|g| g.live);
                groups.push(Group {
                    outer,
                    live: outer && plausible,
                    taken: plausible,
                });
            }
            Some(Directive::Switch { plausible }) => {
                if let Some(group) = groups.last_mut() {
                    let take = plausible && !group.taken;
                    group.taken |= take;
                    group.live = group.outer && take;
                }
            }
            Some(Directive::Close) => {
                groups.pop();
            }
            None => {
                if groups.last().is_none_or(|g| g.live) {
                    continue;
                }
            }
        }

        blank(buffer, start, end);
        changed = true;
        continuing = text.ends_with('\\');
    }

    changed.then_some(out)
}

/// One `#if` … `#endif` group while the scan is inside it.
struct Group {
    /// Whether the enclosing groups kept this one's territory at all.
    outer: bool,
    /// Whether the branch currently open is the one being kept.
    live: bool,
    /// Whether some branch of this group has already been kept, which is what
    /// makes `#else` the alternative rather than a second helping.
    taken: bool,
}

enum Directive {
    /// `plausible` is false only for `#if 0`, the idiom for commenting out a
    /// block. Keeping that branch would feed the graph code that never builds.
    Open {
        plausible: bool,
    },
    Switch {
        plausible: bool,
    },
    Close,
}

/// Classify a line as a conditional directive, or `None` if it is not one.
///
/// `#define`, `#include` and `#pragma` are deliberately absent: they carry
/// meaning the parser can already use, and erasing them would lose it.
fn directive(line: &str) -> Option<Directive> {
    // `#  ifdef` with space after the hash is legal and does occur.
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let (keyword, condition) = rest.split_at(end);
    let plausible = condition.trim().trim_end_matches('\\').trim() != "0";
    match keyword {
        "if" | "ifdef" | "ifndef" => Some(Directive::Open { plausible }),
        "elif" | "elifdef" | "elifndef" => Some(Directive::Switch { plausible }),
        "else" => Some(Directive::Switch { plausible: true }),
        "endif" => Some(Directive::Close),
        _ => None,
    }
}

fn blank(buffer: &mut [u8], start: usize, end: usize) {
    for slot in buffer[start..end].iter_mut() {
        *slot = b' ';
    }
}

/// Nodes whose span is markup rather than code.
///
/// `jsx_expression` is deliberately absent and handled as its opposite: the
/// `{...}` inside an element is ordinary TypeScript, where `&` really is the
/// bitwise operator or a type intersection.
const JSX_MARKUP: &[&str] = &[
    "jsx_element",
    "jsx_self_closing_element",
    "jsx_opening_element",
    "jsx_closing_element",
    "jsx_fragment",
    "jsx_attribute",
];

/// Blank each `&` in JSX markup that does not begin a character reference.
///
/// The JSX lexer reads `&` as the start of an entity and fails when no `;`
/// closes it, so `Registered & Managed` and `url="a&b"` both truncate the file
/// while `&amp;` is fine. The damage is not cosmetic: everything after the
/// `&` leaves the element, so the component's own symbols can be lost.
///
/// Which `&` to touch is decided from the parse tree, not from the text. A
/// regex would also hit `A & B` in a type intersection and `x & y` in an
/// expression, corrupting real code; measured on 32 failing files, that
/// approach made four of them worse. Reading the tree keeps every `&` outside
/// markup untouched — including those inside `{...}`, which is why
/// `jsx_expression` spans are subtracted rather than merely not added.
///
/// The replacement is a space because JSX text is content, never a symbol, so
/// nothing the graph records can change.
pub fn neutralise_jsx_ampersands(language: LanguageId, source: &str) -> Option<String> {
    if !source.contains('&') {
        return None;
    }
    let grammar = crate::registry::grammar(language)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar).ok()?;
    let tree = parser.parse(source, None)?;

    let root = tree.root_node();
    let bytes = source.as_bytes();
    let mut out = source.to_string();
    // Safe because a space is one ASCII byte replacing one ASCII byte.
    let buffer = unsafe { out.as_bytes_mut() };
    let mut changed = false;

    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'&' || begins_character_reference(bytes, at) {
            continue;
        }
        if in_markup(root, at) {
            buffer[at] = b' ';
            changed = true;
        }
    }

    changed.then_some(out)
}

/// Whether the byte at `at` is markup rather than code.
///
/// Decided by the nearest enclosing JSX node, not by comparing spans: markup
/// and expressions nest in both directions, and an element written inside
/// `{cond ? (...) : null}` sits within a `jsx_expression` while still being
/// markup itself. Subtracting whole expression spans would discard it, which
/// on one project left fourteen files unrepaired.
fn in_markup(root: tree_sitter::Node, at: usize) -> bool {
    let mut node = root.descendant_for_byte_range(at, at + 1);
    while let Some(current) = node {
        if current.kind() == "jsx_expression" {
            return false;
        }
        if JSX_MARKUP.contains(&current.kind()) {
            return true;
        }
        node = current.parent();
    }
    false
}

/// Whether the `&` at `at` opens `&name;`, `&#48;` or `&#x30;`.
fn begins_character_reference(bytes: &[u8], at: usize) -> bool {
    let mut i = at + 1;
    if bytes.get(i) == Some(&b'#') {
        i += 1;
        if matches!(bytes.get(i), Some(b'x' | b'X')) {
            i += 1;
        }
    }
    let start = i;
    while bytes.get(i).is_some_and(u8::is_ascii_alphanumeric) {
        i += 1;
    }
    i > start && bytes.get(i) == Some(&b';')
}

/// Keywords the lexer will claim from the start of an interface member name.
///
/// Found by trying twenty-seven keywords as a name prefix: only these two
/// break, because only these two are binary operators that could continue the
/// type on the line above.
const SHADOWING_KEYWORDS: &[&str] = &["in", "instanceof"];

/// Terminate the member above one whose name starts with a shadowing keyword.
///
/// TypeScript lets interface members be separated by newlines alone, and then
/// `oper_status: number` followed by `in_octets: number` closes the interface
/// early: the lexer takes `in` as the operator continuing `number`. Every
/// member after that point leaves the interface and becomes a top-level
/// labelled statement, so those fields never reach the graph at all.
///
/// An explicit `;` removes the ambiguity. It is written over the last two
/// bytes of the line's indentation, so the file keeps its length, its line
/// count, and the column of every name — and because only whitespace is
/// overwritten, no symbol the graph records can be altered by this pass.
/// Indentation of tabs, or of fewer than two spaces, is left alone rather than
/// shifting anything.
///
/// An object literal is spelled the same way but forbids the semicolon, so the
/// line's enclosing container is read from the tree first. Skipping that check
/// turned three healthy literals into errors on one project, and the net count
/// still fell, so the caller's guard would have accepted the damage.
pub fn separate_keyword_members(language: LanguageId, source: &str) -> Option<String> {
    let candidates: Vec<(usize, usize)> = {
        let mut found = Vec::new();
        let mut offset = 0usize;
        for line in source.split_inclusive('\n') {
            if let Some(indent) = shadowed_member_indent(line) {
                found.push((offset, indent));
            }
            offset += line.len();
        }
        found
    };
    if candidates.is_empty() {
        return None;
    }

    let grammar = crate::registry::grammar(language)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();

    let mut out = source.to_string();
    // Safe because a semicolon is one ASCII byte replacing one space.
    let buffer = unsafe { out.as_bytes_mut() };
    let mut changed = false;

    for (offset, indent) in candidates {
        if takes_commas(root, offset + indent) {
            continue;
        }
        buffer[offset + indent - 2] = b';';
        changed = true;
    }

    changed.then_some(out)
}

/// Whether the member at `at` sits in a container whose entries are separated
/// by commas, where a semicolon would be a syntax error.
///
/// Only the nearest enclosing container counts. An interface declared inside a
/// method of an object literal is still an interface.
fn takes_commas(root: tree_sitter::Node, at: usize) -> bool {
    let mut node = root.descendant_for_byte_range(at, at + 1);
    while let Some(current) = node {
        match current.kind() {
            "object" | "arguments" | "array" | "formal_parameters" => return true,
            "interface_body" | "object_type" | "statement_block" | "program" => return false,
            _ => node = current.parent(),
        }
    }
    false
}

/// Width of the indentation when a line declares a member whose name a keyword
/// would claim, or `None` when the line is anything else.
fn shadowed_member_indent(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent < 2 {
        return None;
    }
    let rest = &line[indent..];
    let name_end = rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')?;
    let name = &rest[..name_end];
    if !SHADOWING_KEYWORDS.iter().any(|k| name.starts_with(k)) {
        return None;
    }
    // A member is a name, an optional `?`, then `:`. Anything else on the line
    // is some other construct that must not collect a semicolon.
    let after = rest[name_end..].trim_start();
    let after = after.strip_prefix('?').unwrap_or(after).trim_start();
    after.starts_with(':').then_some(indent)
}

/// Offset of the `{` in an `= {}` starting at `equals`, if that is what follows.
///
/// Deliberately narrow: only an empty brace pair, and not `==`, `>=` or any
/// other operator ending in `=`.
fn empty_brace_initialiser(bytes: &[u8], equals: usize) -> Option<usize> {
    if bytes.get(equals + 1) == Some(&b'=') {
        return None;
    }
    if matches!(
        equals.checked_sub(1).and_then(|p| bytes.get(p)),
        Some(b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^')
    ) {
        return None;
    }
    let mut j = equals + 1;
    while matches!(bytes.get(j), Some(b' ' | b'\t')) {
        j += 1;
    }
    (bytes.get(j) == Some(&b'{') && bytes.get(j + 1) == Some(&b'}')).then_some(j)
}

/// Advance past a comment or literal, returning the index after it.
fn skip_uninteresting(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes[i] {
        b'/' if bytes.get(i + 1) == Some(&b'/') => {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            Some(j)
        }
        b'/' if bytes.get(i + 1) == Some(&b'*') => {
            let mut j = i + 2;
            while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            Some((j + 2).min(bytes.len()))
        }
        quote @ (b'"' | b'\'') => {
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => j += 2,
                    b if b == quote => return Some(j + 1),
                    b'\n' if quote == b'\'' => return Some(j),
                    _ => j += 1,
                }
            }
            Some(bytes.len())
        }
        _ => None,
    }
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_word_start(bytes: &[u8], i: usize) -> bool {
    let starts = bytes[i].is_ascii_alphabetic() || bytes[i] == b'_';
    starts && (i == 0 || !is_word_char(bytes[i - 1]))
}

fn word_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && is_word_char(bytes[end]) {
        end += 1;
    }
    end
}

/// End of a `MACRO(...)` invocation, honouring nesting. `None` when the
/// parentheses never balance, in which case the macro name is left alone.
fn macro_invocation_end(bytes: &[u8], after_name: usize) -> Option<usize> {
    let mut i = after_name;
    while matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Directives the Make grammar will not also accept as a rule target.
///
/// Measured by dumping `export:`, `unexport:`, `override:` and `include:` —
/// each one is parsed as the directive, and the recipe becomes ERROR. A target
/// that merely *contains* the word (`exports:`) is already a rule.
const MAKE_TARGET_KEYWORDS: &[&str] = &["export", "unexport", "override", "include"];

/// Turn a directive-keyword used as a target into a word the grammar accepts.
///
/// `export:` is a legal Make target. The grammar only has `export` as a
/// directive, so the line is read as `export` plus a broken assignment and the
/// recipe never attaches. Replacing the keyword with underscores of the same
/// length makes it an ordinary `word`, which is a rule. The letters come back
/// from the original file in [`restore_make_target_names`] — they have to,
/// because the keyword *is* the name the graph must record.
///
/// A real directive is left alone: `export FOO = bar` and a bare `export` are
/// not followed by `:`, and a recipe line starts with a tab.
pub fn repair_make_keyword_targets(source: &str) -> Option<String> {
    let mut out = source.to_string();
    // Safe because every write is ASCII `_` over an ASCII keyword.
    let buffer = unsafe { out.as_bytes_mut() };
    let mut changed = false;
    let mut offset = 0usize;

    for line in source.split_inclusive('\n') {
        if let Some(keyword) = keyword_target_on_line(line) {
            let start = offset + keyword.start;
            buffer[start..start + keyword.len].fill(b'_');
            changed = true;
        }
        offset += line.len();
    }

    changed.then_some(out)
}

struct KeywordTarget {
    start: usize,
    len: usize,
}

/// A line that declares a target whose name is a directive keyword, or `None`.
fn keyword_target_on_line(line: &str) -> Option<KeywordTarget> {
    if line.starts_with('\t') {
        return None;
    }
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    for keyword in MAKE_TARGET_KEYWORDS {
        let Some(after) = rest.strip_prefix(keyword) else {
            continue;
        };
        // `export:` is the target. `export :=` / `export ::=` are assignments
        // and already parse as the directive.
        let Some(after) = after.strip_prefix(':') else {
            continue;
        };
        if after.starts_with('=') {
            continue;
        }
        return Some(KeywordTarget {
            start: indent,
            len: keyword.len(),
        });
    }
    None
}

/// Put back target names that [`repair_make_keyword_targets`] had to overwrite.
///
/// The rule node starts at the target word, so `start_byte` plus the dummy
/// name's length is the original keyword. Qualified names are rebuilt from
/// that same slice so they stay in lockstep.
pub fn restore_make_target_names(original: &str, extracted: &mut crate::ExtractedFile) {
    for definition in &mut extracted.definitions {
        if !definition.name.bytes().all(|b| b == b'_') {
            continue;
        }
        let end = definition.start_byte + definition.name.len();
        let Some(original_name) = original.get(definition.start_byte..end) else {
            continue;
        };
        if !MAKE_TARGET_KEYWORDS.contains(&original_name) {
            continue;
        }
        if let Some((prefix, _)) = definition.qualified_name.rsplit_once('.') {
            definition.qualified_name = format!("{prefix}.{original_name}");
        } else {
            definition.qualified_name = original_name.to_string();
        }
        definition.name = original_name.to_string();
    }
}

/// Replace `import('mod')` in `import('mod').T[]` with a dummy type identifier.
///
/// The bundled grammar can read `import('mod').T` as a type in some positions,
/// but `array_type` only wraps a `primary_type`, and the import form is not
/// one. The `[]` is then a tuple or a subscript, and `<{ data: import('m').T[] }>`
/// is read as a comparison — the shape that left three files partial in one
/// project.
///
/// `Foo.Bar[]` already parses, so the import call is overwritten with an
/// identifier of the same length (`I` plus underscores). The path is a type
/// query, not an `import_statement`, and is not extracted. `.T` and every
/// definition around it keep their letters.
///
/// `await import('./mod')` and `import('./mod').then(...)` are not followed by
/// `.Ident[]`, so they are left alone. Which `import` to touch is taken from
/// the tree: a string or a comment does not produce an `import` node.
pub fn neutralise_import_type_arrays(language: LanguageId, source: &str) -> Option<String> {
    if !matches!(
        language,
        LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript | LanguageId::Jsx
    ) {
        return None;
    }
    if !source.contains("import(") {
        return None;
    }

    let grammar = crate::registry::grammar(language)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar).ok()?;
    let tree = parser.parse(source, None)?;

    let mut spans = Vec::new();
    collect_import_type_array_spans(tree.root_node(), source, &mut spans);
    if spans.is_empty() {
        return None;
    }

    let mut out = source.to_string();
    // Safe because every write is ASCII over an ASCII `import(...)` span.
    let buffer = unsafe { out.as_bytes_mut() };
    for (start, end) in spans {
        if end <= start || end > buffer.len() {
            continue;
        }
        buffer[start] = b'I';
        buffer[start + 1..end].fill(b'_');
    }
    Some(out)
}

fn collect_import_type_array_spans(
    node: tree_sitter::Node,
    source: &str,
    spans: &mut Vec<(usize, usize)>,
) {
    if node.kind() == "import" {
        if let Some(span) = import_call_followed_by_array_type(node, source) {
            spans.push(span);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_import_type_array_spans(child, source, spans);
    }
}

/// The `import('mod')` span when it is immediately followed by `.Ident[]`.
fn import_call_followed_by_array_type(
    import_kw: tree_sitter::Node,
    source: &str,
) -> Option<(usize, usize)> {
    let call = import_kw.parent()?;
    if call.kind() != "call_expression" {
        return None;
    }
    let rest = source.get(call.end_byte()..)?;
    let rest = rest.strip_prefix('.')?;
    let ident_end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
        .unwrap_or(rest.len());
    if ident_end == 0 {
        return None;
    }
    rest[ident_end..]
        .starts_with("[]")
        .then_some((call.start_byte(), call.end_byte()))
}

/// Overwrite `[start, end)` with `replacement`, padding to the original length
/// so no byte offset in the file shifts.
fn blank_with(buffer: &mut [u8], start: usize, end: usize, replacement: &str) {
    for (offset, slot) in buffer[start..end].iter_mut().enumerate() {
        *slot = replacement.as_bytes().get(offset).copied().unwrap_or(b' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_cpp_is_left_alone() {
        assert_eq!(prepare_cpp("int main() { return 0; }"), None);
    }

    #[test]
    fn byte_length_is_preserved_so_offsets_stay_valid() {
        let source = "class A { Q_OBJECT\nsignals:\n  void go(int n = {});\n};\n";
        let rewritten = prepare_cpp(source).expect("Qt keywords present");
        assert_eq!(
            rewritten.len(),
            source.len(),
            "offsets must not shift: {rewritten:?}"
        );
        assert_eq!(rewritten.lines().count(), source.lines().count());
    }

    #[test]
    fn an_access_section_keeps_its_specifier() {
        let rewritten = prepare_cpp("signals:\n").expect("rewritten");
        assert_eq!(
            rewritten, "public :\n",
            "erasing the word would leave a bare colon, which is itself invalid"
        );
    }

    #[test]
    fn emit_and_q_object_are_erased() {
        let rewritten = prepare_cpp("  Q_OBJECT\n  emit done();\n").expect("rewritten");
        assert_eq!(rewritten, "          \n       done();\n");
    }

    #[test]
    fn a_macro_with_arguments_goes_parentheses_and_all() {
        let rewritten =
            prepare_cpp("Q_PROPERTY(int x READ x NOTIFY xChanged)\nint y;\n").expect("rewritten");
        assert_eq!(
            rewritten,
            "                                        \nint y;\n"
        );
    }

    #[test]
    fn nested_parentheses_in_a_macro_are_balanced() {
        let source = "Q_DECLARE_METATYPE(QVector<QPair<int, int>>)\n";
        let rewritten = prepare_cpp(source).expect("rewritten");
        assert_eq!(rewritten.trim(), "");
        assert_eq!(rewritten.len(), source.len());
    }

    /// A log line or route path containing `emit` is data. Rewriting it would
    /// change what the extractor reads out of the file.
    #[test]
    fn occurrences_inside_strings_and_comments_survive() {
        let source = "// emit here\nconst char *s = \"emit signals\";\n/* Q_OBJECT */\n";
        assert_eq!(
            prepare_cpp(source),
            None,
            "nothing outside a literal or comment needs rewriting"
        );
    }

    #[test]
    fn an_identifier_that_merely_contains_a_keyword_is_untouched() {
        assert_eq!(prepare_cpp("int emitter = 1; int myslots = 2;"), None);
    }

    #[test]
    fn an_empty_brace_default_becomes_a_value_the_grammar_accepts() {
        let rewritten = prepare_cpp("void f(const QString &a = {});\n").expect("rewritten");
        assert_eq!(rewritten, "void f(const QString &a = 0 );\n");
    }

    #[test]
    fn a_brace_default_without_a_space_is_handled_too() {
        assert_eq!(
            prepare_cpp("void f(int a={});\n").expect("rewritten"),
            "void f(int a=0 );\n"
        );
    }

    /// Only the empty pair is a problem; a real initialiser list must survive
    /// untouched, and so must an empty function body.
    #[test]
    fn braces_that_are_not_an_empty_default_are_untouched() {
        assert_eq!(prepare_cpp("int a[] = {1, 2};\n"), None);
        assert_eq!(prepare_cpp("void f() {}\n"), None);
        assert_eq!(prepare_cpp("if (a == b) {}\n"), None);
    }

    /// Erasing these would leave an empty slot between the commas of the
    /// enclosing call, which parses no better than the macro did.
    #[test]
    fn an_argument_position_macro_leaves_a_valid_expression_behind() {
        let source = "f(&p, \"r\", Q_ARG(int, 0), Q_RETURN_ARG(bool, ok));\n";
        let rewritten = prepare_cpp(source).expect("rewritten");
        assert_eq!(rewritten.len(), source.len(), "offsets must not shift");
        assert_eq!(
            rewritten.split_whitespace().collect::<Vec<_>>().join(" "),
            "f(&p, \"r\", 0 , 0 );",
            "each macro must collapse to one expression, not an empty slot"
        );
    }

    #[test]
    fn a_test_entry_point_macro_is_erased() {
        let rewritten = prepare_cpp("QTEST_MAIN(TestLiveBus)\n").expect("rewritten");
        assert_eq!(rewritten.trim(), "");
    }

    /// `>= {}` cannot occur in valid code, but the scan must not mistake the
    /// `=` of a compound operator for the start of an initialiser.
    #[test]
    fn a_compound_operator_is_not_mistaken_for_an_initialiser() {
        assert_eq!(prepare_cpp("while (a >= {});\n"), None);
        assert_eq!(prepare_cpp("if (a != {});\n"), None);
    }

    /// Copied in shape from the one file that still parsed partially: the
    /// initialiser of `names` lives inside the conditional, semicolon and all.
    const SPLIT_DECLARATION: &str = "\
void f() {
    const QStringList names =
#ifdef Q_OS_WIN
        {QStringLiteral(\"a.exe\")};
#else
        {QStringLiteral(\"a\")};
#endif
}
";

    #[test]
    fn a_conditional_that_splits_a_declaration_is_flattened() {
        let flattened = flatten_conditionals(SPLIT_DECLARATION).expect("directives present");

        assert_eq!(
            flattened.len(),
            SPLIT_DECLARATION.len(),
            "offsets must not shift"
        );
        assert_eq!(
            flattened.lines().count(),
            SPLIT_DECLARATION.lines().count(),
            "line numbers must not shift"
        );
        assert!(
            !flattened.contains('#'),
            "every directive line must be gone: {flattened:?}"
        );
        assert!(
            flattened.contains("QStringList names") && flattened.contains("a.exe"),
            "the branch a build would take must survive: {flattened:?}"
        );
        assert!(
            !flattened.contains("QStringLiteral(\"a\")"),
            "the branch not taken must be erased, not merged: {flattened:?}"
        );
    }

    /// The shape that defeats merging: the branches split the signature, so
    /// keeping both produces two openings and one closing brace.
    #[test]
    fn a_conditional_that_splits_a_signature_parses_after_resolution() {
        let source = "\
#ifdef Q_OS_WIN
void windowsOnly() {
#else
void otherwise() {
#endif
    work();
}
";
        let flattened = flatten_conditionals(source).expect("directives present");
        let errors = crate::extract(loci_core::LanguageId::Cpp, "s.cpp", &flattened)
            .expect("parse")
            .error_ranges
            .len();

        assert_eq!(
            errors, 0,
            "resolution must leave a clean parse: {flattened:?}"
        );
        assert!(flattened.contains("windowsOnly"));
    }

    /// `#if 0` is how a block is commented out, so its body is not what a
    /// build compiles and must not become graph symbols.
    #[test]
    fn a_disabled_block_yields_to_its_alternative() {
        let flattened =
            flatten_conditionals("#if 0\nvoid dead() {}\n#else\nvoid live() {}\n#endif\n")
                .expect("directives present");

        assert!(flattened.contains("live"), "got {flattened:?}");
        assert!(
            !flattened.contains("dead"),
            "disabled code must not reach the graph: {flattened:?}"
        );
    }

    /// An inner conditional inside a branch that was dropped must go with it,
    /// rather than resurrecting its own first branch.
    #[test]
    fn a_nested_conditional_inside_a_dropped_branch_stays_dropped() {
        let flattened = flatten_conditionals(
            "#ifdef A\nvoid kept() {}\n#else\n#ifdef B\nvoid buried() {}\n#endif\n#endif\n",
        )
        .expect("directives present");

        assert!(flattened.contains("kept"), "got {flattened:?}");
        assert!(
            !flattened.contains("buried"),
            "nesting must not escape a dropped branch: {flattened:?}"
        );
    }

    /// The whole point is that the grammar can read the result, so assert on
    /// the parser rather than on the text alone.
    #[test]
    fn flattening_removes_the_parse_errors_the_conditional_caused() {
        let before = crate::extract(loci_core::LanguageId::Cpp, "n.cpp", SPLIT_DECLARATION)
            .expect("parse")
            .error_ranges
            .len();
        assert!(before > 0, "the unflattened form must actually fail");

        let flattened = flatten_conditionals(SPLIT_DECLARATION).expect("directives present");
        let after = crate::extract(loci_core::LanguageId::Cpp, "n.cpp", &flattened)
            .expect("parse")
            .error_ranges
            .len();

        assert_eq!(after, 0, "flattening must leave a clean parse, had {after}");
    }

    /// Directives that carry meaning the parser uses must not be swept up
    /// alongside the conditionals.
    #[test]
    fn includes_defines_and_pragmas_are_left_alone() {
        assert_eq!(
            flatten_conditionals("#include <QDir>\n#define N 4\n#pragma once\nint a = N;\n"),
            None
        );
    }

    #[test]
    fn a_hash_with_space_before_the_keyword_is_still_a_directive() {
        let flattened = flatten_conditionals("#  ifdef X\nint a;\n#  endif\n").expect("directive");
        assert!(!flattened.contains('#'), "got {flattened:?}");
        assert!(flattened.contains("int a;"));
    }

    /// A condition spread over continuation lines has to be erased whole, or
    /// the tail is left behind as stray tokens.
    #[test]
    fn a_continued_condition_is_erased_including_its_tail() {
        let flattened =
            flatten_conditionals("#if defined(A) || \\\n    defined(B)\nint a;\n#endif\n")
                .expect("directive");

        assert!(
            !flattened.contains("defined"),
            "the continuation must go too: {flattened:?}"
        );
        assert!(
            flattened.contains("int a;"),
            "the first branch is kept: {flattened:?}"
        );
    }

    #[test]
    fn code_without_conditionals_is_not_rewritten() {
        assert_eq!(flatten_conditionals("int main() { return 0; }\n"), None);
    }

    fn tsx_errors(source: &str) -> usize {
        crate::extract(LanguageId::Tsx, "c.tsx", source)
            .expect("parse")
            .error_ranges
            .len()
    }

    fn ts_errors(source: &str) -> usize {
        crate::extract(LanguageId::TypeScript, "m.ts", source)
            .expect("parse")
            .error_ranges
            .len()
    }

    /// Taken from a component that read
    /// `Broadband Remote Access Server — authentication, accounting & session
    /// control`, where the lone `&` truncated the file.
    #[test]
    fn a_lone_ampersand_in_jsx_text_stops_breaking_the_parse() {
        let source = "const A = () => <Tag color=\"ok\">Registered & Managed</Tag>\n";
        assert!(tsx_errors(source) > 0, "the original must actually fail");

        let rewritten = neutralise_jsx_ampersands(LanguageId::Tsx, source).expect("rewritten");

        assert_eq!(rewritten.len(), source.len(), "offsets must not shift");
        assert_eq!(tsx_errors(&rewritten), 0, "got {rewritten:?}");
        assert!(
            rewritten.contains("Registered") && rewritten.contains("Managed"),
            "only the ampersand may change: {rewritten:?}"
        );
    }

    /// An attribute holding a query string is the other half of the same
    /// fault, and there the parser recovers with a MISSING node rather than an
    /// ERROR, so looking only for ERROR nodes would miss it.
    #[test]
    fn an_ampersand_inside_a_jsx_attribute_is_handled_too() {
        let source = "const A = () => <T url=\"https://m/vt?x={x}&y={y}\" />\n";
        assert!(tsx_errors(source) > 0, "the original must actually fail");

        let rewritten = neutralise_jsx_ampersands(LanguageId::Tsx, source).expect("rewritten");

        assert_eq!(tsx_errors(&rewritten), 0, "got {rewritten:?}");
    }

    /// Markup and expressions nest both ways. Comparing whole spans instead of
    /// asking for the nearest enclosing node left fourteen files unrepaired,
    /// because their elements were written inside a conditional.
    #[test]
    fn an_element_written_inside_an_expression_is_still_markup() {
        let source = "\
export function P(p: { done: boolean }) {
  return (
    <Card>
      {p.done ? (
        <div>
          {p.done ? <Check /> : <Circle />}
          Customer & service selected
        </div>
      ) : null}
    </Card>
  )
}
";
        assert!(tsx_errors(source) > 0, "the original must actually fail");

        let rewritten = neutralise_jsx_ampersands(LanguageId::Tsx, source).expect("rewritten");

        assert_eq!(tsx_errors(&rewritten), 0, "got {rewritten:?}");
        assert!(
            rewritten.contains("p.done ? <Check /> : <Circle />"),
            "the expression beside it must be untouched: {rewritten:?}"
        );
    }

    /// The whole reason for reading the tree. A text-level pass corrupts these
    /// and, measured over the failing files, made four of them worse.
    #[test]
    fn an_ampersand_that_is_an_operator_is_never_touched() {
        for source in [
            "type Both = Left & Right\n",
            "const mask = flags & 0xff\n",
            "const A = () => <p>{left & right}</p>\n",
            "const B = () => <p>{a && b ? 'y' : 'n'}</p>\n",
        ] {
            assert_eq!(
                neutralise_jsx_ampersands(LanguageId::Tsx, source),
                None,
                "must leave operators alone: {source:?}"
            );
        }
    }

    #[test]
    fn a_valid_character_reference_is_left_as_written() {
        for source in [
            "const A = () => <p>a &amp; b</p>\n",
            "const A = () => <p>a &#38; b</p>\n",
            "const A = () => <p>a &#x26; b</p>\n",
        ] {
            assert_eq!(tsx_errors(source), 0, "precondition: {source:?}");
            assert_eq!(neutralise_jsx_ampersands(LanguageId::Tsx, source), None);
        }
    }

    /// Shaped after `frontend/src/types/index.ts`, where `in_rate` closed the
    /// interface and every member below it left the graph.
    const TRUNCATED_INTERFACE: &str = "\
interface PortStats {
  utilization: number
  oper_status: number
  in_octets: number
  measured_at: string
}
";

    fn separated(source: &str) -> Option<String> {
        separate_keyword_members(LanguageId::TypeScript, source)
    }

    #[test]
    fn an_interface_member_named_after_a_keyword_stops_truncating_the_file() {
        assert!(
            ts_errors(TRUNCATED_INTERFACE) > 0,
            "the original must actually fail"
        );

        let rewritten = separated(TRUNCATED_INTERFACE).expect("rewritten");

        assert_eq!(rewritten.len(), TRUNCATED_INTERFACE.len());
        assert_eq!(
            rewritten.lines().count(),
            TRUNCATED_INTERFACE.lines().count(),
            "line numbers must not shift"
        );
        assert_eq!(ts_errors(&rewritten), 0, "got {rewritten:?}");
    }

    /// What truncation actually costs. The interface's own symbol survives
    /// either way, but it is recorded ending at the member the parse died on,
    /// so `get_code_snippet` would hand back half a type.
    #[test]
    fn the_interface_regains_its_true_extent() {
        let end_line = |source: &str| {
            crate::extract(LanguageId::TypeScript, "s.ts", source)
                .expect("parse")
                .definitions
                .iter()
                .find(|d| d.name == "PortStats")
                .expect("the interface itself is found either way")
                .end_line
        };

        let truncated = end_line(TRUNCATED_INTERFACE);
        let whole = end_line(&separated(TRUNCATED_INTERFACE).expect("rewritten"));

        assert_eq!(
            truncated, 3,
            "the parse dies on the member above `in_octets`"
        );
        assert_eq!(whole, 6, "the rewrite must restore the closing brace");
    }

    /// Only whitespace may be overwritten, or a rescued member could arrive
    /// under a mangled name.
    /// The pass ran file-wide at first and put a semicolon into object
    /// literals that were already correct. The net error count still fell, so
    /// the indexer's guard would have kept the damage.

    /// A parameter named `invoice_category` starts with `in` and is followed
    /// by `:`, the same shape as a shadowed interface member. Putting a
    /// semicolon into the parameter list is what left `billingService.ts`
    /// partial after the import-type pass had already cleared its real errors.
    #[test]
    fn a_parameter_named_after_a_keyword_is_left_alone() {
        let source = "\
export const getBillings = async (
  invoice_category: string = '',
  start_date?: string,
): Promise<void> => {}
";
        assert_eq!(ts_errors(source), 0, "this signature is already correct");
        assert_eq!(
            separated(source),
            None,
            "a parameter list must not collect a semicolon"
        );
    }

    /// The pass ran file-wide at first and put a semicolon into object
    /// literals that were already correct. The net error count still fell, so
    /// the indexer's guard would have kept the damage.
    #[test]
    fn an_object_literal_key_named_after_a_keyword_is_left_alone() {
        let source = "\
function f(s: Snapshot) {
  return {
    oper_status: s.oper_status,
    in_rate: toBps(s.in_rate),
    in_octets: 0,
  }
}
";
        assert_eq!(ts_errors(source), 0, "this literal is already correct");
        assert_eq!(
            separated(source),
            None,
            "a comma-separated container must not collect a semicolon"
        );
    }

    #[test]
    fn only_indentation_is_overwritten() {
        let rewritten = separated(TRUNCATED_INTERFACE).expect("rewritten");
        for (before, after) in TRUNCATED_INTERFACE.chars().zip(rewritten.chars()) {
            if before != after {
                assert_eq!(before, ' ', "a non-space byte was overwritten");
                assert_eq!(after, ';');
            }
        }
    }

    #[test]
    fn ordinary_members_and_shallow_indentation_are_left_alone() {
        assert_eq!(
            separated("interface A {\n  a: number\n  b: number\n}\n"),
            None,
            "nothing here is shadowed by a keyword"
        );
        assert_eq!(
            separated("interface A {\n a: number\n in_b: number\n}\n"),
            None,
            "one space of indentation leaves no room for a semicolon"
        );
        assert_eq!(
            separated("interface A {\n\tin_b: number\n}\n"),
            None,
            "a tab is one byte and cannot hold `; `"
        );
    }

    /// Copied from the three files that stayed `parse_partial` after the
    /// earlier TypeScript passes: an inline `import('…').T[]` inside a type
    /// argument or a property type.
    const IMPORT_TYPE_ARRAY: &str = "\
export const getBillingWhatsAppDeliveries = async (id: string) => {
  const response = await api.get<{ data: import('@/types').WAMessage[] }>(
    `/billings/${id}/whatsapp-deliveries`,
  )
  return response.data.data ?? []
}

export interface CreateCategoryRequest {
  required_fields?: import('@/types/workOrder').WorkOrderCategoryField[]
}

export function useSearchONUsByCustomerName(q: string) {
  return useQuery({
    queryFn: async () => {
      const res = await api.get<{ data: import('../services/onuService').ONUNameSearchResult[] }>('/onus/search-by-name')
      return res.data.data ?? []
    },
  })
}
";

    #[test]
    fn an_import_type_array_stops_breaking_the_parse() {
        assert!(
            ts_errors(IMPORT_TYPE_ARRAY) > 0,
            "the original must actually fail"
        );

        let rewritten = neutralise_import_type_arrays(LanguageId::TypeScript, IMPORT_TYPE_ARRAY)
            .expect("rewritten");

        assert_eq!(
            rewritten.len(),
            IMPORT_TYPE_ARRAY.len(),
            "offsets must not shift"
        );
        assert_eq!(
            rewritten.lines().count(),
            IMPORT_TYPE_ARRAY.lines().count(),
            "line numbers must not shift"
        );
        assert_eq!(ts_errors(&rewritten), 0, "got {rewritten:?}");
    }

    /// The functions and the interface are what the graph records. The import
    /// path is a type query, not a name, and is the only thing allowed to change.
    #[test]
    fn import_type_rewrite_keeps_the_symbols_around_it() {
        let rewritten = neutralise_import_type_arrays(LanguageId::TypeScript, IMPORT_TYPE_ARRAY)
            .expect("rewritten");
        let extracted = crate::extract(LanguageId::TypeScript, "s.ts", &rewritten).expect("parse");
        let names: Vec<&str> = extracted
            .definitions
            .iter()
            .map(|d| d.name.as_str())
            .collect();

        assert!(names.contains(&"getBillingWhatsAppDeliveries"), "{names:?}");
        assert!(names.contains(&"CreateCategoryRequest"), "{names:?}");
        assert!(names.contains(&"useSearchONUsByCustomerName"), "{names:?}");
        assert!(
            rewritten.contains("WAMessage") && rewritten.contains("WorkOrderCategoryField"),
            "the imported type names must survive: {rewritten:?}"
        );
    }

    #[test]
    fn a_runtime_import_is_not_rewritten() {
        for source in [
            "const m = await import('./mod')\n",
            "import('./mod').then(x => x)\n",
            "import { Foo } from './mod'\n",
        ] {
            assert_eq!(ts_errors(source), 0, "precondition: {source:?}");
            assert_eq!(
                neutralise_import_type_arrays(LanguageId::TypeScript, source),
                None,
                "must leave runtime imports alone: {source:?}"
            );
        }
    }

    /// Taken from `ai-trainer/Makefile`: a target named `export`, which the
    /// grammar reads as the export directive.
    const EXPORT_TARGET: &str = "\
.PHONY: export
export:
\t@mkdir -p data
\tDB_HOST=$(DB_HOST) \\
\t$(PYTHON) export_training_data.py
";

    #[test]
    fn a_target_named_export_stops_breaking_the_parse() {
        let before = crate::extract(LanguageId::Make, "Makefile", EXPORT_TARGET)
            .expect("parse")
            .error_ranges
            .len();
        assert!(before > 0, "the original must actually fail");

        let rewritten = repair_make_keyword_targets(EXPORT_TARGET).expect("rewritten");

        assert_eq!(
            rewritten.len(),
            EXPORT_TARGET.len(),
            "offsets must not shift"
        );
        assert_eq!(
            rewritten.lines().count(),
            EXPORT_TARGET.lines().count(),
            "line numbers must not shift"
        );

        let mut extracted =
            crate::extract(LanguageId::Make, "Makefile", &rewritten).expect("parse");
        assert_eq!(
            extracted.error_ranges.len(),
            0,
            "rewrite must leave a clean parse: {rewritten:?}"
        );

        restore_make_target_names(EXPORT_TARGET, &mut extracted);
        assert!(
            extracted.definitions.iter().any(|d| d.name == "export"),
            "the target must keep its name: {:?}",
            extracted.definitions
        );
    }

    #[test]
    fn a_real_export_directive_is_left_alone() {
        assert_eq!(
            repair_make_keyword_targets("export FOO = bar\nexport\nunexport FOO\n"),
            None,
            "directives must not be rewritten as targets"
        );
        assert_eq!(
            repair_make_keyword_targets("exports:\n\t@echo ok\n"),
            None,
            "a target that merely contains the word is already a rule"
        );
    }
}
