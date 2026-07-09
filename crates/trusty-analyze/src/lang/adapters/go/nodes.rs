//! Go AST node builders and node-shape inspection helpers for the KG adapter.
//!
//! Why: node/qualified-id construction and the small predicates that decide
//! what a declaration *is* (test function? which receiver?) are a
//! self-contained concern lifted out of the walker so `mod.rs` stays under
//! the 500-SLOC cap (see #1195).
//! What: helpers that turn tree-sitter nodes into `KgNode`s, read node text,
//! and classify function/method declarations.
//! Test: exercised end-to-end via the adapter tests in `super::tests`.

use crate::types::{CodeChunk, KgNode, KgNodeKind};
use tree_sitter::Node;

pub(crate) fn file_node(file: &str) -> KgNode {
    KgNode {
        id: format!("go:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "go".into(),
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

/// Capitalized identifier → exported (`is_public: true`) in Go.
pub(crate) fn is_exported(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Walk backward through preceding comment siblings and join them.
pub(crate) fn preceding_doc(node: Node, src: &[u8]) -> Option<String> {
    let mut sib = node.prev_sibling();
    let mut parts: Vec<String> = Vec::new();
    while let Some(s) = sib {
        if s.kind() == "comment" {
            parts.push(node_text(s, src));
            sib = s.prev_sibling();
        } else {
            break;
        }
    }
    if parts.is_empty() {
        None
    } else {
        parts.reverse();
        Some(parts.join("\n"))
    }
}

pub(crate) fn make_node(
    kind: KgNodeKind,
    name: &str,
    chunk: &CodeChunk,
    ast: Node,
    doc: Option<String>,
) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("go:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "go".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: doc,
        is_public: is_exported(name),
        extra: serde_json::Value::Null,
    }
}

/// Build a method node where the ID is `go:Method:file:Receiver:Name` so
/// methods on different receivers don't collide. The displayed `name` stays
/// just the method name; `qualified_name` includes the receiver.
pub(crate) fn make_method_node(
    receiver: &str,
    name: &str,
    chunk: &CodeChunk,
    ast: Node,
    doc: Option<String>,
) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let qualified = if receiver.is_empty() {
        name.to_string()
    } else {
        format!("{receiver}.{name}")
    };
    let id_suffix = if receiver.is_empty() {
        name.to_string()
    } else {
        format!("{receiver}:{name}")
    };
    KgNode {
        id: format!("go:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "go".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: doc,
        is_public: is_exported(name),
        extra: serde_json::Value::Null,
    }
}

/// Inspect a `function_declaration` to decide if it's a Go test function
/// (name starts with `Test` and first parameter is `*testing.T`).
pub(crate) fn is_test_function(name: &str, fn_node: Node, src: &[u8]) -> bool {
    if !name.starts_with("Test") {
        return false;
    }
    let Some(params) = fn_node.child_by_field_name("parameters") else {
        return false;
    };
    let txt = node_text(params, src);
    txt.contains("testing.T")
}

/// Extract the receiver type name from a `method_declaration` node.
///
/// Why: Methods need to be uniquely keyed by receiver type so `(*Foo).Bar`
/// and `(*Baz).Bar` don't collapse into the same graph node.
///
/// What: Reads the `receiver` field (a `parameter_list`), descends into the
/// first `parameter_declaration`, and returns the underlying `type_identifier`
/// text — stripping a leading `*` for pointer receivers.
pub(crate) fn receiver_type(method: Node, src: &[u8]) -> Option<String> {
    let receiver = method.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() != "parameter_declaration" {
            continue;
        }
        let ty = child.child_by_field_name("type")?;
        // ty is either `type_identifier` or `pointer_type`.
        match ty.kind() {
            "type_identifier" => return Some(node_text(ty, src)),
            "pointer_type" => {
                let mut tc = ty.walk();
                for tchild in ty.children(&mut tc) {
                    if tchild.kind() == "type_identifier" {
                        return Some(node_text(tchild, src));
                    }
                }
            }
            _ => {}
        }
    }
    None
}
