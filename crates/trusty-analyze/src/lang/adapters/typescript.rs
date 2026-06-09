//! TypeScript `LanguageAnalyzer` adapter backed by tree-sitter-typescript.
//!
//! Why: Extracts functions, classes, interfaces, imports/exports and call
//! expressions from TypeScript and TSX source.
//!
//! What: For each chunk, parses with `tree_sitter_typescript::LANGUAGE_TSX`
//! (which is a superset that also accepts plain `.ts`), walks the tree
//! once, and emits nodes/edges into a shared `KgGraph`.
//!
//! Test: `ts_analyzer_extracts_function` parses `function hello() {}` and
//! asserts the Function node is produced.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

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

fn name_of(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

fn make_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node, language: &str) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    KgNode {
        id: format!("{language}:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: language.to_string(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: false,
        extra: serde_json::Value::Null,
    }
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

/// Extract `call_expression` nodes from a function/method body and produce
/// deduplicated `Calls` edges keyed by callee name.
///
/// Why: A function's outgoing call graph is one of the most useful pieces of
/// static analysis we can derive cheaply and is required for graph traversal
/// queries ("what calls auth?"). Without scoped extraction, every call site is
/// emitted as an orphan edge with no caller, defeating the purpose.
///
/// What: Walks the AST subtree rooted at `body`, collects every direct
/// `call_expression` (skipping nested function/method/arrow/class bodies so
/// each function only emits its own direct calls), resolves the callee name
/// from the `function` field, counts repeats, and returns one `KgEdge` per
/// unique callee with `weight = call_count as f32`.
///
/// Test: `ts_adapter_extracts_call_edges` and
/// `ts_adapter_deduplicates_repeated_calls` cover the happy paths.
fn extract_calls(body: Node, src: &[u8], caller_id: &str, language: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        // Stop at nested function-like / class bodies so each function only
        // attributes its own direct calls.
        match node.kind() {
            "function_declaration"
            | "function"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "class_declaration"
            | "class" => {
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
            to: build_call_target(language, "Function", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a JS/TS `call_expression` node.
///
/// Why: Cross-file resolution is out of scope for the adapter (the linker
/// merges by qualified_name), so we only need a stable string handle for the
/// callee — bare identifier or member-expression property name.
///
/// What: Inspects the `function` field. Returns the bare text for
/// `identifier`, the property name for `member_expression`
/// (`a.b.c()` → `c`), or `None` for unsupported forms (e.g. dynamic
/// `obj[expr]()` calls).
///
/// Test: Exercised indirectly by the `extract_calls` tests.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let fun = call.child_by_field_name("function")?;
    match fun.kind() {
        "identifier" => Some(node_text(fun, src)),
        "member_expression" => fun
            .child_by_field_name("property")
            .map(|p| node_text(p, src)),
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
    fn ts_analyzer_extracts_function() {
        let a = TypeScriptAnalyzer::new();
        let c = make_chunk("function hello() { return 1; }\n", "f.ts");
        let r = a.analyze_chunks(&[c]);
        assert_eq!(r.analyzed_chunks, 1);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "hello");
        assert_eq!(funcs[0].language, "typescript");
    }

    #[test]
    fn ts_analyzer_extracts_class_and_interface() {
        let a = TypeScriptAnalyzer::new();
        let c = make_chunk(
            "interface Foo { x: number }\n\
             class Bar implements Foo { x = 1; }\n",
            "f.ts",
        );
        let r = a.analyze_chunks(&[c]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Bar"));
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Interface) && n.name == "Foo"));
        assert!(r
            .graph
            .edges
            .iter()
            .any(|e| matches!(e.kind, KgEdgeKind::Implements)));
    }

    #[test]
    fn supports_dot_ts_and_tsx() {
        let a = TypeScriptAnalyzer::new();
        assert!(a.supports("App.tsx"));
        assert!(a.supports("foo.ts"));
        assert!(!a.supports("foo.js"));
    }

    #[test]
    fn ts_adapter_extracts_arrow_function_assigned_to_variable() {
        let a = TypeScriptAnalyzer::new();
        let c = make_chunk("const greet = (n: string) => `hi ${n}`;\n", "f.ts");
        let r = a.analyze_chunks(&[c]);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function) && n.name == "greet")
            .collect();
        assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph);
        assert_eq!(funcs[0].language, "typescript");
    }

    #[test]
    fn ts_adapter_extracts_call_edges() {
        let src =
            "function caller() {\n    helper();\n    obj.method();\n}\nfunction helper() {}\n";
        let c = make_chunk(src, "test.ts");
        let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
        let calls: Vec<_> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        assert!(
            !calls.is_empty(),
            "expected at least one Calls edge, got none"
        );
        let has_helper = calls.iter().any(|e| e.to.contains("helper"));
        let has_method = calls.iter().any(|e| e.to.contains("method"));
        assert!(has_helper, "expected edge to 'helper', got {calls:?}");
        assert!(has_method, "expected edge to 'method', got {calls:?}");
        // Caller must be scoped to the function, not the file.
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Function:") || e.from.contains(":Method:")),
            "Calls edges should originate from a function/method node, got {calls:?}"
        );
    }

    #[test]
    fn walk_declarations_emits_method_node_via_class_body() {
        // Methods are handled by walk_declarations when the recursion descends
        // into the class body. Hitting this path validates the dispatcher
        // routes `method_definition` correctly after refactoring.
        let src = "class Greeter { hello() { return 1; } }\n";
        let c = make_chunk(src, "g.ts");
        let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method) && n.name == "hello")
            .collect();
        assert_eq!(
            methods.len(),
            1,
            "expected one Method node, got: {:?}",
            r.graph.nodes
        );
    }

    #[test]
    fn walk_imports_emits_import_and_export_nodes() {
        // Validates that walk_imports (the extracted sub-walker) produces both
        // Import and Export nodes after refactoring — same input, same output.
        let src = "import { x } from 'mod';\nexport const y = 1;\n";
        let c = make_chunk(src, "f.ts");
        let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
        let has_import = r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Import));
        let has_export = r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Export));
        assert!(has_import, "expected Import node, got: {:?}", r.graph.nodes);
        assert!(has_export, "expected Export node, got: {:?}", r.graph.nodes);
        assert!(r
            .graph
            .edges
            .iter()
            .any(|e| matches!(e.kind, KgEdgeKind::Imports)));
        assert!(r
            .graph
            .edges
            .iter()
            .any(|e| matches!(e.kind, KgEdgeKind::Exports)));
    }

    #[test]
    fn ts_adapter_deduplicates_repeated_calls() {
        let src = "function foo() {\n    bar();\n    bar();\n    bar();\n}\nfunction bar() {}\n";
        let c = make_chunk(src, "test.ts");
        let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
        let bar_edges: Vec<_> = r
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
}
