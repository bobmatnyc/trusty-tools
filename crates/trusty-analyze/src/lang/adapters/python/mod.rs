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
//! Node builders live in `nodes`, import/call extraction in `calls`, and
//! tests in `tests` so this facade stays under the 500-SLOC cap (see #1195).
//!
//! Test: `python_extracts_function` and `python_extracts_class` cover the
//! basic happy paths; `python_adapter_extracts_call_edges` and
//! `python_adapter_deduplicates_repeated_calls` cover call-edge extraction.

use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::{Node, Parser};

mod calls;
mod nodes;

#[cfg(test)]
mod tests;

use calls::{emit_imports, extract_calls};
use nodes::{file_node, make_method_node, make_node, name_of, node_text};

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
