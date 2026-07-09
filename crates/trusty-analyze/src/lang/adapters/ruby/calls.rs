//! Ruby call-edge and require-import extraction for the KG adapter.
//!
//! Why: per-method call graphs and require handling are a self-contained
//! concern lifted out of the walker so `mod.rs` stays under the 500-SLOC cap
//! (see #1195).
//! What: predicates + emitters producing `Calls`/`Imports` edges and `Import`
//! nodes.
//! Test: exercised end-to-end via the adapter tests in `super`.

use super::nodes::{make_simple_node, node_text};
use crate::lang::call_target::build_call_target;
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::Node;

/// True if this `call` node is a `require` / `require_relative` invocation.
/// We treat these as imports rather than as call edges.
pub(crate) fn is_require_call(call: Node, src: &[u8]) -> bool {
    let Some(method) = call.child_by_field_name("method") else {
        return false;
    };
    if method.kind() != "identifier" {
        return false;
    }
    let name = node_text(method, src);
    name == "require" || name == "require_relative"
}

/// Names that look like declarative DSL (`attr_reader`, etc.) and shouldn't
/// be treated as outgoing call edges.
pub(crate) fn is_declarative_call(name: &str) -> bool {
    matches!(
        name,
        "require"
            | "require_relative"
            | "attr_reader"
            | "attr_writer"
            | "attr_accessor"
            | "private"
            | "public"
            | "protected"
            | "include"
            | "extend"
            | "prepend"
    )
}

/// Emit one `Import` node + `Imports` edge for a `require` / `require_relative`
/// call.
///
/// Why: Ruby doesn't have dedicated import syntax; requires are parsed as
/// regular method calls. We pull them out so the dependency graph reflects
/// file-level loading just like Python's `import`.
/// What: Looks at the first string argument and emits one node per require.
/// Falls back silently if the argument shape is unexpected (e.g. dynamic
/// `require some_var`).
/// Test: `ruby_extracts_require_imports`.
pub(crate) fn emit_require(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
) {
    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() != "string" {
            continue;
        }
        // Find the inner string_content if present, otherwise strip quotes.
        let mut content_cursor = child.walk();
        let mut target: Option<String> = None;
        for inner in child.children(&mut content_cursor) {
            if inner.kind() == "string_content" {
                target = Some(node_text(inner, src));
                break;
            }
        }
        let target = target.unwrap_or_else(|| {
            let raw = node_text(child, src);
            raw.trim_matches(|c| c == '"' || c == '\'').to_string()
        });
        if target.is_empty() {
            continue;
        }
        let n = make_simple_node(KgNodeKind::Import, &target, chunk, node);
        let id = n.id.clone();
        graph.nodes.push(n);
        graph.edges.push(KgEdge {
            from: parent_id.to_string(),
            to: id,
            kind: KgEdgeKind::Imports,
            weight: 1.0,
        });
        // Only emit one import per require call (require takes one arg).
        break;
    }
}

/// Extract `call` expression nodes from a method body and produce
/// deduplicated `Calls` edges keyed by callee name.
///
/// Why: Per-method outgoing call graphs are the cheapest behavioral signal we
/// can emit; unique-per-callee deduplication with `weight = count` keeps the
/// graph compact while preserving frequency information.
/// What: Walks the AST subtree rooted at `body`, collects every direct `call`
/// (skipping nested `method` / `singleton_method` / `class` / `module` /
/// `do_block` / `block` bodies so each method only attributes its own direct
/// calls), resolves the callee name, skips declarative DSL like `attr_reader`,
/// counts repeats, and returns one `KgEdge` per unique callee with
/// `weight = call_count as f32`.
/// Test: `ruby_adapter_extracts_call_edges`,
/// `ruby_adapter_deduplicates_repeated_calls`.
pub(crate) fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        match node.kind() {
            // Stop at nested function-like bodies so each method only
            // attributes its own direct calls.
            "method" | "singleton_method" | "class" | "module" | "do_block" | "block"
            | "lambda" => {
                return;
            }
            "call" => {
                if let Some(callee) = callee_name(node, src) {
                    if !is_declarative_call(&callee) && callee != "self" {
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
            to: build_call_target("ruby", "Method", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a Ruby `call` node.
///
/// Why: Cross-file resolution is out of scope for the adapter (the linker
/// merges by qualified_name later). We only need a stable string handle.
/// What: Inspects the `method` field. Returns the bare text for `identifier`,
/// `constant`, or `operator`; returns `None` for unsupported forms.
/// Test: Exercised indirectly by `ruby_adapter_extracts_call_edges`.
pub(crate) fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let m = call.child_by_field_name("method")?;
    match m.kind() {
        "identifier" | "constant" | "operator" => Some(node_text(m, src)),
        _ => None,
    }
}
