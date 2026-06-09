//! C `LanguageAnalyzer` adapter backed by tree-sitter-c.
//!
//! Why: Provides static analysis for C source so the registry can ingest
//! `.c` and `.h` files. C has no class system; all functions are top-level.
//!
//! What: For each `CodeChunk` parses with tree-sitter-c, walks the tree, and
//! emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for `function_definition`
//! - `Class` nodes for `struct_specifier` (with body), `enum_specifier`, and
//!   `type_definition` (typedef'd struct/enum); the `Class` kind is the
//!   closest semantic match for an aggregate type in our schema
//! - `Import` nodes + `Imports` edges for `preproc_include`
//! - `Calls` edges from each function to its callees, scoped to the
//!   enclosing function, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module — covers detection, function/struct/enum
//! extraction, call scoping, dedup, and `#include` imports.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-c-backed analyzer.
pub struct CAnalyzer;

impl CAnalyzer {
    /// Construct a stateless analyzer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for CAnalyzer {
    fn language(&self) -> &str {
        "c"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".c", ".h"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-c grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "c analyze chunk");
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
            let file_id = format!("c:File:{}", chunk.file);
            recurse(tree.root_node(), src, chunk, &mut result.graph, &file_id);
        }

        result
    }
}

fn file_node(file: &str) -> KgNode {
    KgNode {
        id: format!("c:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "c".into(),
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

fn make_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("c:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "c".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// Pull the function name out of a `function_definition` node by walking the
/// `function_declarator` chain to the leaf `identifier` / `field_identifier`.
fn function_name(def: Node, src: &[u8]) -> Option<String> {
    let declarator = def.child_by_field_name("declarator")?;
    extract_declarator_name(declarator, src)
}

fn extract_declarator_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(node_text(node, src)),
        _ => {
            // Recurse through pointer_declarator / function_declarator / etc.
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

fn recurse(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    match node.kind() {
        "function_definition" => {
            if let Some(name) = function_name(node, src) {
                let n = make_node(KgNodeKind::Function, &name, chunk, node);
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
        "struct_specifier" => {
            // Only emit a Class node when the struct has a body (definition,
            // not a forward decl or use site).
            let has_body = node.child_by_field_name("body").is_some();
            if has_body {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, src);
                    let n = make_node(KgNodeKind::Class, &name, chunk, node);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: parent_id.to_string(),
                        to: id,
                        kind: KgEdgeKind::Contains,
                        weight: 1.0,
                    });
                }
            }
            // Don't return — typedef wraps these; sibling handling needed.
        }
        "enum_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
                let n = make_node(KgNodeKind::Class, &name, chunk, node);
                let id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: id,
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
            }
        }
        "type_definition" => {
            // typedef struct { ... } Bar; — emit Class using typedef name
            // (last type_identifier child).
            let mut cursor = node.walk();
            let mut typedef_name: Option<String> = None;
            for child in node.children(&mut cursor) {
                if child.kind() == "type_identifier" {
                    typedef_name = Some(node_text(child, src));
                }
            }
            if let Some(name) = typedef_name {
                let n = make_node(KgNodeKind::Class, &name, chunk, node);
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
        recurse(child, src, chunk, graph, parent_id);
    }
}

fn emit_include(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let target = match child.kind() {
            "system_lib_string" => {
                // <stdio.h> — strip angle brackets
                let raw = node_text(child, src);
                raw.trim_matches(|c| c == '<' || c == '>').to_string()
            }
            "string_literal" => {
                // "foo.h" — find string_content child or strip quotes
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
        let n = make_node(KgNodeKind::Import, &target, chunk, node);
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

/// Extract `call_expression` nodes from a function body and produce
/// deduplicated `Calls` edges keyed by callee name.
fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Stop at nested function bodies (rare in C but allowed by GCC ext).
        if node.kind() == "function_definition" {
            return;
        }
        if node.kind() == "call_expression" {
            if let Some(callee) = callee_name(node, src) {
                *counts.entry(callee).or_insert(0) += 1;
            }
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
            to: build_call_target("c", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Resolve a best-effort callee name from a C `call_expression`.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let fun = call.child_by_field_name("function")?;
    match fun.kind() {
        "identifier" => Some(node_text(fun, src)),
        "field_expression" => fun.child_by_field_name("field").map(|p| node_text(p, src)),
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
    fn c_supports_c_and_h() {
        let a = CAnalyzer::new();
        assert!(a.supports("foo.c"));
        assert!(a.supports("bar.h"));
        assert!(!a.supports("foo.cpp"));
    }

    #[test]
    fn c_extracts_function() {
        let a = CAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk(
            "int add(int a, int b) { return a + b; }\n",
            "f.c",
        )]);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(funcs[0].name, "add");
        assert_eq!(funcs[0].language, "c");
    }

    #[test]
    fn c_extracts_struct_as_class() {
        let a = CAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("struct Foo { int x; };\n", "f.c")]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Foo"));
    }

    #[test]
    fn c_extracts_enum_as_class() {
        let a = CAnalyzer::new();
        let r = a.analyze_chunks(&[make_chunk("enum Color { RED, GREEN, BLUE };\n", "f.c")]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Color"));
    }

    #[test]
    fn c_call_edges_scoped_and_deduped() {
        let a = CAnalyzer::new();
        let src = "int caller(void) {\n    helper();\n    helper();\n    helper();\n    obj.field();\n    return 0;\n}\nint helper(void) { return 0; }\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.c")]);
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
            (helper_edges[0].weight - 3.0).abs() < f32::EPSILON,
            "weight should be 3, got {}",
            helper_edges[0].weight
        );
        let field_edges: Vec<_> = calls.iter().filter(|e| e.to.ends_with(":field")).collect();
        assert_eq!(field_edges.len(), 1, "expected field edge: {calls:?}");
        assert!(
            calls.iter().all(|e| e.from.contains(":Function:")),
            "all call edges should originate from a Function node, got {calls:?}"
        );
    }

    #[test]
    fn c_extracts_includes() {
        let a = CAnalyzer::new();
        let src = "#include <stdio.h>\n#include \"local.h\"\n";
        let r = a.analyze_chunks(&[make_chunk(src, "f.c")]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"stdio.h"),
            "expected 'stdio.h' import, got {names:?}"
        );
        assert!(
            names.contains(&"local.h"),
            "expected 'local.h' import, got {names:?}"
        );
    }
}
