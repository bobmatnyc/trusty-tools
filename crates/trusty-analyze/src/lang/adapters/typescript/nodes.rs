//! TypeScript/JSX AST node builders for the KG adapter.
//!
//! Why: node/qualified-id construction is a self-contained concern lifted out
//! of the walker so `mod.rs` stays under the 500-SLOC cap (see #1195).
//! What: helpers that turn tree-sitter nodes into `KgNode`s and read node text.
//! Test: exercised end-to-end via the adapter tests in `super`.

use crate::types::{CodeChunk, KgNode, KgNodeKind};
use tree_sitter::Node;

pub(crate) fn file_node(file: &str, language: &str) -> KgNode {
    KgNode {
        id: format!("{language}:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: language.to_string(),
        file: file.to_string(),
        start_line: 0,
        end_line: 0,
        doc_comment: None,
        is_public: false,
        extra: serde_json::Value::Null,
    }
}

pub(crate) fn node_text(node: Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

pub(crate) fn name_of(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

pub(crate) fn make_node(
    kind: KgNodeKind,
    name: &str,
    chunk: &CodeChunk,
    ast: Node,
    language: &str,
) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("{language}:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: language.to_string(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: false,
        extra: serde_json::Value::Null,
    }
}
