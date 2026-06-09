//! C# `LanguageAnalyzer` adapter backed by tree-sitter-c-sharp.
//!
//! Why: Provides static analysis for C# source so the registry can ingest
//! `.cs` files. Mirrors the Python/Ruby adapters' class-qualified method IDs.
//!
//! What: For each `CodeChunk` parses with tree-sitter-c-sharp, walks the
//! tree, and emits:
//! - one `File` node per unique `chunk.file`
//! - `Method` nodes (class-qualified) for `method_declaration` and
//!   `constructor_declaration` inside a class/interface/struct
//! - `Class` nodes for `class_declaration`, `struct_declaration`,
//!   `enum_declaration`
//! - `Interface` nodes for `interface_declaration`
//! - `Import` nodes + `Imports` edges for `using_directive`
//! - `Calls` edges from each method to its callees, scoped to the enclosing
//!   method, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module — covers detection, method/class/interface
//! extraction, qualified IDs, call scoping, dedup, `using` imports.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-c-sharp-backed analyzer.
pub struct CSharpAnalyzer;

impl CSharpAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CSharpAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for CSharpAnalyzer {
    fn language(&self) -> &str {
        "csharp"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".cs"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-c-sharp grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "csharp analyze chunk");
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
            let file_id = format!("csharp:File:{}", chunk.file);
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
        id: format!("csharp:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "csharp".into(),
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
        id: format!("csharp:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "csharp".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: name.chars().next().is_some_and(|c| c.is_uppercase()),
        extra: serde_json::Value::Null,
    }
}

/// Build a method node where the ID is `csharp:Method:file:Class:Name`.
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
        id: format!("csharp:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "csharp".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: name.chars().next().is_some_and(|c| c.is_uppercase()),
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
        "method_declaration" | "constructor_declaration" => {
            if let Some(name) = name_of(node, src) {
                let n = make_method_node(class_name.unwrap_or(""), &name, chunk, node);
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
            }
            return;
        }
        "class_declaration" | "struct_declaration" | "enum_declaration" => {
            if let Some(name) = name_of(node, src) {
                let n = make_simple_node(KgNodeKind::Class, &name, chunk, node);
                let class_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: class_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        recurse(child, src, chunk, graph, &class_id, Some(&name));
                    }
                }
            }
            return;
        }
        "interface_declaration" => {
            if let Some(name) = name_of(node, src) {
                let n = make_simple_node(KgNodeKind::Interface, &name, chunk, node);
                let iface_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: iface_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        recurse(child, src, chunk, graph, &iface_id, Some(&name));
                    }
                }
            }
            return;
        }
        "using_directive" => {
            emit_using(node, src, chunk, graph, parent_id);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        recurse(child, src, chunk, graph, parent_id, class_name);
    }
}

fn emit_using(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    // `using X = Foo;` exposes X via `name:` field; `using System.Linq;`
    // puts the namespace in a `type` / `qualified_name` / `identifier` child.
    let mut target = node
        .child_by_field_name("name")
        .map(|n| node_text(n, src))
        .unwrap_or_default();
    if target.is_empty() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "qualified_name" | "identifier" => {
                    target = node_text(child, src);
                    break;
                }
                _ => {}
            }
        }
    }
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
        // Stop at nested method/class bodies.
        match node.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "lambda_expression"
            | "anonymous_method_expression" => {
                return;
            }
            "invocation_expression" => {
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
            to: build_call_target("csharp", "Method", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Resolve a best-effort callee name from a C# `invocation_expression`.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let fun = call.child_by_field_name("function")?;
    match fun.kind() {
        "identifier" => Some(node_text(fun, src)),
        "member_access_expression" => fun.child_by_field_name("name").map(|p| node_text(p, src)),
        "generic_name" => {
            if let Some(n) = fun.child_by_field_name("name") {
                Some(node_text(n, src))
            } else {
                let mut cursor = fun.walk();
                let result = fun
                    .children(&mut cursor)
                    .find(|c| c.kind() == "identifier")
                    .map(|c| node_text(c, src));
                result
            }
        }
        _ => None,
    }
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
    fn cs_supports_dot_cs() {
        let a = CSharpAnalyzer::new();
        assert!(a.supports("Foo.cs"));
        assert!(!a.supports("foo.csv"));
    }

    #[test]
    fn cs_extracts_class_method_with_qualified_id() {
        let a = CSharpAnalyzer::new();
        let src = "class C { public void M() { Helper(); } }\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.cs")]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(methods[0].name, "M");
        assert!(
            methods[0].id.contains(":C:M"),
            "id should embed C, got {}",
            methods[0].id
        );
        assert_eq!(methods[0].qualified_name, "C.M");
    }

    #[test]
    fn cs_extracts_class_and_interface() {
        let a = CSharpAnalyzer::new();
        let src = "interface I { void X(); }\nclass C : I { public void X() {} }\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.cs")]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "C"));
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Interface) && n.name == "I"));
    }

    #[test]
    fn cs_call_edges_scoped_and_deduped() {
        let a = CSharpAnalyzer::new();
        let src = "class C {\n  public void M() {\n    Helper();\n    Helper();\n    this.Other();\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.cs")]);
        let calls: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        let helper_edges: Vec<_> = calls.iter().filter(|e| e.to.ends_with(":Helper")).collect();
        assert_eq!(
            helper_edges.len(),
            1,
            "expected one deduped Helper edge: {calls:?}"
        );
        assert!(
            (helper_edges[0].weight - 2.0).abs() < f32::EPSILON,
            "weight should be 2, got {}",
            helper_edges[0].weight
        );
        let other_edges: Vec<_> = calls.iter().filter(|e| e.to.ends_with(":Other")).collect();
        assert_eq!(other_edges.len(), 1, "expected one Other edge: {calls:?}");
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Method:") && e.from.contains(":C:M")),
            "all call edges should originate from C.M, got {calls:?}"
        );
    }

    #[test]
    fn cs_extracts_using_directives() {
        let a = CSharpAnalyzer::new();
        let src = "using System;\nusing System.Linq;\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.cs")]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"System"), "got {names:?}");
        assert!(names.contains(&"System.Linq"), "got {names:?}");
    }

    #[test]
    fn cs_struct_emits_class() {
        let a = CSharpAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("struct S {}\n", "f.cs")]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "S"));
    }
}
