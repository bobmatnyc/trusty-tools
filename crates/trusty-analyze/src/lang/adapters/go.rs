//! Go `LanguageAnalyzer` adapter backed by tree-sitter-go.
//!
//! Why: Extracts Go structure — functions, methods, types (struct /
//! interface), imports, test functions, and outgoing call edges — into a
//! language-neutral `KgGraph`. The call edges are scoped to the enclosing
//! function/method so the graph captures behavioral relationships, not just
//! structural ones.
//!
//! What: For each `CodeChunk`, parses with tree-sitter-go, walks the tree,
//! and emits:
//! - `Function` for `function_declaration`
//! - `Method` for `method_declaration` (has a receiver); ID is prefixed with
//!   the receiver type (`go:Method:file:Receiver:MethodName`) so distinct
//!   receivers don't collide
//! - `Class` for `type_declaration` wrapping a `struct_type`
//! - `Interface` for `type_declaration` wrapping an `interface_type`
//! - `Import` + `Imports` edges for `import_declaration` / `import_spec`
//!   (quoted import paths are unquoted)
//! - `TestCase` for functions named `Test*` taking `*testing.T`
//! - `Calls` edges from the enclosing function/method to each callee, one
//!   edge per unique callee with `weight = call_count`
//! - `is_public` set when the identifier starts with an uppercase letter
//! - `doc_comment` captured from `comment` siblings immediately preceding
//!   the declaration
//!
//! Test: see `tests` module — covers function, method, struct, imports,
//! call edges (with weights and caller scoping), and doc comments.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-go-backed analyzer.
pub struct GoAnalyzer;

impl GoAnalyzer {
    /// Construct a stateless analyzer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for GoAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for GoAnalyzer {
    fn language(&self) -> &str {
        "go"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".go"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-go grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "go analyze chunk");
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

fn node_text(node: Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn name_of(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

/// Capitalized identifier → exported (`is_public: true`) in Go.
fn is_exported(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Walk backward through preceding comment siblings and join them.
fn preceding_doc(node: Node, src: &[u8]) -> Option<String> {
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

fn make_node(
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
fn make_method_node(
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
fn is_test_function(name: &str, fn_node: Node, src: &[u8]) -> bool {
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
fn receiver_type(method: Node, src: &[u8]) -> Option<String> {
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

/// Strip surrounding quotes from a Go interpreted string literal so import
/// targets are clean (`"fmt"` → `fmt`).
fn unquote_import(s: &str) -> String {
    let trimmed = s.trim();
    trimmed
        .trim_start_matches('"')
        .trim_end_matches('"')
        .to_string()
}

fn walk(root: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph) {
    let file_id = format!("go:File:{}", chunk.file);
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        emit_top_level(child, src, chunk, &file_id, graph);
    }
}

fn emit_top_level(node: Node, src: &[u8], chunk: &CodeChunk, file_id: &str, graph: &mut KgGraph) {
    match node.kind() {
        "function_declaration" => {
            let Some(name) = name_of(node, src) else {
                return;
            };
            let doc = preceding_doc(node, src);
            let kind = if is_test_function(&name, node, src) {
                KgNodeKind::TestCase
            } else {
                KgNodeKind::Function
            };
            let n = make_node(kind, &name, chunk, node, doc);
            let id = n.id.clone();
            graph.nodes.push(n);
            graph.edges.push(KgEdge {
                from: file_id.to_string(),
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
        "method_declaration" => {
            let Some(name) = name_of(node, src) else {
                return;
            };
            let doc = preceding_doc(node, src);
            let receiver = receiver_type(node, src).unwrap_or_default();
            let n = make_method_node(&receiver, &name, chunk, node, doc);
            let id = n.id.clone();
            graph.nodes.push(n);
            graph.edges.push(KgEdge {
                from: file_id.to_string(),
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
        "type_declaration" => {
            // type_declaration → type_spec(s) → name + type
            let doc = preceding_doc(node, src);
            let mut cursor = node.walk();
            for spec in node.children(&mut cursor) {
                if spec.kind() != "type_spec" {
                    continue;
                }
                let Some(name) = name_of(spec, src) else {
                    continue;
                };
                let Some(type_node) = spec.child_by_field_name("type") else {
                    continue;
                };
                let kind = match type_node.kind() {
                    "struct_type" => KgNodeKind::Class,
                    "interface_type" => KgNodeKind::Interface,
                    _ => continue,
                };
                let n = make_node(kind, &name, chunk, spec, doc.clone());
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
        "import_declaration" => {
            // import_declaration may contain a single import_spec or an
            // import_spec_list wrapping many.
            let mut stack = vec![node];
            while let Some(cur) = stack.pop() {
                if cur.kind() == "import_spec" {
                    // Prefer the `path` field (interpreted_string_literal) so
                    // we end up with a clean unquoted module path.
                    let raw = cur
                        .child_by_field_name("path")
                        .map(|p| node_text(p, src))
                        .unwrap_or_else(|| node_text(cur, src));
                    let unquoted = unquote_import(&raw);
                    if !unquoted.is_empty() {
                        let n = make_node(KgNodeKind::Import, &unquoted, chunk, cur, None);
                        let id = n.id.clone();
                        graph.nodes.push(n);
                        graph.edges.push(KgEdge {
                            from: file_id.to_string(),
                            to: id,
                            kind: KgEdgeKind::Imports,
                            weight: 1.0,
                        });
                    }
                    continue;
                }
                let mut c = cur.walk();
                for child in cur.children(&mut c) {
                    stack.push(child);
                }
            }
        }
        _ => {}
    }
}

/// Extract `call_expression` nodes from a function/method body and produce
/// deduplicated `Calls` edges keyed by callee name.
///
/// Why: A function's outgoing call graph is one of the most useful pieces of
/// static analysis we can derive cheaply and is required for graph traversal
/// queries ("what calls auth?"). Without scoped extraction, every call site
/// would be emitted as an orphan edge with no caller.
///
/// What: Walks the AST subtree rooted at `body`, collects every direct
/// `call_expression` (skipping nested function literals and inner
/// function/method declarations so each function only emits its own direct
/// calls), resolves the callee name from the `function` field, counts
/// repeats, and returns one `KgEdge` per unique callee with
/// `weight = call_count as f32`.
///
/// Test: `go_adapter_extracts_call_edges` and
/// `go_adapter_deduplicates_repeated_calls` cover the happy paths.
fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Stop at nested function-like / type bodies so each function only
        // attributes its own direct calls.
        match node.kind() {
            "function_declaration" | "method_declaration" | "func_literal" | "function_literal" => {
                return;
            }
            "call_expression" => {
                if let Some(callee) = callee_name(node, src) {
                    *counts.entry(callee).or_insert(0) += 1;
                }
                // Still recurse so nested calls inside arguments are counted
                // (e.g. `foo(bar())` records both foo and bar).
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
            to: build_call_target("go", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a Go `call_expression` node.
///
/// Why: Cross-file resolution (and even cross-package binding) is out of
/// scope for the adapter; the linker merges by qualified_name later. We only
/// need a stable string handle for the callee.
///
/// What: Inspects the `function` field. Returns the bare text for
/// `identifier` (`foo()` → `foo`), the `field_identifier` of a
/// `selector_expression` (`pkg.Foo()` or `recv.Method()` → `Foo` /
/// `Method`), or `None` for unsupported forms (e.g. dynamic
/// `slice[i]()` calls).
///
/// Test: Exercised indirectly by the `extract_calls` tests.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let fun = call.child_by_field_name("function")?;
    match fun.kind() {
        "identifier" => Some(node_text(fun, src)),
        "selector_expression" => fun.child_by_field_name("field").map(|f| node_text(f, src)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str) -> CodeChunk {
        CodeChunk {
            id: "main.go:1:20".into(),
            file: "main.go".into(),
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
    fn go_supports_go_files() {
        let a = GoAnalyzer::new();
        assert!(a.supports("main.go"));
        assert!(!a.supports("main.rs"));
    }

    #[test]
    fn go_extracts_function() {
        let a = GoAnalyzer::new();
        let c = make_chunk("package main\n\nfunc Hello() {}\n");
        let r = a.analyze_chunks(&[c]);
        assert_eq!(r.analyzed_chunks, 1);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(funcs[0].name, "Hello");
        assert!(funcs[0].is_public, "Hello should be exported");
    }

    #[test]
    fn go_lowercase_function_is_not_public() {
        let a = GoAnalyzer::new();
        let c = make_chunk("package main\n\nfunc helper() {}\n");
        let r = a.analyze_chunks(&[c]);
        let f = r
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, KgNodeKind::Function))
            .unwrap();
        assert!(!f.is_public);
    }

    #[test]
    fn go_test_function_detected() {
        let a = GoAnalyzer::new();
        let c = make_chunk("package main\n\nimport \"testing\"\n\nfunc TestFoo(t *testing.T) {}\n");
        let r = a.analyze_chunks(&[c]);
        assert!(
            r.graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, KgNodeKind::TestCase) && n.name == "TestFoo"),
            "graph: {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn go_extracts_struct_and_interface() {
        let a = GoAnalyzer::new();
        let c = make_chunk(
            "package main\n\
             \n\
             type Foo struct { X int }\n\
             type Bar interface { Run() }\n",
        );
        let r = a.analyze_chunks(&[c]);
        let kinds: Vec<&KgNodeKind> = r.graph.nodes.iter().map(|n| &n.kind).collect();
        assert!(kinds.iter().any(|k| matches!(k, KgNodeKind::Class)));
        assert!(kinds.iter().any(|k| matches!(k, KgNodeKind::Interface)));
    }

    #[test]
    fn go_extracts_struct_class() {
        let a = GoAnalyzer::new();
        let c = make_chunk(
            "package main\n\
             \n\
             type Widget struct { N int }\n",
        );
        let r = a.analyze_chunks(&[c]);
        let class = r
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, KgNodeKind::Class))
            .expect("expected a Class node for struct Widget");
        assert_eq!(class.name, "Widget");
        assert!(class.is_public);
    }

    #[test]
    fn go_extracts_method() {
        let a = GoAnalyzer::new();
        let c = make_chunk(
            "package main\n\
             \n\
             type Foo struct{}\n\
             func (f *Foo) Bar() {}\n",
        );
        let r = a.analyze_chunks(&[c]);
        let method = r
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, KgNodeKind::Method) && n.name == "Bar")
            .expect("expected Method node Bar");
        // Receiver type must be encoded into the ID and qualified_name.
        assert!(
            method.id.contains(":Foo:Bar"),
            "method id should embed receiver type, got {}",
            method.id
        );
        assert_eq!(method.qualified_name, "Foo.Bar");
    }

    #[test]
    fn go_extracts_imports() {
        let a = GoAnalyzer::new();
        let c = make_chunk("package main\n\nimport (\n    \"fmt\"\n    \"os\"\n)\n");
        let r = a.analyze_chunks(&[c]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        assert_eq!(imports.len(), 2);
        // Quotes must be stripped from the import path.
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"fmt"), "expected unquoted fmt: {names:?}");
        assert!(names.contains(&"os"), "expected unquoted os: {names:?}");
    }

    #[test]
    fn go_extracts_single_import() {
        let a = GoAnalyzer::new();
        let c = make_chunk("package main\n\nimport \"fmt\"\n");
        let r = a.analyze_chunks(&[c]);
        let import_edges: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Imports))
            .collect();
        assert_eq!(import_edges.len(), 1);
        assert!(
            import_edges[0].to.ends_with(":fmt"),
            "import edge target should end with :fmt, got {}",
            import_edges[0].to
        );
    }

    #[test]
    fn go_doc_comment_captured() {
        let a = GoAnalyzer::new();
        let c = make_chunk("package main\n\n// Hello greets the world.\nfunc Hello() {}\n");
        let r = a.analyze_chunks(&[c]);
        let f = r
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, KgNodeKind::Function))
            .unwrap();
        assert!(f.doc_comment.is_some());
        assert!(f.doc_comment.as_ref().unwrap().contains("greets"));
    }

    #[test]
    fn go_adapter_extracts_call_edges() {
        let src = "package main\n\
                   \n\
                   func caller() {\n\
                       helper()\n\
                       fmt.Println(\"hi\")\n\
                   }\n\
                   \n\
                   func helper() {}\n";
        let c = make_chunk(src);
        let r = GoAnalyzer::new().analyze_chunks(&[c]);
        let calls: Vec<&KgEdge> = r
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
        let has_println = calls.iter().any(|e| e.to.contains("Println"));
        assert!(has_helper, "expected edge to 'helper', got {calls:?}");
        assert!(has_println, "expected edge to 'Println', got {calls:?}");
        // Caller must be scoped to the function/method, not the file.
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Function:") || e.from.contains(":Method:")),
            "Calls edges should originate from a function/method node, got {calls:?}"
        );
    }

    #[test]
    fn go_adapter_deduplicates_repeated_calls() {
        let src = "package main\n\
                   \n\
                   func foo() {\n\
                       bar()\n\
                       bar()\n\
                       bar()\n\
                   }\n\
                   \n\
                   func bar() {}\n";
        let c = make_chunk(src);
        let r = GoAnalyzer::new().analyze_chunks(&[c]);
        let bar_edges: Vec<&KgEdge> = r
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
    fn go_adapter_method_call_edges_scoped_to_method() {
        let src = "package main\n\
                   \n\
                   type Foo struct{}\n\
                   \n\
                   func (f *Foo) Bar() {\n\
                       helper()\n\
                       helper()\n\
                   }\n\
                   \n\
                   func helper() {}\n";
        let c = make_chunk(src);
        let r = GoAnalyzer::new().analyze_chunks(&[c]);
        let calls: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        assert_eq!(calls.len(), 1, "expected one deduped call edge: {calls:?}");
        assert!(
            calls[0].from.contains(":Method:") && calls[0].from.contains(":Foo:Bar"),
            "call edge should originate from method Foo.Bar, got {}",
            calls[0].from
        );
        assert!(
            (calls[0].weight - 2.0).abs() < f32::EPSILON,
            "weight should be 2, got {}",
            calls[0].weight
        );
    }
}
