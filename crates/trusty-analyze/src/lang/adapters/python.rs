//! Python `LanguageAnalyzer` adapter backed by tree-sitter-python.
//!
//! Why: Extracts Python structure — functions, classes, methods, imports,
//! and test cases — into a language-neutral `KgGraph`. Mirrors the Rust and
//! TypeScript adapters so the analyzer registry behaves uniformly across
//! languages.
//!
//! What: For each `CodeChunk`, parses the content with tree-sitter-python,
//! walks the tree, and emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for top-level `function_definition`
//! - `Method` nodes for `function_definition` nested in a class
//! - `Class` nodes for `class_definition`
//! - `Import` nodes + `Imports` edges for `import_statement` /
//!   `import_from_statement`
//! - `TestCase` nodes for functions decorated with anything containing `test`
//!   or named `test_*`
//! - `Contains` edges from file to top-level items, and from classes to
//!   their methods
//!
//! - `Calls` edges from each function/method to its callees, scoped to the
//!   enclosing function/method, deduplicated with `weight = call_count`
//!
//! Test: `python_extracts_function` and `python_extracts_class` cover the
//! basic happy paths; `python_adapter_extracts_call_edges` and
//! `python_adapter_deduplicates_repeated_calls` cover call-edge extraction.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-python-backed analyzer.
pub struct PythonAnalyzer;

impl PythonAnalyzer {
    /// Construct a stateless analyzer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PythonAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for PythonAnalyzer {
    fn language(&self) -> &str {
        "python"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".py", ".pyi"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-python grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "python analyze chunk");
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
        id: format!("python:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "python".into(),
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
    let is_public = !name.starts_with('_');
    KgNode {
        id: format!("python:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "python".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: doc,
        is_public,
        extra: serde_json::Value::Null,
    }
}

/// Build a method node where the ID is `python:Method:file:Class:Name` so
/// methods on different classes don't collide. The displayed `name` stays
/// just the method name; `qualified_name` includes the class.
///
/// Why: Python methods on distinct classes share simple names (e.g. `__init__`,
/// `run`); without a class qualifier the linker would collapse them.
/// What: Returns a `KgNode` with `id = python:Method:file:Class:Name` and
/// `qualified_name = Class.Name`.
/// Test: `python_extracts_class_methods_with_qualified_ids`.
fn make_method_node(
    class_name: &str,
    name: &str,
    chunk: &CodeChunk,
    ast: Node,
    doc: Option<String>,
) -> KgNode {
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
        id: format!("python:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "python".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: doc,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// First expression-statement-string child of `block` is the docstring.
fn extract_docstring(definition: Node, src: &[u8]) -> Option<String> {
    let body = definition.child_by_field_name("body")?;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            let mut c2 = child.walk();
            for inner in child.children(&mut c2) {
                if inner.kind() == "string" {
                    return Some(node_text(inner, src));
                }
            }
            return None;
        }
    }
    None
}

/// True if any decorator on `decorated_definition` matches a test pattern.
fn has_test_decorator(decorated: Node, src: &[u8]) -> bool {
    let mut cursor = decorated.walk();
    for child in decorated.children(&mut cursor) {
        if child.kind() == "decorator" {
            let txt = node_text(child, src);
            if txt.contains("test") || txt.contains("pytest") {
                return true;
            }
        }
    }
    false
}

fn walk(root: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph) {
    let file_id = format!("python:File:{}", chunk.file);

    fn emit_function_like(
        def: Node,
        src: &[u8],
        chunk: &CodeChunk,
        graph: &mut KgGraph,
        parent_id: &str,
        class_name: Option<&str>,
        is_test: bool,
    ) {
        let Some(name) = name_of(def, src) else {
            return;
        };
        let doc = extract_docstring(def, src);
        let (id, kind_label) = if let Some(cn) = class_name {
            let n = make_method_node(cn, &name, chunk, def, doc);
            let id = n.id.clone();
            graph.nodes.push(n);
            (id, "Method")
        } else if is_test {
            let n = make_node(KgNodeKind::TestCase, &name, chunk, def, doc);
            let id = n.id.clone();
            graph.nodes.push(n);
            (id, "TestCase")
        } else {
            let n = make_node(KgNodeKind::Function, &name, chunk, def, doc);
            let id = n.id.clone();
            graph.nodes.push(n);
            (id, "Function")
        };
        let _ = kind_label;
        graph.edges.push(KgEdge {
            from: parent_id.to_string(),
            to: id.clone(),
            kind: KgEdgeKind::Contains,
            weight: 1.0,
        });
        if let Some(body) = def.child_by_field_name("body") {
            for edge in extract_calls(body, src, &id) {
                graph.edges.push(edge);
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
                let name = name_of(node, src).unwrap_or_default();
                let is_test = class_name.is_none() && name.starts_with("test_");
                emit_function_like(node, src, chunk, graph, parent_id, class_name, is_test);
                // Don't recurse into function body for symbol extraction.
                return;
            }
            "decorated_definition" => {
                let mut cursor = node.walk();
                let mut inner_def: Option<Node> = None;
                for child in node.children(&mut cursor) {
                    if child.kind() == "function_definition" || child.kind() == "class_definition" {
                        inner_def = Some(child);
                        break;
                    }
                }
                let Some(def) = inner_def else {
                    return;
                };
                if def.kind() == "function_definition" {
                    let name = name_of(def, src).unwrap_or_default();
                    let is_test = class_name.is_none()
                        && (has_test_decorator(node, src) || name.starts_with("test_"));
                    emit_function_like(def, src, chunk, graph, parent_id, class_name, is_test);
                    return;
                }
                // class_definition: fall through to normal handling.
                recurse(def, src, chunk, graph, parent_id, class_name);
                return;
            }
            "class_definition" => {
                if let Some(name) = name_of(node, src) {
                    let doc = extract_docstring(node, src);
                    let n = make_node(KgNodeKind::Class, &name, chunk, node, doc);
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
            "import_statement" | "import_from_statement" => {
                emit_imports(node, src, chunk, graph, parent_id);
                return;
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            recurse(child, src, chunk, graph, parent_id, class_name);
        }
    }

    recurse(root, src, chunk, graph, &file_id, None);
}

/// Emit `Import` nodes + `Imports` edges from a Python import statement.
///
/// Why: Import edges drive the file/module-level dependency graph; one node
/// per imported target gives the graph a clean fan-out instead of a single
/// concatenated string.
/// What: For `import a, b.c` emits one node per dotted name. For
/// `from foo import bar, baz` emits `foo.bar` and `foo.baz`. Falls back to
/// the raw statement text if the AST shape is unexpected.
/// Test: `python_extracts_imports` and `python_extracts_from_imports`.
fn emit_imports(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    let mut targets: Vec<String> = Vec::new();

    match node.kind() {
        "import_statement" => {
            // children: `import` <name>, <name>, ...; each <name> is dotted_name or aliased_import
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "dotted_name" => targets.push(node_text(child, src)),
                    "aliased_import" => {
                        if let Some(name) = child.child_by_field_name("name") {
                            targets.push(node_text(name, src));
                        }
                    }
                    _ => {}
                }
            }
        }
        "import_from_statement" => {
            let module = node
                .child_by_field_name("module_name")
                .map(|n| node_text(n, src))
                .unwrap_or_default();
            // Collect imported names; module_name is also a child so skip it.
            let module_name_node = node.child_by_field_name("module_name");
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if Some(child.id()) == module_name_node.map(|n| n.id()) {
                    continue;
                }
                match child.kind() {
                    "dotted_name" => {
                        let nm = node_text(child, src);
                        if !nm.is_empty() {
                            targets.push(if module.is_empty() {
                                nm
                            } else {
                                format!("{module}.{nm}")
                            });
                        }
                    }
                    "aliased_import" => {
                        if let Some(name) = child.child_by_field_name("name") {
                            let nm = node_text(name, src);
                            targets.push(if module.is_empty() {
                                nm
                            } else {
                                format!("{module}.{nm}")
                            });
                        }
                    }
                    "wildcard_import" if !module.is_empty() => {
                        targets.push(format!("{module}.*"));
                    }
                    _ => {}
                }
            }
            if targets.is_empty() && !module.is_empty() {
                targets.push(module);
            }
        }
        _ => {}
    }

    if targets.is_empty() {
        let cleaned = node_text(node, src).trim().to_string();
        if !cleaned.is_empty() {
            targets.push(cleaned);
        }
    }

    for target in targets {
        let n = make_node(KgNodeKind::Import, &target, chunk, node, None);
        let id = n.id.clone();
        graph.nodes.push(n);
        graph.edges.push(KgEdge {
            from: parent_id.to_string(),
            to: id,
            kind: KgEdgeKind::Imports,
            weight: 1.0,
        });
    }
}

/// Extract `call` expression nodes from a function/method body and produce
/// deduplicated `Calls` edges keyed by callee name.
///
/// Why: A function's outgoing call graph is the most useful behavioral
/// signal we can derive cheaply; emitting each call site as a separate file-
/// scoped orphan defeats graph traversal queries ("what calls auth?").
/// What: Walks the AST subtree rooted at `body`, collects every direct `call`
/// (skipping nested function/class bodies so each function only emits its own
/// direct calls), resolves the callee name from the `function` field, counts
/// repeats, skips uninteresting `self`/`cls`, and returns one `KgEdge` per
/// unique callee with `weight = call_count as f32`.
/// Test: `python_adapter_extracts_call_edges` and
/// `python_adapter_deduplicates_repeated_calls` cover the happy paths.
fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Stop at nested function-like / class bodies so each function only
        // attributes its own direct calls.
        match node.kind() {
            "function_definition" | "class_definition" | "lambda" => {
                return;
            }
            "call" => {
                if let Some(callee) = callee_name(node, src) {
                    if callee != "self" && callee != "cls" {
                        *counts.entry(callee).or_insert(0) += 1;
                    }
                }
                // Recurse into arguments so nested calls are still counted
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
            to: build_call_target("python", "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a Python `call` node.
///
/// Why: Cross-file resolution is out of scope for the adapter (the linker
/// merges by qualified_name later). We only need a stable string handle.
/// What: Inspects the `function` field. Returns the bare text for
/// `identifier`, the innermost attribute name for `attribute`
/// (`a.b.c()` → `c`), or `None` for unsupported forms (e.g. dynamic
/// `arr[0]()` calls).
/// Test: Exercised indirectly by the `extract_calls` tests.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let fun = call.child_by_field_name("function")?;
    match fun.kind() {
        "identifier" => Some(node_text(fun, src)),
        "attribute" => fun
            .child_by_field_name("attribute")
            .map(|a| node_text(a, src)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str) -> CodeChunk {
        CodeChunk {
            id: "f.py:1:10".into(),
            file: "f.py".into(),
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
    fn python_supports_py_files() {
        let a = PythonAnalyzer::new();
        assert!(a.supports("foo.py"));
        assert!(a.supports("stubs.pyi"));
        assert!(!a.supports("foo.rs"));
    }

    #[test]
    fn python_extracts_function() {
        let a = PythonAnalyzer::new();
        let c = make_chunk("def hello():\n    pass\n");
        let r = a.analyze_chunks(&[c]);
        assert_eq!(r.analyzed_chunks, 1);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(funcs[0].name, "hello");
        assert_eq!(funcs[0].language, "python");
        assert!(funcs[0].is_public);
    }

    #[test]
    fn python_extracts_class() {
        let a = PythonAnalyzer::new();
        let c = make_chunk("class Foo:\n    def bar(self):\n        pass\n");
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
    }

    #[test]
    fn python_private_function_is_not_public() {
        let a = PythonAnalyzer::new();
        let c = make_chunk("def _hidden():\n    pass\n");
        let r = a.analyze_chunks(&[c]);
        let f = r
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, KgNodeKind::Function))
            .expect("function node");
        assert!(!f.is_public);
    }

    #[test]
    fn python_test_function_detected() {
        let a = PythonAnalyzer::new();
        let c = make_chunk("def test_login():\n    pass\n");
        let r = a.analyze_chunks(&[c]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::TestCase)));
    }

    #[test]
    fn python_extracts_imports() {
        let a = PythonAnalyzer::new();
        let c = make_chunk("import os\nfrom pathlib import Path\n");
        let r = a.analyze_chunks(&[c]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        assert_eq!(imports.len(), 2, "graph: {:?}", r.graph.nodes);
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"os"),
            "expected 'os' import target, got {names:?}"
        );
        assert!(
            names.contains(&"pathlib.Path"),
            "expected 'pathlib.Path' import target, got {names:?}"
        );
    }

    #[test]
    fn python_extracts_class_methods_with_qualified_ids() {
        let a = PythonAnalyzer::new();
        let c = make_chunk(
            "class Foo:\n    def bar(self):\n        pass\n    def baz(self):\n        pass\n",
        );
        let r = a.analyze_chunks(&[c]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 2, "expected two methods, got {methods:?}");
        for m in &methods {
            assert!(
                m.id.contains(":Foo:"),
                "method id should embed class name 'Foo', got {}",
                m.id
            );
            assert!(
                m.qualified_name.starts_with("Foo."),
                "qualified_name should start with 'Foo.', got {}",
                m.qualified_name
            );
        }
    }

    #[test]
    fn python_adapter_extracts_call_edges() {
        let a = PythonAnalyzer::new();
        let src = "def caller():\n    helper()\n    obj.method()\n\ndef helper():\n    pass\n";
        let c = make_chunk(src);
        let r = a.analyze_chunks(&[c]);
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
        let has_method = calls.iter().any(|e| e.to.contains("method"));
        assert!(has_helper, "expected edge to 'helper', got {calls:?}");
        assert!(has_method, "expected edge to 'method', got {calls:?}");
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Function:") || e.from.contains(":Method:")),
            "Calls edges should originate from a function/method node, got {calls:?}"
        );
    }

    #[test]
    fn python_adapter_deduplicates_repeated_calls() {
        let a = PythonAnalyzer::new();
        let src = "def caller():\n    foo()\n    foo()\n    foo()\n\ndef foo():\n    pass\n";
        let c = make_chunk(src);
        let r = a.analyze_chunks(&[c]);
        let foo_edges: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls) && e.to.contains("foo"))
            .collect();
        assert_eq!(
            foo_edges.len(),
            1,
            "repeated calls should be deduplicated, got {foo_edges:?}"
        );
        assert!(
            (foo_edges[0].weight - 3.0).abs() < f32::EPSILON,
            "weight should reflect call count=3, got {}",
            foo_edges[0].weight
        );
    }

    #[test]
    fn python_method_call_edges_scoped_to_method() {
        let a = PythonAnalyzer::new();
        let src = "class Foo:\n    def bar(self):\n        helper()\n        helper()\n\ndef helper():\n    pass\n";
        let c = make_chunk(src);
        let r = a.analyze_chunks(&[c]);
        let calls: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        assert_eq!(calls.len(), 1, "expected one deduped call edge: {calls:?}");
        assert!(
            calls[0].from.contains(":Method:") && calls[0].from.contains(":Foo:bar"),
            "call edge should originate from method Foo.bar, got {}",
            calls[0].from
        );
        assert!(
            (calls[0].weight - 2.0).abs() < f32::EPSILON,
            "weight should be 2, got {}",
            calls[0].weight
        );
    }
}
