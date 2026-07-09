//! PHP AST node builders for the KG adapter.
//!
//! Why: node/qualified-id construction is a self-contained concern lifted out
//! of the walker so `mod.rs` stays under the 500-SLOC cap (see #1195).
//! What: helpers that turn tree-sitter nodes into `KgNode`s and read node text.
//! Test: exercised end-to-end via the adapter tests in `super`.

use crate::types::{CodeChunk, KgNode, KgNodeKind};
use tree_sitter::Node;

pub(crate) fn file_node(file: &str) -> KgNode {
    KgNode {
        id: format!("php:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "php".into(),
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

pub(crate) fn make_simple_node(
    kind: KgNodeKind,
    name: &str,
    chunk: &CodeChunk,
    ast: Node,
) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    // PHP visibility is keyword-driven; underscore prefix is purely
    // conventional. Mirror the other adapters and treat names without a
    // leading underscore as public by default.
    let is_public = !name.starts_with('_');
    KgNode {
        id: format!("php:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "php".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public,
        extra: serde_json::Value::Null,
    }
}

/// Build a method node with class-qualified ID/qualified_name. Mirrors the
/// Python and Ruby adapter strategy so methods on different classes don't
/// collide.
///
/// Why: PHP method names (`__construct`, `handle`, `__toString`) are reused
/// across countless classes. Without a class qualifier the cross-chunk linker
/// would merge them into one node.
/// What: Returns a `KgNode` with `id = php:Method:file:Class:name` and
/// `qualified_name = Class.name`. When `class_name` is empty falls back to the
/// bare name (which only happens when a stray `method_declaration` appears
/// outside a `declaration_list`, an unusual case).
/// Test: `php_extracts_class_methods_with_qualified_ids`.
pub(crate) fn make_method_node(
    class_name: &str,
    name: &str,
    chunk: &CodeChunk,
    ast: Node,
) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let qualified = if class_name.is_empty() {
        name.to_string()
    } else {
        format!("{class_name}.{name}")
    };
    let id_suffix = if class_name.is_empty() {
        name.to_string()
    } else {
        format!("{class_name}:{name}")
    };
    KgNode {
        id: format!("php:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "php".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}
