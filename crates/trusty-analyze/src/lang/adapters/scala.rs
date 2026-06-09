//! Scala `LanguageAnalyzer` adapter backed by tree-sitter-scala.
//!
//! Why: Provides static analysis for Scala source so the registry can ingest
//! `.scala` files. Mirrors the Kotlin/Java adapters' class-qualified method
//! IDs so cross-language queries return consistent shapes for JVM languages.
//!
//! What: For each `CodeChunk` parses with tree-sitter-scala, walks the tree,
//! and emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for top-level `function_definition`
//! - `Method` nodes (class-qualified) for `function_definition` /
//!   `function_declaration` inside a `template_body`
//! - `Class` nodes for `class_definition` (including `case class`) and
//!   `object_definition`
//! - `Interface` nodes for `trait_definition`
//! - `Import` nodes + `Imports` edges for `import_declaration` (wildcard
//!   imports are emitted as `pkg.*`)
//! - `Calls` edges from each function/method to its callees, scoped to the
//!   enclosing function/method, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-scala-backed analyzer.
pub struct ScalaAnalyzer;

impl ScalaAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScalaAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for ScalaAnalyzer {
    fn language(&self) -> &str {
        "scala"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".scala"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_scala::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-scala grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "scala analyze chunk");
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
            let file_id = format!("scala:File:{}", chunk.file);
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
        id: format!("scala:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "scala".into(),
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
/// `name:` field, like Scala's class/object/trait/function definitions).
fn first_identifier_child(node: Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return Some(node_text(child, src));
        }
    }
    None
}

fn make_simple_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("scala:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "scala".into(),
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
        id: format!("scala:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "scala".into(),
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
        "function_definition" | "function_declaration" => {
            if let Some(name) = first_identifier_child(node, src) {
                let id = if let Some(cn) = class_name {
                    let n = make_method_node(cn, &name, chunk, node);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    id
                } else {
                    let n = make_simple_node(KgNodeKind::Function, &name, chunk, node);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    id
                };
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                // Body can be a `block` (def f() = { ... }) or any expression
                // (def f() = expr). Scan all children — extract_calls stops at
                // nested function/class boundaries so this is safe.
                for edge in extract_calls(node, src, &id) {
                    graph.edges.push(edge);
                }
            }
            return;
        }
        "class_definition" | "object_definition" => {
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
                // Recurse into template_body (class/object body).
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "template_body" {
                        let mut c2 = child.walk();
                        for inner in child.children(&mut c2) {
                            recurse(inner, src, chunk, graph, &class_id, Some(&name));
                        }
                    }
                }
            }
            return;
        }
        "trait_definition" => {
            if let Some(name) = first_identifier_child(node, src) {
                let n = make_simple_node(KgNodeKind::Interface, &name, chunk, node);
                let trait_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: trait_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "template_body" {
                        let mut c2 = child.walk();
                        for inner in child.children(&mut c2) {
                            recurse(inner, src, chunk, graph, &trait_id, Some(&name));
                        }
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
        recurse(child, src, chunk, graph, parent_id, class_name);
    }
}

/// Build the import target string from an `import_declaration`'s children.
///
/// Scala imports look like `import scala.collection.mutable.ListBuffer` or
/// `import foo.bar._` — the grammar emits them as a sequence of `identifier`
/// children separated by `.` tokens, optionally followed by a
/// `namespace_wildcard` (the `_`). We join the identifiers with `.` and
/// suffix `.*` for wildcard imports so callers see `foo.bar.*`.
fn emit_import(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    let mut parts: Vec<String> = Vec::new();
    let mut wildcard = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => parts.push(node_text(child, src)),
            "namespace_wildcard" => wildcard = true,
            _ => {}
        }
    }
    if parts.is_empty() {
        return;
    }
    let mut target = parts.join(".");
    if wildcard {
        target.push_str(".*");
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

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>, depth: usize) {
        // Stop at nested function-like / class bodies so the caller scope
        // doesn't leak inner-call counts. Don't stop at depth 0 — the caller
        // passes the function_definition itself as the entry point.
        if depth > 0 {
            match node.kind() {
                "function_definition"
                | "function_declaration"
                | "class_definition"
                | "object_definition"
                | "trait_definition" => {
                    return;
                }
                _ => {}
            }
        }
        if node.kind() == "call_expression" {
            if let Some(callee) = callee_name(node, src) {
                *counts.entry(callee).or_insert(0) += 1;
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, src, counts, depth + 1);
        }
    }

    visit(body, src, &mut counts, 0);

    counts
        .into_iter()
        .map(|(callee, count)| KgEdge {
            from: caller_id.to_string(),
            to: build_call_target("scala", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Resolve a best-effort callee name from a Scala `call_expression`.
///
/// scala has no `function:` field; callees look like:
/// - `identifier` for `foo()`
/// - `field_expression` for `obj.foo()` / `Foo.bar()` (rightmost identifier)
/// - `generic_function` for `foo[T](args)` (function position is an identifier
///   or field_expression)
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let mut cursor = call.walk();
    for child in call.children(&mut cursor) {
        match child.kind() {
            "identifier" => return Some(node_text(child, src)),
            "field_expression" => return field_leaf_name(child, src),
            "generic_function" => return generic_function_name(child, src),
            _ => {}
        }
    }
    None
}

/// Walk a `field_expression` and return the rightmost identifier — the member
/// being accessed (`a.b.c` → `c`).
fn field_leaf_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let mut last: Option<String> = None;
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => last = Some(node_text(child, src)),
            "field_expression" => {
                if let Some(inner) = field_leaf_name(child, src) {
                    last = Some(inner);
                }
            }
            _ => {}
        }
    }
    last
}

/// `generic_function` wraps the callable for parameterized calls: pull the
/// function position out and resolve recursively.
fn generic_function_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => return Some(node_text(child, src)),
            "field_expression" => return field_leaf_name(child, src),
            _ => {}
        }
    }
    None
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
    fn scala_supports_scala_files() {
        let a = ScalaAnalyzer::new();
        assert!(a.supports("Foo.scala"));
        assert!(!a.supports("Foo.kt"));
        assert!(!a.supports("Foo.java"));
    }

    #[test]
    fn scala_extracts_top_level_function() {
        let a = ScalaAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("def top(): Int = 42\n", "f.scala")]);
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
    fn scala_class_method_is_qualified() {
        let a = ScalaAnalyzer::new();
        let src = "class Foo {\n  def greet(): Unit = {\n    hello()\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.scala")]);
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
    fn scala_class_definition_emits_class() {
        let a = ScalaAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("class Foo {}\n", "f.scala")]);
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
    fn scala_trait_definition_emits_interface() {
        let a = ScalaAnalyzer::new();
        let src = "trait MyTrait {\n  def doit(): Unit\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.scala")]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Interface) && n.name == "MyTrait"),
            "expected Interface MyTrait, nodes: {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn scala_object_definition_emits_class() {
        let a = ScalaAnalyzer::new();
        let src = "object MyObject {\n  def util(): Int = 42\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.scala")]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "MyObject"),
            "expected Class MyObject for object, nodes: {:?}",
            r.graph.nodes
        );
        // Method inside object should be class-qualified.
        let m = r
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, KgNodeKind::Method) && n.name == "util")
            .expect("util method");
        assert_eq!(m.qualified_name, "MyObject.util");
    }

    #[test]
    fn scala_case_class_emits_class() {
        let a = ScalaAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("case class Point(x: Int, y: Int)\n", "f.scala")]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Point"),
            "expected Class Point for case class, nodes: {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn scala_call_edges_scoped_and_deduped() {
        let a = ScalaAnalyzer::new();
        let src = "class Foo {\n  def greet(): Unit = {\n    hello()\n    hello()\n    hello()\n    obj.method()\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.scala")]);
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
            (hello_edges[0].weight - 3.0).abs() < f32::EPSILON,
            "weight should be 3, got {}",
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
    fn scala_extracts_imports_with_wildcard() {
        let a = ScalaAnalyzer::new();
        let src = "import scala.collection.mutable.ListBuffer\nimport foo.bar._\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.scala")]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"scala.collection.mutable.ListBuffer"),
            "got {names:?}"
        );
        assert!(names.contains(&"foo.bar.*"), "got {names:?}");
    }
}
