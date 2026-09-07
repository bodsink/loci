use loci_core::LanguageId;
use tree_sitter::Language;

/// A grammar that is compiled into this binary.
///
/// Only languages listed here can be parsed. `get_graph_schema` reports exactly
/// this list, so the engine never claims coverage it does not have.
#[derive(Debug, Clone, Copy)]
pub struct BundledGrammar {
    pub language: LanguageId,
    /// Upstream grammar crate, so users can audit what is linked in.
    pub grammar_crate: &'static str,
}

pub const BUNDLED: &[BundledGrammar] = &[
    BundledGrammar {
        language: LanguageId::Python,
        grammar_crate: "tree-sitter-python",
    },
    BundledGrammar {
        language: LanguageId::JavaScript,
        grammar_crate: "tree-sitter-javascript",
    },
    BundledGrammar {
        language: LanguageId::Jsx,
        grammar_crate: "tree-sitter-javascript",
    },
    BundledGrammar {
        language: LanguageId::TypeScript,
        grammar_crate: "tree-sitter-typescript",
    },
    BundledGrammar {
        language: LanguageId::Tsx,
        grammar_crate: "tree-sitter-typescript",
    },
    BundledGrammar {
        language: LanguageId::Go,
        grammar_crate: "tree-sitter-go",
    },
    BundledGrammar {
        language: LanguageId::Rust,
        grammar_crate: "tree-sitter-rust",
    },
    BundledGrammar {
        language: LanguageId::C,
        grammar_crate: "tree-sitter-c",
    },
    BundledGrammar {
        language: LanguageId::Cpp,
        grammar_crate: "tree-sitter-cpp",
    },
    BundledGrammar {
        language: LanguageId::Java,
        grammar_crate: "tree-sitter-java",
    },
    BundledGrammar {
        language: LanguageId::CSharp,
        grammar_crate: "tree-sitter-c-sharp",
    },
    BundledGrammar {
        language: LanguageId::Kotlin,
        grammar_crate: "tree-sitter-kotlin-ng",
    },
    BundledGrammar {
        language: LanguageId::Perl,
        grammar_crate: "tree-sitter-perl",
    },
    BundledGrammar {
        language: LanguageId::Bash,
        grammar_crate: "tree-sitter-bash",
    },
    BundledGrammar {
        language: LanguageId::Toml,
        grammar_crate: "tree-sitter-toml-ng",
    },
    BundledGrammar {
        language: LanguageId::Yaml,
        grammar_crate: "tree-sitter-yaml",
    },
    BundledGrammar {
        language: LanguageId::Ini,
        grammar_crate: "tree-sitter-ini",
    },
    BundledGrammar {
        language: LanguageId::Make,
        grammar_crate: "tree-sitter-make",
    },
    BundledGrammar {
        language: LanguageId::Cmake,
        grammar_crate: "tree-sitter-cmake",
    },
    BundledGrammar {
        language: LanguageId::Html,
        grammar_crate: "tree-sitter-html",
    },
    BundledGrammar {
        language: LanguageId::Dart,
        grammar_crate: "tree-sitter-dart",
    },
];

pub fn is_bundled(language: LanguageId) -> bool {
    BUNDLED.iter().any(|g| g.language == language)
}

pub fn bundled_language_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = BUNDLED.iter().map(|g| g.language.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Load the tree-sitter grammar for a language, or `None` if not bundled.
pub fn grammar(language: LanguageId) -> Option<Language> {
    Some(match language {
        LanguageId::Python => Language::new(tree_sitter_python::LANGUAGE),
        LanguageId::JavaScript | LanguageId::Jsx => Language::new(tree_sitter_javascript::LANGUAGE),
        LanguageId::TypeScript => Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
        LanguageId::Tsx => Language::new(tree_sitter_typescript::LANGUAGE_TSX),
        LanguageId::Go => Language::new(tree_sitter_go::LANGUAGE),
        LanguageId::Rust => Language::new(tree_sitter_rust::LANGUAGE),
        LanguageId::C => Language::new(tree_sitter_c::LANGUAGE),
        LanguageId::Cpp => Language::new(tree_sitter_cpp::LANGUAGE),
        LanguageId::Java => Language::new(tree_sitter_java::LANGUAGE),
        LanguageId::CSharp => Language::new(tree_sitter_c_sharp::LANGUAGE),
        LanguageId::Kotlin => Language::new(tree_sitter_kotlin_ng::LANGUAGE),
        LanguageId::Perl => Language::new(tree_sitter_perl::LANGUAGE),
        LanguageId::Bash => Language::new(tree_sitter_bash::LANGUAGE),
        LanguageId::Toml => Language::new(tree_sitter_toml_ng::LANGUAGE),
        LanguageId::Yaml => Language::new(tree_sitter_yaml::LANGUAGE),
        LanguageId::Ini => Language::new(tree_sitter_ini::LANGUAGE),
        LanguageId::Make => Language::new(tree_sitter_make::LANGUAGE),
        LanguageId::Cmake => Language::new(tree_sitter_cmake::LANGUAGE),
        LanguageId::Html => Language::new(tree_sitter_html::LANGUAGE),
        LanguageId::Dart => Language::new(tree_sitter_dart::LANGUAGE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_language_loads_a_grammar() {
        for entry in BUNDLED {
            assert!(
                grammar(entry.language).is_some(),
                "{} claims to be bundled but has no grammar",
                entry.language
            );
        }
    }

    /// Every language named for Hybrid LSP now has a grammar, so an eligible
    /// language can never be silently unparseable.
    #[test]
    fn every_lsp_scoped_language_is_bundled() {
        for language in [
            LanguageId::Python,
            LanguageId::TypeScript,
            LanguageId::Tsx,
            LanguageId::JavaScript,
            LanguageId::Jsx,
            LanguageId::CSharp,
            LanguageId::Go,
            LanguageId::C,
            LanguageId::Cpp,
            LanguageId::Java,
            LanguageId::Kotlin,
            LanguageId::Rust,
            LanguageId::Perl,
        ] {
            assert!(is_bundled(language), "{language} must have a grammar");
            assert!(grammar(language).is_some(), "{language} must load");
        }
    }

    /// A grammar that links but cannot parse its own language is worse than a
    /// missing one, because it reports success on an empty tree.
    #[test]
    fn grammars_can_actually_parse_their_language() {
        let cases: &[(LanguageId, &str, &str)] = &[
            (LanguageId::Python, "def hello():\n    return 1\n", "module"),
            (
                LanguageId::CSharp,
                "class Greeter { public int Hello() { return 1; } }\n",
                "compilation_unit",
            ),
            (
                LanguageId::Kotlin,
                "fun hello(): Int {\n    return 1\n}\n",
                "source_file",
            ),
            (
                LanguageId::Perl,
                "sub hello {\n    return 1;\n}\n",
                "source_file",
            ),
        ];

        for (language, source, expected_root) in cases {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&grammar(*language).expect("grammar"))
                .expect("set language");
            let tree = parser.parse(source, None).expect("parse");
            assert_eq!(tree.root_node().kind(), *expected_root, "{language}");
            assert!(
                !tree.root_node().has_error(),
                "{language} parsed with errors"
            );
        }
    }
}
