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
//! Node builders live in `nodes`, call/import extraction in `calls`, and
//! tests in `tests` so this facade stays under the 500-SLOC cap (see #1195).
//!
//! Test: see the `tests` module — covers function, method, struct, imports,
//! call edges (with weights and caller scoping), and doc comments.

use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::{Node, Parser};

mod calls;
mod nodes;

#[cfg(test)]
mod tests;

use calls::{extract_calls, extract_import_specs};
use nodes::{
    file_node, is_test_function, make_method_node, make_node, name_of, preceding_doc, receiver_type,
};

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
            for (path, spec_node) in extract_import_specs(node, src) {
                let n = make_node(KgNodeKind::Import, &path, chunk, spec_node, None);
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
        _ => {}
    }
}
