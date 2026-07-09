//! TypeScript `LanguageAnalyzer` adapter backed by tree-sitter-typescript.
//!
//! Why: Extracts functions, classes, interfaces, imports/exports and call
//! expressions from TypeScript and TSX source.
//!
//! What: For each chunk, parses with `tree_sitter_typescript::LANGUAGE_TSX`
//! (which is a superset that also accepts plain `.ts`), walks the tree
//! once, and emits nodes/edges into a shared `KgGraph`. Node builders live in
//! `nodes`, call extraction in `calls`, and tests in `tests` so this facade
//! stays under the 500-SLOC cap (see #1195).
//!
//! Test: `ts_analyzer_extracts_function` parses `function hello() {}` and
//! asserts the Function node is produced (see the `tests` module).

use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::{Node, Parser};

mod calls;
mod nodes;

#[cfg(test)]
mod tests;

use calls::extract_calls;
use nodes::{file_node, make_node, name_of, node_text};

/// tree-sitter-typescript-backed analyzer (also handles TSX).
pub struct TypeScriptAnalyzer;

impl TypeScriptAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TypeScriptAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for TypeScriptAnalyzer {
    fn language(&self) -> &str {
        "typescript"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".ts", ".tsx"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        analyze_with_grammar(chunks, "typescript", true)
    }
}

/// Shared implementation: parse with TS or JS grammar, walk, emit.
pub(crate) fn analyze_with_grammar(
    chunks: &[CodeChunk],
    language_tag: &str,
    is_typescript: bool,
) -> StaticAnalysisResult {
    let mut parser = Parser::new();
    let lang = if is_typescript {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_javascript::LANGUAGE.into()
    };
    if parser.set_language(&lang).is_err() {
        return StaticAnalysisResult {
            errors: vec![format!("failed to load grammar for {language_tag}")],
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
            result
                .graph
                .nodes
                .push(file_node(&chunk.file, language_tag));
        }

        walk_ts_like(
            tree.root_node(),
            chunk.content.as_bytes(),
            chunk,
            language_tag,
            &mut result.graph,
        );
    }

    result
}

/// Top-level dispatcher: routes each AST node to the matching sub-walker
/// (declarations vs. imports/exports), then recurses into children.
///
/// Why: A single 200-line walker hid which node kinds produced which graph
/// effects. Splitting by concern caps per-walker complexity and makes each
/// piece independently testable.
/// What: For each node, calls `walk_declarations` (functions/classes/methods/
/// interfaces/arrow-fn vars) and `walk_imports` (import/export statements),
/// then recurses into children.
/// Test: All existing `ts_*` tests continue to pass; new tests
/// `walk_declarations_emits_method_node` and `walk_imports_emits_import_node`
/// exercise the sub-walkers in isolation through a public chunk.
fn walk_ts_like(node: Node, src: &[u8], chunk: &CodeChunk, language: &str, graph: &mut KgGraph) {
    let file_id = format!("{language}:File:{}", chunk.file);
    walk_node(node, src, chunk, language, graph, &file_id);
}

fn walk_node(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    language: &str,
    graph: &mut KgGraph,
    file_id: &str,
) {
    walk_declarations(node, src, chunk, language, graph, file_id);
    walk_imports(node, src, chunk, language, graph, file_id);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(child, src, chunk, language, graph, file_id);
    }
}

/// Emit nodes/edges for declaration-shaped AST nodes:
/// functions, methods, classes (with extends/implements), interfaces, and
/// arrow/function expressions assigned to variables.
fn walk_declarations(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    language: &str,
    graph: &mut KgGraph,
    file_id: &str,
) {
    match node.kind() {
        "function_declaration" | "function" => {
            emit_named_callable(
                node,
                src,
                chunk,
                language,
                graph,
                file_id,
                KgNodeKind::Function,
            );
        }
        "method_definition" => {
            emit_named_callable(
                node,
                src,
                chunk,
                language,
                graph,
                file_id,
                KgNodeKind::Method,
            );
        }
        "lexical_declaration" | "variable_declaration" => {
            emit_arrow_var_declarators(node, src, chunk, language, graph, file_id);
        }
        "class_declaration" => {
            emit_class_declaration(node, src, chunk, language, graph, file_id);
        }
        "interface_declaration" => {
            if let Some(name) = name_of(node, src) {
                let n = make_node(KgNodeKind::Interface, &name, chunk, node, language);
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
        _ => {}
    }
}

/// Emit nodes/edges for module-boundary AST nodes (`import` / `export`).
fn walk_imports(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    language: &str,
    graph: &mut KgGraph,
    file_id: &str,
) {
    let (kind, edge_kind) = match node.kind() {
        "import_statement" => (KgNodeKind::Import, KgEdgeKind::Imports),
        "export_statement" => (KgNodeKind::Export, KgEdgeKind::Exports),
        _ => return,
    };
    let txt = node_text(node, src);
    let cleaned = txt.trim().trim_end_matches(';').to_string();
    if cleaned.is_empty() {
        return;
    }
    let n = make_node(kind, &cleaned, chunk, node, language);
    let id = n.id.clone();
    graph.nodes.push(n);
    graph.edges.push(KgEdge {
        from: file_id.to_string(),
        to: id,
        kind: edge_kind,
        weight: 1.0,
    });
}

/// Emit a Function or Method node for a named callable (`function foo() {}`
/// or `foo() {}` in a class body), wire its `Contains` edge from the file,
/// and attach call edges from its body.
fn emit_named_callable(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    language: &str,
    graph: &mut KgGraph,
    file_id: &str,
    kind: KgNodeKind,
) {
    let Some(name) = name_of(node, src) else {
        return;
    };
    let n = make_node(kind, &name, chunk, node, language);
    let id = n.id.clone();
    graph.nodes.push(n);
    graph.edges.push(KgEdge {
        from: file_id.to_string(),
        to: id.clone(),
        kind: KgEdgeKind::Contains,
        weight: 1.0,
    });
    if let Some(body) = node.child_by_field_name("body") {
        for edge in extract_calls(body, src, &id, language) {
            graph.edges.push(edge);
        }
    }
}

/// Handle `const/let/var foo = () => {...}` and `var foo = function () {...}`.
fn emit_arrow_var_declarators(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    language: &str,
    graph: &mut KgGraph,
    file_id: &str,
) {
    let mut cursor = node.walk();
    for decl in node.children(&mut cursor) {
        if decl.kind() != "variable_declarator" {
            continue;
        }
        let (Some(nm), Some(val)) = (
            decl.child_by_field_name("name"),
            decl.child_by_field_name("value"),
        ) else {
            continue;
        };
        if !matches!(
            val.kind(),
            "arrow_function" | "function" | "function_expression"
        ) || nm.kind() != "identifier"
        {
            continue;
        }
        let name = node_text(nm, src);
        let n = make_node(KgNodeKind::Function, &name, chunk, decl, language);
        let id = n.id.clone();
        graph.nodes.push(n);
        graph.edges.push(KgEdge {
            from: file_id.to_string(),
            to: id.clone(),
            kind: KgEdgeKind::Contains,
            weight: 1.0,
        });
        if let Some(body) = val.child_by_field_name("body") {
            for edge in extract_calls(body, src, &id, language) {
                graph.edges.push(edge);
            }
        }
    }
}

/// Emit Class node + Contains edge, then walk the heritage clause for
/// Extends / Implements edges.
fn emit_class_declaration(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    language: &str,
    graph: &mut KgGraph,
    file_id: &str,
) {
    let Some(name) = name_of(node, src) else {
        return;
    };
    let n = make_node(KgNodeKind::Class, &name, chunk, node, language);
    let id = n.id.clone();
    graph.nodes.push(n);
    graph.edges.push(KgEdge {
        from: file_id.to_string(),
        to: id.clone(),
        kind: KgEdgeKind::Contains,
        weight: 1.0,
    });
    emit_class_heritage(node, src, chunk, language, graph, &id);
}

fn emit_class_heritage(
    class_node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    language: &str,
    graph: &mut KgGraph,
    class_id: &str,
) {
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        if child.kind() != "class_heritage" {
            continue;
        }
        let mut c2 = child.walk();
        for h in child.children(&mut c2) {
            let (target_kind, edge_kind) = match h.kind() {
                "extends_clause" => ("Class", KgEdgeKind::Extends),
                "implements_clause" => ("Interface", KgEdgeKind::Implements),
                _ => continue,
            };
            let mut inner_cursor = h.walk();
            for inner in h.children(&mut inner_cursor) {
                if !matches!(inner.kind(), "identifier" | "type_identifier") {
                    continue;
                }
                let target = node_text(inner, src);
                let to_id = format!("{language}:{target_kind}:{}:{target}", chunk.file);
                graph.edges.push(KgEdge {
                    from: class_id.to_string(),
                    to: to_id,
                    kind: edge_kind.clone(),
                    weight: 1.0,
                });
            }
        }
    }
}
