//! Java `LanguageAnalyzer` adapter backed by tree-sitter-java.
//!
//! Why: Extracts Java structure — classes, interfaces, methods, imports,
//! superclass/superinterface relationships — into a language-neutral
//! `KgGraph`. Mirrors the Rust/TypeScript/Python adapters.
//!
//! What: For each `CodeChunk`, parses the content with tree-sitter-java,
//! walks the tree, and emits:
//! - `Class` for `class_declaration` and `enum_declaration`
//! - `Interface` for `interface_declaration`
//! - `Method` for `method_declaration` inside a class/interface
//! - `Method` for `constructor_declaration` (constructors)
//! - `Field` for `field_declaration` variable declarators
//! - `Import` + `Imports` edges for `import_declaration`
//! - `TestCase` for methods annotated `@Test`
//! - `Extends` edges for `superclass` clauses
//! - `Implements` edges for `super_interfaces` clauses
//! - `Calls` edges from each method/constructor to its direct callees,
//!   deduplicated with `weight = call_count`
//! - `Contains` edges from the file to top-level types and from types to
//!   their members
//!
//! Test: `java_extracts_class_and_method` covers a minimal class with one
//! method.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-java-backed analyzer.
pub struct JavaAnalyzer;

impl JavaAnalyzer {
    /// Construct a stateless analyzer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for JavaAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for JavaAnalyzer {
    fn language(&self) -> &str {
        "java"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".java"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-java grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "java analyze chunk");
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
            walk(tree.root_node(), src, chunk, &mut result.graph);
        }

        result
    }
}

fn file_node(file: &str) -> KgNode {
    KgNode {
        id: format!("java:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "java".into(),
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

fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            return node_text(child, src).contains("public");
        }
    }
    false
}

/// True if any modifier annotation on `node` is `@Test`.
fn has_test_annotation(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut c2 = child.walk();
            for m in child.children(&mut c2) {
                if m.kind() == "annotation" || m.kind() == "marker_annotation" {
                    let txt = node_text(m, src);
                    if txt.starts_with("@Test") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Find the immediately preceding block_comment if it's a Javadoc `/** ... */`.
fn javadoc(node: Node, src: &[u8]) -> Option<String> {
    let prev = node.prev_sibling()?;
    if prev.kind() == "block_comment" {
        let txt = node_text(prev, src);
        if txt.starts_with("/**") {
            return Some(txt);
        }
    }
    None
}

fn make_node(
    kind: KgNodeKind,
    name: &str,
    chunk: &CodeChunk,
    ast: Node,
    is_pub: bool,
    doc: Option<String>,
) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("java:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "java".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: doc,
        is_public: is_pub,
        extra: serde_json::Value::Null,
    }
}

fn walk(root: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph) {
    let file_id = format!("java:File:{}", chunk.file);

    fn recurse(
        node: Node,
        src: &[u8],
        chunk: &CodeChunk,
        graph: &mut KgGraph,
        parent_id: &str,
        inside_type: bool,
    ) {
        match node.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                let is_iface = node.kind() == "interface_declaration";
                let Some(name) = name_of(node, src) else {
                    return;
                };
                let pub_ = is_public(node, src);
                let doc = javadoc(node, src);
                let class_kind = if is_iface {
                    KgNodeKind::Interface
                } else {
                    KgNodeKind::Class
                };
                let n = make_node(class_kind.clone(), &name, chunk, node, pub_, doc);
                let class_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: class_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });

                // superclass → Extends edge
                if let Some(sup) = node.child_by_field_name("superclass") {
                    // sup is a `superclass` node wrapping a type identifier
                    let mut c = sup.walk();
                    for ch in sup.children(&mut c) {
                        if ch.kind() == "type_identifier" {
                            let target = node_text(ch, src);
                            let to_id = format!("java:Class:{}:{target}", chunk.file);
                            graph.edges.push(KgEdge {
                                from: class_id.clone(),
                                to: to_id,
                                kind: KgEdgeKind::Extends,
                                weight: 1.0,
                            });
                        }
                    }
                }
                // super_interfaces → Implements edges
                if let Some(supi) = node.child_by_field_name("interfaces") {
                    add_super_interface_edges(supi, src, chunk, &class_id, graph);
                } else {
                    // tree-sitter-java sometimes attaches it as a sibling child
                    let mut c = node.walk();
                    for ch in node.children(&mut c) {
                        if ch.kind() == "super_interfaces" {
                            add_super_interface_edges(ch, src, chunk, &class_id, graph);
                        }
                    }
                }

                // Recurse into body
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        recurse(child, src, chunk, graph, &class_id, true);
                    }
                }
                return;
            }
            "method_declaration" | "constructor_declaration" => {
                let Some(name) = name_of(node, src) else {
                    return;
                };
                let pub_ = is_public(node, src);
                let doc = javadoc(node, src);
                let is_test = has_test_annotation(node, src);
                let kind = if is_test {
                    KgNodeKind::TestCase
                } else if inside_type {
                    KgNodeKind::Method
                } else {
                    KgNodeKind::Function
                };
                let n = make_node(kind, &name, chunk, node, pub_, doc);
                let id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    for edge in extract_calls(body, src, &id) {
                        graph.edges.push(edge);
                    }
                }
                return;
            }
            "field_declaration" => {
                let pub_ = is_public(node, src);
                let doc = javadoc(node, src);
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() != "variable_declarator" {
                        continue;
                    }
                    let Some(name_node) = child.child_by_field_name("name") else {
                        continue;
                    };
                    let name = node_text(name_node, src);
                    let n = make_node(KgNodeKind::Field, &name, chunk, child, pub_, doc.clone());
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: parent_id.to_string(),
                        to: id,
                        kind: KgEdgeKind::Contains,
                        weight: 1.0,
                    });
                }
                return;
            }
            "import_declaration" => {
                let txt = node_text(node, src).trim().to_string();
                if !txt.is_empty() {
                    let n = make_node(KgNodeKind::Import, &txt, chunk, node, false, None);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: parent_id.to_string(),
                        to: id,
                        kind: KgEdgeKind::Imports,
                        weight: 1.0,
                    });
                }
                return;
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            recurse(child, src, chunk, graph, parent_id, inside_type);
        }
    }

    recurse(root, src, chunk, graph, &file_id, false);
}

/// Extract `method_invocation` and `object_creation_expression` nodes from a
/// method/constructor body and produce deduplicated `Calls` edges keyed by
/// callee name.
///
/// Why: A function's outgoing call graph is one of the most useful pieces of
/// static analysis we can derive cheaply and is required for graph traversal
/// queries ("what calls auth?"). Without scoped extraction, every call site is
/// emitted as an orphan edge with no caller, defeating the purpose.
///
/// What: Walks the AST subtree rooted at `body`, collects every direct
/// invocation (skipping nested method/constructor/lambda/anonymous-class
/// bodies so each method only emits its own direct calls), resolves the
/// callee name, counts repeats, and returns one `KgEdge` per unique callee
/// with `weight = call_count as f32`.
///
/// Test: `java_adapter_extracts_call_edges` and
/// `java_adapter_deduplicates_repeated_calls` cover the happy paths.
fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Stop at nested function-like / class bodies so each method only
        // attributes its own direct calls.
        match node.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "lambda_expression"
            | "class_declaration"
            | "interface_declaration"
            | "enum_declaration" => {
                return;
            }
            "method_invocation" => {
                if let Some(callee) = method_invocation_name(node, src) {
                    *counts.entry(callee).or_insert(0) += 1;
                }
            }
            "object_creation_expression" => {
                if let Some(callee) = object_creation_name(node, src) {
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
            to: build_call_target("java", "Method", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Resolve the callee name from a Java `method_invocation` node.
///
/// Why: Cross-file resolution is out of scope for the adapter; we only need a
/// stable string handle for the callee.
///
/// What: Returns the text of the `name` field (an identifier) for both bare
/// `foo()` and qualified `obj.foo()` invocations. Returns `None` if missing.
///
/// Test: Exercised indirectly by the `extract_calls` tests.
fn method_invocation_name(call: Node, src: &[u8]) -> Option<String> {
    call.child_by_field_name("name").map(|n| node_text(n, src))
}

/// Resolve the type name from a Java `object_creation_expression` (`new Foo()`).
///
/// Why: Constructor invocations are part of a method's call graph.
///
/// What: Walks children and returns the first `type_identifier`'s text.
///
/// Test: Exercised by `java_adapter_extracts_call_edges`.
fn object_creation_name(call: Node, src: &[u8]) -> Option<String> {
    if let Some(t) = call.child_by_field_name("type") {
        if t.kind() == "type_identifier" {
            return Some(node_text(t, src));
        }
        // scoped/parameterized types: descend to find a type_identifier
        let mut cursor = t.walk();
        for child in t.children(&mut cursor) {
            if child.kind() == "type_identifier" {
                return Some(node_text(child, src));
            }
        }
    }
    let mut cursor = call.walk();
    for child in call.children(&mut cursor) {
        if child.kind() == "type_identifier" {
            return Some(node_text(child, src));
        }
    }
    None
}

fn add_super_interface_edges(
    super_node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    from_id: &str,
    graph: &mut KgGraph,
) {
    let mut stack = vec![super_node];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            let target = node_text(n, src);
            let to_id = format!("java:Interface:{}:{target}", chunk.file);
            graph.edges.push(KgEdge {
                from: from_id.to_string(),
                to: to_id,
                kind: KgEdgeKind::Implements,
                weight: 1.0,
            });
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str) -> CodeChunk {
        CodeChunk {
            id: "Foo.java:1:20".into(),
            file: "Foo.java".into(),
            start_line: 1,
            end_line: 20,
            content: content.into(),
            function_name: None,
            score: 0.0,
            compact_snippet: None,
            match_reason: String::new(),
        }
    }

    #[test]
    fn java_supports_java_files() {
        let a = JavaAnalyzer::new();
        assert!(a.supports("Foo.java"));
        assert!(!a.supports("foo.go"));
    }

    #[test]
    fn java_extracts_class_and_method() {
        let a = JavaAnalyzer::new();
        let c = make_chunk("public class Foo {\n    public void bar() {}\n}\n");
        let r = a.analyze_chunks(&[c]);
        let kinds: Vec<&KgNodeKind> = r.graph.nodes.iter().map(|n| &n.kind).collect();
        assert!(
            kinds.iter().any(|k| matches!(k, KgNodeKind::Class)),
            "expected Class, got {:?}",
            kinds
        );
        assert!(
            kinds.iter().any(|k| matches!(k, KgNodeKind::Method)),
            "expected Method, got {:?}",
            kinds
        );
        let class = r
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, KgNodeKind::Class))
            .unwrap();
        assert!(class.is_public);
    }

    #[test]
    fn java_extracts_interface() {
        let a = JavaAnalyzer::new();
        let c = make_chunk("public interface Bar { void baz(); }\n");
        let r = a.analyze_chunks(&[c]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Interface)));
    }

    #[test]
    fn java_test_method_detected() {
        let a = JavaAnalyzer::new();
        let c = make_chunk("class FooTest {\n    @Test\n    public void shouldWork() {}\n}\n");
        let r = a.analyze_chunks(&[c]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::TestCase)),
            "graph: {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn java_extracts_imports() {
        let a = JavaAnalyzer::new();
        let c = make_chunk("import java.util.List;\nclass A {}\n");
        let r = a.analyze_chunks(&[c]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Import)));
    }

    #[test]
    fn java_extracts_two_methods() {
        let a = JavaAnalyzer::new();
        let c = make_chunk(
            "public class Foo {\n    public void bar() {}\n    public int baz() { return 1; }\n}\n",
        );
        let r = a.analyze_chunks(&[c]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 2, "expected 2 Method nodes, got {methods:?}");
        let names: Vec<&str> = methods.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"baz"));
    }

    #[test]
    fn java_extracts_field() {
        let a = JavaAnalyzer::new();
        let c =
            make_chunk("public class Foo {\n    private int count;\n    public String name;\n}\n");
        let r = a.analyze_chunks(&[c]);
        let fields: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Field))
            .collect();
        assert_eq!(fields.len(), 2, "expected 2 Field nodes, got {fields:?}");
    }

    #[test]
    fn java_extracts_constructor() {
        let a = JavaAnalyzer::new();
        let c = make_chunk("public class Foo {\n    public Foo() {}\n}\n");
        let r = a.analyze_chunks(&[c]);
        // Constructors are emitted as Method nodes with the class name.
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Method) && n.name == "Foo"),
            "expected constructor Method node 'Foo', got {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn java_extracts_enum() {
        let a = JavaAnalyzer::new();
        let c = make_chunk("public enum Color { RED, GREEN, BLUE }\n");
        let r = a.analyze_chunks(&[c]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Color"),
            "expected enum to be emitted as Class node, got {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn java_adapter_extracts_call_edges() {
        let a = JavaAnalyzer::new();
        let c = make_chunk(
            "public class Foo {\n    public void caller() {\n        helper();\n        new Helper();\n    }\n    public void helper() {}\n}\n",
        );
        let r = a.analyze_chunks(&[c]);
        let calls: Vec<_> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        assert!(
            !calls.is_empty(),
            "expected at least one Calls edge, got none. graph={:?}",
            r.graph
        );
        let has_helper = calls.iter().any(|e| e.to.contains("helper"));
        let has_new = calls.iter().any(|e| e.to.contains("Helper"));
        assert!(has_helper, "expected edge to 'helper', got {calls:?}");
        assert!(has_new, "expected edge to 'Helper' (new), got {calls:?}");
        // Caller must be scoped to the method, not the file.
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Method:") || e.from.contains(":TestCase:")),
            "Calls edges should originate from a method node, got {calls:?}"
        );
    }

    #[test]
    fn java_adapter_deduplicates_repeated_calls() {
        let a = JavaAnalyzer::new();
        let c = make_chunk(
            "public class Foo {\n    public void caller() {\n        bar();\n        bar();\n        bar();\n    }\n    public void bar() {}\n}\n",
        );
        let r = a.analyze_chunks(&[c]);
        let bar_edges: Vec<_> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls) && e.to.contains("bar"))
            .collect();
        assert_eq!(
            bar_edges.len(),
            1,
            "repeated calls should be deduplicated, got {bar_edges:?}"
        );
        assert!(
            (bar_edges[0].weight - 3.0).abs() < f32::EPSILON,
            "weight should reflect call count=3, got {}",
            bar_edges[0].weight
        );
    }

    #[test]
    fn java_implements_edge_emitted() {
        let a = JavaAnalyzer::new();
        let c = make_chunk(
            "class Foo implements Runnable, AutoCloseable {\n    public void run() {}\n    public void close() {}\n}\n",
        );
        let r = a.analyze_chunks(&[c]);
        let impls = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Implements))
            .count();
        assert!(impls >= 2, "expected >= 2 Implements edges, got {impls}");
    }
}
