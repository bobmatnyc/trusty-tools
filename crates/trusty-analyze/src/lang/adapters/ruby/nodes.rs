//! Ruby AST node builders for the KG adapter.
//!
//! Why: node/qualified-id construction is a self-contained concern lifted out
//! of the walker so `mod.rs` stays under the 500-SLOC cap (see #1195).
//! What: helpers that turn tree-sitter nodes into `KgNode`s.
//! Test: exercised end-to-end via the adapter tests in `super`.

use crate::types::{CodeChunk, KgNode, KgNodeKind};
use tree_sitter::Node;

pub(crate) fn file_node(file: &str) -> KgNode {
    KgNode {
        id: format!("ruby:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "ruby".into(),
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
    // Ruby visibility is dynamic; treat anything not starting with `_` as public.
    let is_public = !name.starts_with('_');
    KgNode {
        id: format!("ruby:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "ruby".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public,
        extra: serde_json::Value::Null,
    }
}

/// Build a method node with class-qualified ID/qualified_name. Mirrors the
/// Python adapter's strategy so methods on different classes don't collide.
///
/// Why: Ruby method names are short and frequently shared across classes
/// (e.g. `initialize`, `to_s`, `call`); without a class qualifier the linker
/// would merge them.
/// What: Returns a `KgNode` with `id = ruby:Method:file:Class:name` and
/// `qualified_name = Class.name`. When `class_name` is empty, falls back to
/// the bare name (top-level `def`).
/// Test: `ruby_extracts_class_methods_with_qualified_ids`,
/// `ruby_singleton_method_uses_class_qualified_name`.
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
        id: format!("ruby:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "ruby".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        // Ruby has no `_` privacy convention by default, but we mirror the
        // other adapters so internal helpers like `_internal` are flagged.
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}
