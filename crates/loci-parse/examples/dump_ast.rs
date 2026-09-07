//! Print the tree-sitter s-expression for a snippet, so query specs are written
//! against the grammar's real node names instead of guessed ones.
//!
//! Usage: cargo run -p loci-parse --example dump_ast -- <language> <<'EOF' ... EOF

use loci_core::LanguageId;
use std::io::Read;

fn main() {
    let mut args = std::env::args().skip(1);
    let language = args.next().expect("language id");
    let language = LanguageId::from_str_id(&language).expect("known language");

    // `--rewrite` runs the source through the same dialect passes the indexer
    // uses, so what is dumped is what the indexer actually parsed. Without it a
    // dump can look broken in a way the indexer never sees.
    let rewrite = matches!(args.next().as_deref(), Some("--rewrite" | "--qt"));

    let mut source = String::new();
    std::io::stdin().read_to_string(&mut source).expect("stdin");
    if rewrite {
        // Applied in turn, each on the output of the last, because a file can
        // need more than one of them.
        let passes: [fn(LanguageId, &str) -> Option<String>; 4] = [
            |_, s| loci_parse::prepare_cpp(s),
            |_, s| loci_parse::flatten_conditionals(s),
            loci_parse::neutralise_jsx_ampersands,
            loci_parse::separate_keyword_members,
        ];
        for pass in passes {
            if let Some(rewritten) = pass(language, &source) {
                source = rewritten;
            }
        }
    }

    let grammar = loci_parse::grammar(language).expect("bundled grammar");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar).expect("set language");
    let tree = parser.parse(&source, None).expect("parse");

    // The tree dump only shows named nodes, so a MISSING node the parser
    // inserted while recovering is invisible in it. Report the ranges the
    // indexer actually records, or a dump looks clean when indexing is not.
    match loci_parse::extract(language, "dump.txt", &source) {
        Ok(extracted) if !extracted.error_ranges.is_empty() => {
            println!("error_ranges: {:?}", extracted.error_ranges);
        }
        Ok(_) => println!("error_ranges: none"),
        Err(e) => println!("extract failed: {e}"),
    }

    print(tree.root_node(), &source, 0);
}

fn print(node: tree_sitter::Node, source: &str, depth: usize) {
    let indent = "  ".repeat(depth);
    let text = &source[node.byte_range()];
    let preview: String = text.chars().take(40).collect();
    let preview = preview.replace('\n', "\\n");
    println!("{indent}{}  {:?}", node.kind(), preview);

    let mut cursor = node.walk();
    for (index, child) in node.children(&mut cursor).enumerate() {
        if !child.is_named() {
            continue;
        }
        // Field names are what queries actually match on, so show them.
        if let Some(field) = node.field_name_for_child(index as u32) {
            println!("{indent}  .{field}:");
        }
        print(child, source, depth + 1);
    }
}
