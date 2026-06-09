//! C++ `LanguageAnalyzer` adapter backed by tree-sitter-cpp.
//!
//! Why: Provides static analysis for C++ source so the registry can ingest
//! `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` files. C++ adds classes and
//! methods on top of C; we mirror Python/Ruby's class-qualified method IDs so
//! methods on different classes don't collide.
//!
//! What: For each `CodeChunk` parses with tree-sitter-cpp, walks the tree,
//! and emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for top-level `function_definition`
//! - `Method` nodes (class-qualified) for `function_definition` inside a
//!   `class_specifier` body
//! - `Class` nodes for `class_specifier`, `struct_specifier` (with body),
//!   and `enum_specifier`
//! - `Import` nodes + `Imports` edges for `preproc_include`
//! - `Calls` edges from each function/method to its callees, scoped to the
//!   enclosing function/method, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module — covers detection, function/method/class
//! extraction, qualified method IDs, call scoping, dedup, `#include` imports.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-cpp-backed analyzer.
pub struct CppAnalyzer;

impl CppAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CppAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for CppAnalyzer {
    fn language(&self) -> &str {
        "cpp"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-cpp grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "cpp analyze chunk");
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
            let file_id = format!("cpp:File:{}", chunk.file);
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
        id: format!("cpp:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "cpp".into(),
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

fn make_simple_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("cpp:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "cpp".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// Build a method node where the ID is `cpp:Method:file:Class:Name`.
fn make_method_node(class_name: &str, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let qualified = if class_name.is_empty() {
        name.to_string()
    } else {
        format!("{class_name}::{name}")
    };
    let id_suffix = if class_name.is_empty() {
        name.to_string()
    } else {
        format!("{class_name}:{name}")
    };
    KgNode {
        id: format!("cpp:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "cpp".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// Pull the function name out of a `function_definition` node by walking the
/// `function_declarator` chain to the leaf identifier.
fn function_name(def: Node, src: &[u8]) -> Option<String> {
    let declarator = def.child_by_field_name("declarator")?;
    extract_declarator_name(declarator, src)
}

fn extract_declarator_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" | "operator_name"
        | "destructor_name" => Some(node_text(node, src)),
        "qualified_identifier" => {
            // Leaf name is the rightmost identifier-like child.
            let mut cursor = node.walk();
            let mut last_name: Option<String> = None;
            for child in node.children(&mut cursor) {
                if let Some(n) = extract_declarator_name(child, src) {
                    last_name = Some(n);
                }
            }
            last_name
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(n) = extract_declarator_name(child, src) {
                    return Some(n);
                }
            }
            None
        }
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
        "function_definition" => {
            if let Some(name) = function_name(node, src) {
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
                if let Some(body) = node.child_by_field_name("body") {
                    for edge in extract_calls(body, src, &id) {
                        graph.edges.push(edge);
                    }
                }
            }
            return;
        }
        "class_specifier" | "struct_specifier" => {
            let has_body = node.child_by_field_name("body").is_some();
            if has_body {
                if let Some(name_node) = node.child_by_field_name("name") {
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
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut cursor = body.walk();
                        for child in body.children(&mut cursor) {
                            recurse(child, src, chunk, graph, &class_id, Some(&name));
                        }
                    }
                    return;
                }
            }
        }
        "enum_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
                let n = make_simple_node(KgNodeKind::Class, &name, chunk, node);
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
        "preproc_include" => {
            emit_include(node, src, chunk, graph, parent_id);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        recurse(child, src, chunk, graph, parent_id, class_name);
    }
}

fn emit_include(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let target = match child.kind() {
            "system_lib_string" => node_text(child, src)
                .trim_matches(|c| c == '<' || c == '>')
                .to_string(),
            "string_literal" => {
                let mut content: Option<String> = None;
                let mut c2 = child.walk();
                for inner in child.children(&mut c2) {
                    if inner.kind() == "string_content" {
                        content = Some(node_text(inner, src));
                        break;
                    }
                }
                content.unwrap_or_else(|| {
                    node_text(child, src)
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string()
                })
            }
            _ => continue,
        };
        if target.is_empty() {
            continue;
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
        break;
    }
}

fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Stop at nested function-like / class bodies.
        match node.kind() {
            "function_definition" | "class_specifier" | "lambda_expression" => {
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
            to: build_call_target("cpp", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Resolve a best-effort callee name from a C++ `call_expression`.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let fun = call.child_by_field_name("function")?;
    leaf_callee_name(fun, src)
}

fn leaf_callee_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(node, src)),
        "field_expression" => node.child_by_field_name("field").map(|p| node_text(p, src)),
        "qualified_identifier" => {
            // ns::other → leaf is `other`. Walk children, take last identifier.
            let mut cursor = node.walk();
            let mut last: Option<String> = None;
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "identifier" | "field_identifier" => last = Some(node_text(child, src)),
                    "qualified_identifier" => {
                        if let Some(n) = leaf_callee_name(child, src) {
                            last = Some(n);
                        }
                    }
                    _ => {}
                }
            }
            last
        }
        "template_function" => node
            .child_by_field_name("name")
            .and_then(|n| leaf_callee_name(n, src)),
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
    fn cpp_supports_extensions() {
        let a = CppAnalyzer::new();
        assert!(a.supports("foo.cpp"));
        assert!(a.supports("foo.cc"));
        assert!(a.supports("foo.cxx"));
        assert!(a.supports("foo.hpp"));
        assert!(!a.supports("foo.c"));
    }

    #[test]
    fn cpp_extracts_top_level_function() {
        let a = CppAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("int top() { return 0; }\n", "f.cpp")]);
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
    fn cpp_class_method_is_qualified() {
        let a = CppAnalyzer::new();
        let src = "class Foo { public: void bar() { helper(); } };\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.cpp")]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(methods[0].name, "bar");
        assert!(
            methods[0].id.contains(":Foo:bar"),
            "id should embed Foo, got {}",
            methods[0].id
        );
        assert_eq!(methods[0].qualified_name, "Foo::bar");
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Foo"));
    }

    #[test]
    fn cpp_call_edges_scoped_and_deduped() {
        let a = CppAnalyzer::new();
        let src = "class Foo { public: void bar() { helper(); helper(); ns::other(); } };\nvoid helper() {}\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.cpp")]);
        let calls: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        let helper_edges: Vec<_> = calls.iter().filter(|e| e.to.ends_with(":helper")).collect();
        assert_eq!(
            helper_edges.len(),
            1,
            "expected one deduped helper edge: {calls:?}"
        );
        assert!(
            (helper_edges[0].weight - 2.0).abs() < f32::EPSILON,
            "weight should be 2, got {}",
            helper_edges[0].weight
        );
        let other_edges: Vec<_> = calls.iter().filter(|e| e.to.ends_with(":other")).collect();
        assert_eq!(other_edges.len(), 1, "expected one other edge: {calls:?}");
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Method:") && e.from.contains(":Foo:bar")),
            "all call edges should originate from Foo::bar, got {calls:?}"
        );
    }

    #[test]
    fn cpp_extracts_includes() {
        let a = CppAnalyzer::new();
        let src = "#include <string>\n#include \"local.h\"\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.cpp")]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"string"), "got {names:?}");
        assert!(names.contains(&"local.h"), "got {names:?}");
    }

    #[test]
    fn cpp_struct_emits_class() {
        let a = CppAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("struct S { int x; };\n", "f.cpp")]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "S"));
    }
}
