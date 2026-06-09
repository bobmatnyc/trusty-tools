//! Swift `LanguageAnalyzer` adapter backed by tree-sitter-swift.
//!
//! Why: Provides static analysis for Swift source so the registry can ingest
//! `.swift` files. Mirrors the other adapters' class-qualified method IDs.
//!
//! What: For each `CodeChunk` parses with tree-sitter-swift, walks the tree,
//! and emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for top-level `function_declaration`
//! - `Method` nodes (type-qualified) for `function_declaration` inside a
//!   `class_body` / `enum_class_body` / `protocol_body`
//! - `Class` nodes for `class_declaration` (which the swift grammar uses for
//!   `class`, `struct`, and `enum` — differentiated by `declaration_kind`)
//! - `Interface` nodes for `protocol_declaration`
//! - `Import` nodes + `Imports` edges for `import_declaration`
//! - `Calls` edges from each function/method to its callees, scoped to the
//!   enclosing function/method, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-swift-backed analyzer.
pub struct SwiftAnalyzer;

impl SwiftAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SwiftAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for SwiftAnalyzer {
    fn language(&self) -> &str {
        "swift"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".swift"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-swift grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "swift analyze chunk");
            let Some(tree) = parser.parse(&chunk.content, None) else {
                result.errors.push(format!("parse failure: {}", chunk.file));
                continue;
            };
            result.analyzed_chunks += 1;
            if seen_files.insert(chunk.file.clone()) {
                result.analyzed_files += 1;
                result.graph.nodes.push(file_node(&chunk.file));
            }

            let src = chunk.content.as_bytes();
            let file_id = format!("swift:File:{}", chunk.file);
            recurse(
                tree.root_node(),
                src,
                chunk,
                &mut result.graph,
                &file_id,
                None,
            );
        }

        result
    }
}

fn file_node(file: &str) -> KgNode {
    KgNode {
        id: format!("swift:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "swift".into(),
        file: file.to_string(),
        start_line: 0,
        end_line: 0,
        doc_comment: None,
        is_public: false,
        extra: serde_json::Value::Null,
    }
}

fn node_text(node: Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn name_of(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

fn make_simple_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("swift:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "swift".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// Build a method node where the ID is `swift:Method:file:Type:Name`.
fn make_method_node(type_name: &str, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let qualified = if type_name.is_empty() {
        name.to_string()
    } else {
        format!("{type_name}.{name}")
    };
    let id_suffix = if type_name.is_empty() {
        name.to_string()
    } else {
        format!("{type_name}:{name}")
    };
    KgNode {
        id: format!("swift:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "swift".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

fn recurse(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
    type_name: Option<&str>,
) {
    match node.kind() {
        "function_declaration" | "protocol_function_declaration" => {
            // Name field is a `simple_identifier`.
            let name = name_of(node, src).or_else(|| {
                // Fallback: first simple_identifier child.
                let mut cursor = node.walk();
                let result = node
                    .children(&mut cursor)
                    .find(|c| c.kind() == "simple_identifier")
                    .map(|c| node_text(c, src));
                result
            });
            let Some(name) = name else { return };
            let (id, _) = if let Some(tn) = type_name {
                let n = make_method_node(tn, &name, chunk, node);
                let id = n.id.clone();
                graph.nodes.push(n);
                (id, "Method")
            } else {
                let n = make_simple_node(KgNodeKind::Function, &name, chunk, node);
                let id = n.id.clone();
                graph.nodes.push(n);
                (id, "Function")
            };
            graph.edges.push(KgEdge {
                from: parent_id.to_string(),
                to: id.clone(),
                kind: KgEdgeKind::Contains,
                weight: 1.0,
            });
            // Body is `function_body` (no `body:` field on protocol decls).
            let body = node.child_by_field_name("body").or_else(|| {
                let mut cursor = node.walk();
                let result = node
                    .children(&mut cursor)
                    .find(|c| c.kind() == "function_body");
                result
            });
            if let Some(body) = body {
                for edge in extract_calls(body, src, &id) {
                    graph.edges.push(edge);
                }
            }
            return;
        }
        "class_declaration" => {
            // The swift grammar uses class_declaration for class/struct/enum;
            // they're all aggregate types so we map all to KgNodeKind::Class.
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = node_text(name_node, src);
            let n = make_simple_node(KgNodeKind::Class, &name, chunk, node);
            let class_id = n.id.clone();
            graph.nodes.push(n);
            graph.edges.push(KgEdge {
                from: parent_id.to_string(),
                to: class_id.clone(),
                kind: KgEdgeKind::Contains,
                weight: 1.0,
            });
            // Recurse into class_body / enum_class_body.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "class_body" | "enum_class_body") {
                    let mut c2 = child.walk();
                    for inner in child.children(&mut c2) {
                        recurse(inner, src, chunk, graph, &class_id, Some(&name));
                    }
                }
            }
            return;
        }
        "protocol_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = node_text(name_node, src);
            let n = make_simple_node(KgNodeKind::Interface, &name, chunk, node);
            let iface_id = n.id.clone();
            graph.nodes.push(n);
            graph.edges.push(KgEdge {
                from: parent_id.to_string(),
                to: iface_id.clone(),
                kind: KgEdgeKind::Contains,
                weight: 1.0,
            });
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "protocol_body" {
                    let mut c2 = child.walk();
                    for inner in child.children(&mut c2) {
                        recurse(inner, src, chunk, graph, &iface_id, Some(&name));
                    }
                }
            }
            return;
        }
        "import_declaration" => {
            emit_import(node, src, chunk, graph, parent_id);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        recurse(child, src, chunk, graph, parent_id, type_name);
    }
}

fn emit_import(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    // import_declaration has children including `import` keyword + `identifier`
    // (which itself nests `simple_identifier` parts joined by `.`).
    let mut target: Option<String> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                target = Some(node_text(child, src));
                break;
            }
            "simple_identifier" => {
                target = Some(node_text(child, src));
                break;
            }
            _ => {}
        }
    }
    let Some(target) = target else { return };
    let target = target.trim().to_string();
    if target.is_empty() {
        return;
    }
    let n = make_simple_node(KgNodeKind::Import, &target, chunk, node);
    let id = n.id.clone();
    graph.nodes.push(n);
    graph.edges.push(KgEdge {
        from: parent_id.to_string(),
        to: id,
        kind: KgEdgeKind::Imports,
        weight: 1.0,
    });
}

fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Stop at nested function-like / class bodies.
        match node.kind() {
            "function_declaration"
            | "class_declaration"
            | "protocol_declaration"
            | "lambda_literal" => {
                return;
            }
            "call_expression" => {
                if let Some(callee) = callee_name(node, src) {
                    *counts.entry(callee).or_insert(0) += 1;
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, src, counts);
        }
    }

    visit(body, src, &mut counts);

    counts
        .into_iter()
        .map(|(callee, count)| KgEdge {
            from: caller_id.to_string(),
            to: build_call_target("swift", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Resolve a best-effort callee name from a Swift `call_expression`.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let mut cursor = call.walk();
    for child in call.children(&mut cursor) {
        match child.kind() {
            "simple_identifier" => return Some(node_text(child, src)),
            "navigation_expression" => return navigation_leaf_name(child, src),
            _ => {}
        }
    }
    None
}

/// `a.b.c()` → `c`. The swift `navigation_expression` has a `suffix:` field
/// containing the property/method being accessed.
fn navigation_leaf_name(node: Node, src: &[u8]) -> Option<String> {
    if let Some(suffix) = node.child_by_field_name("suffix") {
        // suffix wraps a simple_identifier
        if suffix.kind() == "simple_identifier" {
            return Some(node_text(suffix, src));
        }
        let mut cursor = suffix.walk();
        for child in suffix.children(&mut cursor) {
            if child.kind() == "simple_identifier" {
                return Some(node_text(child, src));
            }
        }
    }
    // Fallback: rightmost simple_identifier child.
    let mut cursor = node.walk();
    let mut last: Option<String> = None;
    for child in node.children(&mut cursor) {
        if child.kind() == "simple_identifier" {
            last = Some(node_text(child, src));
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str, file: &str) -> CodeChunk {
        CodeChunk {
            id: format!("{file}:1:10"),
            file: file.into(),
            start_line: 1,
            end_line: 10,
            content: content.into(),
            function_name: None,
            score: 0.0,
            compact_snippet: None,
            match_reason: String::new(),
        }
    }

    #[test]
    fn swift_supports_dot_swift() {
        let a = SwiftAnalyzer::new();
        assert!(a.supports("Foo.swift"));
        assert!(!a.supports("foo.kt"));
    }

    #[test]
    fn swift_extracts_top_level_function() {
        let a = SwiftAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("func top() { hello() }\n", "f.swift")]);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(funcs[0].name, "top");
    }

    #[test]
    fn swift_class_method_is_qualified() {
        let a = SwiftAnalyzer::new();
        let src = "class Foo {\n  func greet() {\n    hello()\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.swift")]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(methods[0].name, "greet");
        assert!(
            methods[0].id.contains(":Foo:greet"),
            "id should embed Foo, got {}",
            methods[0].id
        );
        assert_eq!(methods[0].qualified_name, "Foo.greet");
    }

    #[test]
    fn swift_protocol_emits_interface() {
        let a = SwiftAnalyzer::new();
        let src = "protocol P {\n  func bar()\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.swift")]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Interface) && n.name == "P"),
            "expected Interface P, nodes: {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn swift_call_edges_scoped_and_deduped() {
        let a = SwiftAnalyzer::new();
        let src =
            "class Foo {\n  func greet() {\n    hello()\n    hello()\n    obj.method()\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.swift")]);
        let calls: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        let hello_edges: Vec<_> = calls.iter().filter(|e| e.to.ends_with(":hello")).collect();
        assert_eq!(
            hello_edges.len(),
            1,
            "expected one deduped hello edge: {calls:?}"
        );
        assert!(
            (hello_edges[0].weight - 2.0).abs() < f32::EPSILON,
            "weight should be 2, got {}",
            hello_edges[0].weight
        );
        let method_edges: Vec<_> = calls.iter().filter(|e| e.to.ends_with(":method")).collect();
        assert_eq!(method_edges.len(), 1, "expected one method edge: {calls:?}");
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Method:") && e.from.contains(":Foo:greet")),
            "all calls should originate from Foo.greet, got {calls:?}"
        );
    }

    #[test]
    fn swift_extracts_imports() {
        let a = SwiftAnalyzer::new();
        let src = "import Foundation\nimport UIKit\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.swift")]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Foundation"), "got {names:?}");
        assert!(names.contains(&"UIKit"), "got {names:?}");
    }

    #[test]
    fn swift_struct_emits_class() {
        let a = SwiftAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("struct S { func g() {} }\n", "f.swift")]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "S"));
    }
}
