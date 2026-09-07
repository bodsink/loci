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
}
