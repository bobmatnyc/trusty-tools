//! PHP call-edge extraction and `use`/`include`/`require` import emission for
//! the KG adapter.
//!
//! Why: per-function/method call graphs and import handling are a
//! self-contained concern lifted out of the walker so `mod.rs` stays under
//! the 500-SLOC cap (see #1195).
//! What: predicates + emitters producing `Calls`/`Imports` edges and `Import`
//! nodes.
//! Test: exercised end-to-end via the adapter tests in `super`.

use super::nodes::{make_simple_node, node_text};
use crate::lang::call_target::build_call_target;
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::Node;

/// Names that look like language constructs / declarative DSL and shouldn't
/// be treated as outgoing call edges from a function body.
pub(crate) fn is_declarative_call(name: &str) -> bool {
    matches!(
        name,
        "echo"
            | "print"
            | "isset"
            | "empty"
            | "unset"
            | "list"
            | "array"
            | "die"
            | "exit"
            | "include"
            | "require"
            | "include_once"
            | "require_once"
    )
}

/// Emit one `Import` node + `Imports` edge per `namespace_use_clause` inside a
/// `namespace_use_declaration`.
///
/// Why: PHP's `use Foo\Bar;` is the closest analogue to Python's `import` —
/// surfacing it lets the dependency graph show file-level fan-out. We keep the
/// dotted form (`Foo.Bar.Baz`) so it lines up with the convention used by the
/// Python adapter.
/// What: Iterates `namespace_use_clause` children of the declaration; for each
/// clause grabs the inner `name` or `qualified_name` child, replaces `\` with
/// `.`, and emits one node + edge per target. Skips clauses with no resolvable
/// name.
/// Test: `php_extracts_use_imports`.
pub(crate) fn emit_namespace_use(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "namespace_use_clause" {
            continue;
        }
        // The clause holds either a `name` or `qualified_name` child.
        let mut inner_cursor = child.walk();
        let mut target: Option<String> = None;
        for inner in child.children(&mut inner_cursor) {
            match inner.kind() {
                "qualified_name" | "name" => {
                    let raw = node_text(inner, src);
                    // Normalize `\Foo\Bar` and `Foo\Bar` to `Foo.Bar`.
                    let cleaned = raw.trim_start_matches('\\').replace('\\', ".");
                    if !cleaned.is_empty() {
                        target = Some(cleaned);
                    }
                    break;
                }
                _ => {}
            }
        }
        let Some(target) = target else {
            continue;
        };
        emit_import_node(&target, chunk, node, graph, parent_id);
    }
}

/// Emit an `Import` node + `Imports` edge for an `include`/`require` family
/// expression whose argument is a string literal.
///
/// Why: Although less common in modern code, `require 'config.php'` still
/// drives the dependency graph in many older PHP codebases.
/// What: Inspects the lone expression child; if it is a `string` literal,
/// extracts its `string_content` and emits one node + edge. Variable arguments
/// (`require $path`) are silently skipped.
/// Test: indirectly via `php_extracts_use_imports` (smoke); behavior verified
/// by the parse path itself.
pub(crate) fn emit_include_like(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Drill through the `expression` wrapper.
        let candidate = if child.kind() == "expression" {
            child.named_child(0).unwrap_or(child)
        } else {
            child
        };
        if candidate.kind() != "string" {
            continue;
        }
        let mut inner_cursor = candidate.walk();
        let mut target: Option<String> = None;
        for inner in candidate.children(&mut inner_cursor) {
            if inner.kind() == "string_content" {
                target = Some(node_text(inner, src));
                break;
            }
        }
        let target = target.unwrap_or_else(|| {
            node_text(candidate, src)
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string()
        });
        if target.is_empty() {
            continue;
        }
        emit_import_node(&target, chunk, node, graph, parent_id);
        break;
    }
}

fn emit_import_node(
    target: &str,
    chunk: &CodeChunk,
    ast: Node,
    graph: &mut KgGraph,
    parent_id: &str,
) {
    let n = make_simple_node(KgNodeKind::Import, target, chunk, ast);
    let id = n.id.clone();
    graph.nodes.push(n);
    graph.edges.push(KgEdge {
        from: parent_id.to_string(),
        to: id,
        kind: KgEdgeKind::Imports,
        weight: 1.0,
    });
}

/// Extract call expressions from a function/method body and produce
/// deduplicated `Calls` edges keyed by callee name.
///
/// Why: Per-caller outgoing call graphs are the cheapest behavioral signal we
/// can emit; counting unique callees with `weight = count` keeps the graph
/// compact while preserving frequency information.
/// What: Walks the AST subtree rooted at `body`, collects `function_call_expression`,
/// `member_call_expression`, `nullsafe_member_call_expression`, and
/// `scoped_call_expression` nodes. Skips into nested `function_definition`,
/// `method_declaration`, `class_declaration`, `interface_declaration`,
/// `trait_declaration`, `anonymous_function`, and `arrow_function` so each
/// caller only attributes its own direct calls. Skips PHP language constructs
/// (echo/print/isset/etc.) and emits one `KgEdge` per unique callee with
/// `weight = call_count as f32`.
/// Test: `php_adapter_extracts_call_edges`,
/// `php_adapter_deduplicates_repeated_calls`,
/// `php_method_call_edges_scoped_to_method`.
pub(crate) fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        match node.kind() {
            // Stop at nested function-like bodies so each caller only
            // attributes its own direct calls.
            "function_definition"
            | "method_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "anonymous_function"
            | "anonymous_function_creation_expression"
            | "arrow_function" => {
                return;
            }
            "function_call_expression"
            | "member_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression" => {
                if let Some(callee) = callee_name(node, src) {
                    if !is_declarative_call(&callee) {
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
            to: build_call_target("php", "Method", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a PHP call node.
///
/// Why: Cross-file symbol resolution is out of scope for the adapter; the
/// cross-chunk linker merges by qualified_name later. We just need a stable
/// string handle per call site.
/// What: For `function_call_expression` reads the `function` field — bare
/// `name` returns its text, `qualified_name` returns the trailing segment.
/// For `member_call_expression` / `nullsafe_member_call_expression` /
/// `scoped_call_expression` reads the `name` field. Returns `None` for
/// dynamic / variable callees we can't resolve.
/// Test: exercised indirectly by the call-edge tests.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    match call.kind() {
        "function_call_expression" => {
            let f = call.child_by_field_name("function")?;
            match f.kind() {
                "name" => Some(node_text(f, src)),
                "qualified_name" => {
                    let raw = node_text(f, src);
                    raw.rsplit('\\').next().map(|s| s.to_string())
                }
                _ => None,
            }
        }
        "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" => {
            let n = call.child_by_field_name("name")?;
            if n.kind() == "name" {
                Some(node_text(n, src))
            } else {
                None
            }
        }
        _ => None,
    }
}
