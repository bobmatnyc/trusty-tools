//! PHP `LanguageAnalyzer` adapter backed by tree-sitter-php.
//!
//! Why: Extracts PHP structure — functions, methods, classes, interfaces,
//! traits, namespace `use`/`require`/`include` imports, and intra-method call
//! edges — into a language-neutral `KgGraph`. Mirrors the Python and Ruby
//! adapters so the analyzer registry behaves uniformly across languages.
//!
//! What: For each `CodeChunk`, parses the content with tree-sitter-php,
//! walks the tree, and emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for top-level `function_definition`
//! - `Method` nodes for `method_declaration` inside a class/interface/trait
//!   with class-qualified IDs `php:Method:file:Class:name`
//! - `Class` nodes for `class_declaration` and `trait_declaration`
//!   (traits map to `Class` — closest semantic match)
//! - `Interface` nodes for `interface_declaration`
//! - `Import` nodes + `Imports` edges for `namespace_use_declaration`
//!   (`use Foo\Bar;`) and `include`/`require` (and `_once` variants) when the
//!   argument is a string literal
//! - `Calls` edges from each function/method to its callees, scoped to the
//!   enclosing function/method, deduplicated with `weight = call_count`
//!
//! Node/qualified-id construction lives in `nodes`, call-edge and import
//! extraction in `calls`, and tests in `tests` so this facade stays under
//! the 500-SLOC cap (see #1195).
//!
//! Test: see the `tests` module — covers detection, methods (class-qualified
//! IDs), interface/trait emission, call edges (scoped + deduped), and `use`
//! imports.

use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::{Node, Parser};

mod calls;
mod nodes;

#[cfg(test)]
mod tests;

use calls::{emit_include_like, emit_namespace_use, extract_calls};
use nodes::{file_node, make_method_node, make_simple_node, name_of, node_text};

/// tree-sitter-php-backed analyzer.
pub struct PhpAnalyzer;

impl PhpAnalyzer {
    /// Construct a stateless analyzer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PhpAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for PhpAnalyzer {
    fn language(&self) -> &str {
        "php"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".php"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-php grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "php analyze chunk");
            // The PHP grammar (LANGUAGE_PHP) requires a `<?php` opener; if the
            // chunk content is missing one (e.g. a stripped fragment), prepend
            // it so the parser doesn't bail to an ERROR root.
            let needs_prefix = !chunk.content.trim_start().starts_with("<?");
            let owned: String;
            let source: &str = if needs_prefix {
                owned = format!("<?php\n{}", chunk.content);
                &owned
            } else {
                &chunk.content
            };
            let Some(tree) = parser.parse(source, None) else {
                result.errors.push(format!("parse failure: {}", chunk.file));
                continue;
            };
            result.analyzed_chunks += 1;
            if seen_files.insert(chunk.file.clone()) {
                result.analyzed_files += 1;
                result.graph.nodes.push(file_node(&chunk.file));
            }

            let src = source.as_bytes();
            let file_id = format!("php:File:{}", chunk.file);
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

/// Walk the PHP AST emitting nodes/edges, keeping track of the enclosing
/// container (file/class/interface/trait).
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
            if let Some(name) = name_of(node, src) {
                // Top-level function — emit as Function. Method decls live
                // under `declaration_list` and are handled in the class arm.
                let n = make_simple_node(KgNodeKind::Function, &name, chunk, node);
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
        "method_declaration" => {
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
        "class_declaration" | "trait_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
                // Traits map to `Class` — closest semantic match in our
                // language-neutral schema; PHP traits are mixin-like classes.
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
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
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
        "namespace_use_declaration" => {
            emit_namespace_use(node, src, chunk, graph, parent_id);
            return;
        }
        "include_expression"
        | "require_expression"
        | "include_once_expression"
        | "require_once_expression" => {
            emit_include_like(node, src, chunk, graph, parent_id);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        recurse(child, src, chunk, graph, parent_id, class_name);
    }
}
