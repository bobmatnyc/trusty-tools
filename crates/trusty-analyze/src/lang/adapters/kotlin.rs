//! Kotlin `LanguageAnalyzer` adapter backed by tree-sitter-kotlin-ng.
//!
//! Why: Provides static analysis for Kotlin source so the registry can
//! ingest `.kt` and `.kts` files. Mirrors the other adapters' class-qualified
//! method IDs.
//!
//! What: For each `CodeChunk` parses with tree-sitter-kotlin-ng, walks the
//! tree, and emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for top-level `function_declaration`
//! - `Method` nodes (class-qualified) for `function_declaration` inside a
//!   `class_body`
//! - `Class` nodes for `class_declaration` (when keyword is `class`) and
//!   `object_declaration`
//! - `Interface` nodes for `class_declaration` when its first token is
//!   `interface` (the kotlin-ng grammar models interfaces as a class variant)
//! - `Import` nodes + `Imports` edges for `import` headers
//! - `Calls` edges from each function/method to its callees, scoped to the
//!   enclosing function/method, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-kotlin-ng-backed analyzer.
pub struct KotlinAnalyzer;

impl KotlinAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KotlinAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for KotlinAnalyzer {
    fn language(&self) -> &str {
        "kotlin"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".kt", ".kts"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-kotlin-ng grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "kotlin analyze chunk");
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
            let file_id = format!("kotlin:File:{}", chunk.file);
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
        id: format!("kotlin:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "kotlin".into(),
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

/// First `identifier` child of `node` (used for declarations that lack a
/// `name:` field, like Kotlin's class/function declarations).
fn first_identifier_child(node: Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return Some(node_text(child, src));
        }
    }
    None
}

/// True if the first token-kind child of a `class_declaration` is `interface`.
fn is_interface_decl(node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "interface" => return true,
            "class" => return false,
            _ => {}
        }
    }
    false
}

fn make_simple_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("kotlin:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "kotlin".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// Build a method node with class-qualified ID/qualified_name.
fn make_method_node(class_name: &str, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
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
        id: format!("kotlin:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "kotlin".into(),
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
    class_name: Option<&str>,
) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name) = first_identifier_child(node, src) {
                let (id, _) = if let Some(cn) = class_name {
                    let n = make_method_node(cn, &name, chunk, node);
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
                // Body is wrapped in `function_body` → `block`. extract_calls
                // walks the whole subtree so passing the function_body works.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "function_body" {
                        for edge in extract_calls(child, src, &id) {
                            graph.edges.push(edge);
                        }
                    }
                }
            }
            return;
        }
        "class_declaration" => {
            if let Some(name) = first_identifier_child(node, src) {
                let kind = if is_interface_decl(node) {
                    KgNodeKind::Interface
                } else {
                    KgNodeKind::Class
                };
                let n = make_simple_node(kind, &name, chunk, node);
                let class_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: class_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                // Recurse into class_body.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "class_body" || child.kind() == "enum_class_body" {
                        let mut c2 = child.walk();
                        for inner in child.children(&mut c2) {
                            recurse(inner, src, chunk, graph, &class_id, Some(&name));
                        }
                    }
                }
            }
            return;
        }
        "object_declaration" => {
            if let Some(name) = first_identifier_child(node, src) {
                let n = make_simple_node(KgNodeKind::Class, &name, chunk, node);
                let class_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: class_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "class_body" {
                        let mut c2 = child.walk();
                        for inner in child.children(&mut c2) {
                            recurse(inner, src, chunk, graph, &class_id, Some(&name));
                        }
                    }
                }
            }
            return;
        }
        "import" => {
            emit_import(node, src, chunk, graph, parent_id);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        recurse(child, src, chunk, graph, parent_id, class_name);
    }
}

fn emit_import(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    // import <qualified_identifier>
    let mut target: Option<String> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "qualified_identifier" || child.kind() == "identifier" {
            target = Some(node_text(child, src));
            break;
        }
    }
    let Some(target) = target else { return };
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
            | "object_declaration"
            | "anonymous_function"
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
            to: build_call_target("kotlin", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Resolve a best-effort callee name from a Kotlin `call_expression`.
///
/// kotlin-ng has no `function:` field; the callee is typically the first
/// non-trivia child: an `identifier` for `foo()` or a `navigation_expression`
/// for `obj.foo()` / `Foo().bar()`.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let mut cursor = call.walk();
    for child in call.children(&mut cursor) {
        match child.kind() {
            "identifier" => return Some(node_text(child, src)),
            "navigation_expression" => return navigation_leaf_name(child, src),
            _ => {}
        }
    }
    None
}

/// Walk a `navigation_expression` and return the rightmost identifier — the
/// member being accessed (`a.b.c()` → `c`).
fn navigation_leaf_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let mut last: Option<String> = None;
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => last = Some(node_text(child, src)),
            "navigation_suffix" => {
                let mut c2 = child.walk();
                for inner in child.children(&mut c2) {
                    if inner.kind() == "identifier" {
                        last = Some(node_text(inner, src));
                    }
                }
            }
            _ => {}
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
    fn kotlin_supports_kt_and_kts() {
        let a = KotlinAnalyzer::new();
        assert!(a.supports("Foo.kt"));
        assert!(a.supports("build.kts"));
        assert!(!a.supports("Foo.java"));
    }

    #[test]
    fn kotlin_extracts_top_level_function() {
        let a = KotlinAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("fun top() {}\n", "f.kt")]);
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
    fn kotlin_class_method_is_qualified() {
        let a = KotlinAnalyzer::new();
        let src = "class Foo {\n  fun greet() {\n    hello()\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.kt")]);
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
    fn kotlin_class_and_interface_emit_correct_kinds() {
        let a = KotlinAnalyzer::new();
        let src = "interface I {\n  fun bar()\n}\nclass Foo : I {\n  fun bar() {}\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.kt")]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Interface) && n.name == "I"),
            "expected Interface I, nodes: {:?}",
            r.graph.nodes
        );
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Foo"),
            "expected Class Foo, nodes: {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn kotlin_call_edges_scoped_and_deduped() {
        let a = KotlinAnalyzer::new();
        let src =
            "class Foo {\n  fun greet() {\n    hello()\n    hello()\n    obj.method()\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.kt")]);
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
    fn kotlin_extracts_imports() {
        let a = KotlinAnalyzer::new();
        let src = "import kotlin.collections.List\nimport java.util.HashMap\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.kt")]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"kotlin.collections.List"), "got {names:?}");
        assert!(names.contains(&"java.util.HashMap"), "got {names:?}");
    }
}
