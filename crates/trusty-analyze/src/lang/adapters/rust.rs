//! Rust `LanguageAnalyzer` adapter backed by tree-sitter-rust.
//!
//! Why: Extracts the structural backbone of Rust source — fns, methods,
//! structs/enums, traits, modules, imports, tests — into a language-neutral
//! `KgGraph`. The walker is deliberately conservative: it only emits nodes
//! and edges we can derive directly from the AST without any name
//! resolution.
//!
//! What: For each `CodeChunk`, parse `chunk.content`, walk the syntax tree
//! once, and emit:
//! - one `File` node per unique chunk.file
//! - `Function` / `Method` nodes for `function_item`
//! - `Class` nodes for `struct_item` / `enum_item` / `union_item`
//! - `Interface` nodes for `trait_item`
//! - `Module` nodes for `mod_item`
//! - `Import` nodes + `Imports` edges for `use_declaration`
//! - `TestCase` for `#[test]` fns
//! - `Implements` edges for `impl Trait for Type`
//! - `Contains` edges from file to top-level items
//!
//! Test: `rust_analyzer_extracts_function` parses a minimal `fn hello() {}`
//! chunk and asserts a Function node is produced.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-rust-backed analyzer.
pub struct RustAnalyzer;

impl RustAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RustAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for RustAnalyzer {
    fn language(&self) -> &str {
        "rust"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".rs"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-rust grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            let tree = match parser.parse(&chunk.content, None) {
                Some(t) => t,
                None => {
                    result.errors.push(format!("parse failure: {}", chunk.file));
                    continue;
                }
            };
            result.analyzed_chunks += 1;
            if seen_files.insert(chunk.file.clone()) {
                result.analyzed_files += 1;
                let file_node = file_node(&chunk.file, "rust");
                result.graph.nodes.push(file_node);
            }

            let src = chunk.content.as_bytes();
            walk_rust(tree.root_node(), src, chunk, &mut result.graph);
        }

        result
    }
}

fn file_node(file: &str, language: &str) -> KgNode {
    KgNode {
        id: format!("{language}:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: language.to_string(),
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

fn child_by_field<'a>(node: Node<'a>, field: &str) -> Option<Node<'a>> {
    node.child_by_field_name(field)
}

fn ident_text(node: Node, src: &[u8]) -> Option<String> {
    child_by_field(node, "name").map(|n| node_text(n, src))
}

fn is_public(node: Node, src: &[u8]) -> bool {
    // Look for a child `visibility_modifier` like `pub`.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return node_text(child, src).starts_with("pub");
        }
    }
    false
}

fn make_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node, is_pub: bool) -> KgNode {
    // Lines from the chunk start are 0-based in tree-sitter; offset by chunk start.
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("rust:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "rust".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: is_pub,
        extra: serde_json::Value::Null,
    }
}

/// Recursive walker over a Rust AST. We push nodes/edges into `graph` as we
/// encounter declarations. Nested traversal is enough — we don't need a real
/// scope stack to emit symbol nodes.
fn walk_rust(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph) {
    let file_id = format!("rust:File:{}", chunk.file);

    fn recurse(
        node: Node,
        src: &[u8],
        chunk: &CodeChunk,
        graph: &mut KgGraph,
        file_id: &str,
        inside_impl: bool,
    ) {
        match node.kind() {
            "function_item" => {
                if let Some(name) = ident_text(node, src) {
                    let is_test = has_test_attribute(node, src);
                    let kind = if is_test {
                        KgNodeKind::TestCase
                    } else if inside_impl {
                        KgNodeKind::Method
                    } else {
                        KgNodeKind::Function
                    };
                    let pub_ = is_public(node, src);
                    let n = make_node(kind, &name, chunk, node, pub_);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: file_id.to_string(),
                        to: id.clone(),
                        kind: KgEdgeKind::Contains,
                        weight: 1.0,
                    });
                    // Extract direct calls from the function body and emit
                    // deduplicated Calls edges from this fn to each callee.
                    if let Some(body) = child_by_field(node, "body") {
                        for edge in extract_calls(body, src, &id) {
                            graph.edges.push(edge);
                        }
                    }
                }
            }
            "struct_item" | "enum_item" | "union_item" => {
                if let Some(name) = ident_text(node, src) {
                    let pub_ = is_public(node, src);
                    let n = make_node(KgNodeKind::Class, &name, chunk, node, pub_);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: file_id.to_string(),
                        to: id,
                        kind: KgEdgeKind::Contains,
                        weight: 1.0,
                    });
                }
            }
            "trait_item" => {
                if let Some(name) = ident_text(node, src) {
                    let pub_ = is_public(node, src);
                    let n = make_node(KgNodeKind::Interface, &name, chunk, node, pub_);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: file_id.to_string(),
                        to: id,
                        kind: KgEdgeKind::Contains,
                        weight: 1.0,
                    });
                }
            }
            "mod_item" => {
                if let Some(name) = ident_text(node, src) {
                    let pub_ = is_public(node, src);
                    let n = make_node(KgNodeKind::Module, &name, chunk, node, pub_);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: file_id.to_string(),
                        to: id,
                        kind: KgEdgeKind::Contains,
                        weight: 1.0,
                    });
                }
            }
            "use_declaration" => {
                // Pull the argument text as a single identifier name.
                let txt = node_text(node, src);
                let name = txt
                    .trim_start_matches("pub ")
                    .trim_start_matches("use ")
                    .trim_end_matches(';')
                    .trim()
                    .to_string();
                if !name.is_empty() {
                    let n = make_node(KgNodeKind::Import, &name, chunk, node, false);
                    let id = n.id.clone();
                    graph.nodes.push(n);
                    graph.edges.push(KgEdge {
                        from: file_id.to_string(),
                        to: id,
                        kind: KgEdgeKind::Imports,
                        weight: 1.0,
                    });
                }
            }
            "impl_item" => {
                // impl Trait for Type → Implements edge between the trait and the type.
                let type_ = child_by_field(node, "type").map(|n| node_text(n, src));
                let trait_ = child_by_field(node, "trait").map(|n| node_text(n, src));
                if let (Some(t), Some(tr)) = (type_.as_ref(), trait_.as_ref()) {
                    let type_id = format!("rust:Class:{}:{t}", chunk.file);
                    let trait_id = format!("rust:Interface:{}:{tr}", chunk.file);
                    graph.edges.push(KgEdge {
                        from: type_id,
                        to: trait_id,
                        kind: KgEdgeKind::Implements,
                        weight: 1.0,
                    });
                }
                // Recurse into the impl block so member fns get tagged as methods.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    recurse(child, src, chunk, graph, file_id, true);
                }
                return;
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            recurse(child, src, chunk, graph, file_id, inside_impl);
        }
    }

    recurse(node, src, chunk, graph, &file_id, false);
}

/// Returns true if any sibling/preceding `attribute_item` contains `test`.
/// We keep this lossy and string-based; `#[tokio::test]` matches as well,
/// which is the right default for our taxonomy.
fn has_test_attribute(node: Node, src: &[u8]) -> bool {
    // Walk preceding siblings looking for attribute_item ending the line above.
    let mut sib = node.prev_sibling();
    while let Some(s) = sib {
        if s.kind() == "attribute_item" {
            let txt = node_text(s, src);
            if txt.contains("test") {
                return true;
            }
            sib = s.prev_sibling();
            continue;
        }
        if s.kind() == "line_comment" || s.kind() == "block_comment" {
            sib = s.prev_sibling();
            continue;
        }
        break;
    }
    false
}

/// Extract `call_expression` nodes from a function body and produce
/// deduplicated `Calls` edges.
///
/// Why: A function's outgoing call graph is one of the most useful pieces of
/// static analysis we can derive cheaply. Without it the Rust knowledge graph
/// has only structural (Contains/Imports/Implements) edges and no behavioral
/// edges describing what calls what.
///
/// What: Walks the AST subtree rooted at `fn_body`, collects every direct
/// `call_expression` (skipping ones inside nested closures and inner
/// function/impl items so each fn only emits its own direct calls), resolves
/// the callee name from the `function` field, counts how many times each
/// callee appears, and returns one `KgEdge` per unique callee with
/// `weight = call_count as f32`.
///
/// Test: `rust_adapter_extracts_call_edges` and
/// `rust_adapter_deduplicates_repeated_calls` cover the happy paths.
fn extract_calls(fn_body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Skip recursion into nested function-like / impl bodies — we only
        // want direct calls inside the current function.
        match node.kind() {
            "closure_expression" | "function_item" | "impl_item" | "trait_item" | "mod_item" => {
                return;
            }
            "call_expression" => {
                if let Some(callee) = callee_name(node, src) {
                    *counts.entry(callee).or_insert(0) += 1;
                }
                // Still recurse so that nested call_expressions inside the
                // arguments of the outer call are counted (e.g. `foo(bar())`).
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, src, counts);
        }
    }

    visit(fn_body, src, &mut counts);

    counts
        .into_iter()
        .map(|(callee, count)| KgEdge {
            from: caller_id.to_string(),
            to: build_call_target("rust", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a `call_expression` node.
///
/// Why: Cross-file resolution is out of scope for the adapter (the linker
/// merges by qualified_name), so we only need a stable string handle for the
/// callee — bare identifier, scoped path, or method name.
///
/// What: Inspects the `function` field. Returns the bare identifier text for
/// `identifier`, the full path text for `scoped_identifier`, the method name
/// (`field` subnode) for `field_expression`, or `None` otherwise.
///
/// Test: Exercised indirectly by the `extract_calls` tests.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let fun = call.child_by_field_name("function")?;
    match fun.kind() {
        "identifier" => Some(node_text(fun, src)),
        "scoped_identifier" => Some(node_text(fun, src)),
        "field_expression" => fun.child_by_field_name("field").map(|f| node_text(f, src)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str) -> CodeChunk {
        CodeChunk {
            id: "f.rs:1:10".into(),
            file: "f.rs".into(),
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
    fn rust_analyzer_extracts_function() {
        let a = RustAnalyzer::new();
        let c = make_chunk("fn hello() {}\n");
        let r = a.analyze_chunks(&[c]);
        assert_eq!(r.analyzed_chunks, 1);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph);
        assert_eq!(funcs[0].name, "hello");
        assert_eq!(funcs[0].language, "rust");
    }

    #[test]
    fn rust_analyzer_extracts_struct_and_trait() {
        let a = RustAnalyzer::new();
        let c = make_chunk(
            "pub struct Foo;\n\
             pub trait Bar {}\n",
        );
        let r = a.analyze_chunks(&[c]);
        let kinds: Vec<&KgNodeKind> = r.graph.nodes.iter().map(|n| &n.kind).collect();
        assert!(kinds.iter().any(|k| matches!(k, KgNodeKind::Class)));
        assert!(kinds.iter().any(|k| matches!(k, KgNodeKind::Interface)));
    }

    #[test]
    fn rust_analyzer_extracts_test_fn() {
        let a = RustAnalyzer::new();
        let c = make_chunk("#[test]\nfn it_works() {}\n");
        let r = a.analyze_chunks(&[c]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::TestCase) && n.name == "it_works"));
    }

    #[test]
    fn rust_adapter_extracts_call_edges() {
        let src = r#"
/// Caller function
fn caller() {
    helper();
    std::mem::drop(x);
}

/// Helper function
fn helper() {}
"#;
        let chunks = vec![CodeChunk {
            id: "test:1:10".into(),
            file: "test.rs".into(),
            start_line: 1,
            end_line: 10,
            content: src.into(),
            ..Default::default()
        }];
        let result = RustAnalyzer::new().analyze_chunks(&chunks);
        let calls: Vec<_> = result
            .graph
            .edges
            .iter()
            .filter(|e| e.kind == KgEdgeKind::Calls)
            .collect();
        assert!(
            !calls.is_empty(),
            "expected at least one Calls edge, got none"
        );
        let has_helper_call = calls.iter().any(|e| e.to.contains("helper"));
        assert!(
            has_helper_call,
            "expected edge to 'helper', edges: {calls:?}"
        );
    }

    #[test]
    fn rust_adapter_deduplicates_repeated_calls() {
        let src = r#"
/// fn with repeated calls
fn foo() {
    bar();
    bar();
    bar();
}
fn bar() {}
"#;
        let chunks = vec![CodeChunk {
            id: "test:1:8".into(),
            file: "test.rs".into(),
            start_line: 1,
            end_line: 8,
            content: src.into(),
            ..Default::default()
        }];
        let result = RustAnalyzer::new().analyze_chunks(&chunks);
        let bar_edges: Vec<_> = result
            .graph
            .edges
            .iter()
            .filter(|e| e.kind == KgEdgeKind::Calls && e.to.contains("bar"))
            .collect();
        assert_eq!(bar_edges.len(), 1, "repeated calls should be deduplicated");
        assert!(
            (bar_edges[0].weight - 3.0).abs() < f32::EPSILON,
            "weight should reflect call count=3, got {}",
            bar_edges[0].weight
        );
    }

    #[test]
    fn supports_dot_rs_files() {
        let a = RustAnalyzer::new();
        assert!(a.supports("src/main.rs"));
        assert!(!a.supports("foo.ts"));
    }
}
