//! Cheap structural indexing: extracts top-level symbol names (functions,
//! structs, classes, traits, etc.) from a file's text via tree-sitter, so
//! the relevance ranking in `ws::Hub::relevant_files` has something better
//! than "most recently saved" to rank synced files by. This is the Layer-2
//! "shared context" piece of the 50-person-session design: a real structural
//! index, not a context dump, bounded by what's actually synced rather than
//! project size.
//!
//! One generic walker instead of a per-language tree-sitter query: the node
//! kind names for definitions are consistent enough across these five
//! grammars (e.g. "function_item", "function_definition",
//! "function_declaration" all contain "function") that a substring match on
//! `kind()` plus "find the first identifier-ish child" covers every
//! language here without maintaining five separate `.scm` query files. It's
//! a heuristic, not a compiler-grade parse — exactly matched to this being a
//! relevance signal, not a correctness-critical one.

use tree_sitter::{Language, Node, Parser};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
}

// Node kinds whose name contains one of these substrings are treated as a
// definition worth indexing. Deliberately broad (covers fn/struct/class/
// trait/impl/interface/enum/type-alias/const across all five grammars)
// since a false positive here just adds a harmless extra candidate name,
// while a false negative silently blinds the relevance ranking to a real
// symbol — the two outcomes aren't symmetric, so this errs toward including.
const DEFINITION_KIND_MARKERS: &[&str] = &[
    "function",
    "method",
    "struct",
    "class",
    "trait",
    "impl",
    "interface",
    "enum",
    "type_alias",
    "const_item",
    "static_item",
];

// Child node kinds that hold the definition's own name. Checked in this
// order so e.g. a Rust `type_identifier` (struct/enum/trait names) is found
// even though `identifier` (function/mod names) is also present elsewhere
// under the same parent.
const NAME_KIND_MARKERS: &[&str] = &[
    "type_identifier",
    "property_identifier",
    "field_identifier",
    "identifier",
];

fn language_for_path(path: &str) -> Option<Language> {
    let extension = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        _ => return None,
    })
}

/// Extracts definition names from `content`, based on `path`'s extension.
/// Returns an empty vec for unsupported languages or a parse failure —
/// callers fall back to a raw substring match over file content in that
/// case, so an unsupported file type degrades the ranking's precision
/// rather than breaking it.
pub fn extract_symbols(path: &str, content: &str) -> Vec<Symbol> {
    let Some(language) = language_for_path(path) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let bytes = content.as_bytes();
    let mut symbols = Vec::new();
    let mut cursor = tree.walk();
    walk(&mut cursor, bytes, &mut symbols);
    symbols
}

fn walk(cursor: &mut tree_sitter::TreeCursor, bytes: &[u8], out: &mut Vec<Symbol>) {
    loop {
        let node = cursor.node();
        if is_definition(&node)
            && let Some(name) = definition_name(&node, bytes)
        {
            out.push(Symbol { name });
        }
        if cursor.goto_first_child() {
            walk(cursor, bytes, out);
            cursor.goto_parent();
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn is_definition(node: &Node) -> bool {
    let kind = node.kind();
    DEFINITION_KIND_MARKERS
        .iter()
        .any(|marker| kind.contains(marker))
}

fn definition_name(node: &Node, bytes: &[u8]) -> Option<String> {
    for marker in NAME_KIND_MARKERS {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind().contains(marker) {
                return child.utf8_text(bytes).ok().map(str::to_string);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols() {
        let content =
            "struct Invoice { total: u32 }\n\nfn calculate_total(items: &[Item]) -> u32 { 0 }\n";
        let names: Vec<_> = extract_symbols("invoice.rs", content)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"Invoice".to_string()));
        assert!(names.contains(&"calculate_total".to_string()));
    }

    #[test]
    fn extracts_python_symbols() {
        let content = "class Invoice:\n    def calculate_total(self):\n        pass\n";
        let names: Vec<_> = extract_symbols("invoice.py", content)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"Invoice".to_string()));
        assert!(names.contains(&"calculate_total".to_string()));
    }

    #[test]
    fn extracts_typescript_symbols() {
        let content = "export function calculateTotal(items: Item[]): number {\n  return 0;\n}\n";
        let names: Vec<_> = extract_symbols("invoice.ts", content)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"calculateTotal".to_string()));
    }

    #[test]
    fn unsupported_extension_returns_empty() {
        assert_eq!(
            extract_symbols("notes.md", "# calculate_total\n"),
            Vec::new()
        );
    }
}
