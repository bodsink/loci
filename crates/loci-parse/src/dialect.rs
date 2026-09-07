//! Making C++ sources parseable by a grammar that does not cover them.
//!
//! Two gaps cost real symbols on real repositories:
//!
//! 1. tree-sitter-cpp implements C++, not Qt. Qt's moc keywords — `Q_OBJECT`,
//!    `signals:`, `emit` — are macros that never reach a standards compliant
//!    parser, so a Qt codebase parses as a field of syntax errors.
//! 2. The grammar rejects `= {}` as a default argument, which is ordinary
//!    C++11. One project here used it 42 times across 10 files.
//!
//! Both are handled by rewriting the source in place, preserving byte length
//! exactly, so every offset, line and column the parser reports still points at
//! the original file. Only structure is touched, never a name or a literal, and
//! callers keep reading real source from disk. The indexer applies this only
//! after a direct parse has already failed, and keeps the result only if it
//! parses better, so a file the grammar already handles is never rewritten.
//!
//! Measured on a Qt project of 94 tracked C/C++ files: 52 files and 187 error
//! nodes before, 21 files and 66 error nodes after the Qt pass alone.

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
}
