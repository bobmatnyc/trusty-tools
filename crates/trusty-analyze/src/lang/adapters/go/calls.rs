//! Intra-function call and import extraction for the Go adapter.
//!
//! Why: a function's outgoing call graph is one of the most useful pieces of
//! static analysis we can derive cheaply and is required for graph traversal
//! queries ("what calls auth?"); import-path extraction is the same shape of
//! problem (walk a subtree, collect strings). Lifting both out of the walker
//! keeps `mod.rs` under the 500-SLOC cap (see #1195).
//! What: walks a function/method body, collects direct `call_expression`
//! nodes, and returns deduplicated `Calls` edges keyed by callee name; walks
//! an `import_declaration` subtree and returns unquoted import paths paired
//! with their originating `import_spec` node (for line-number attribution).
//! Test: exercised via the adapter tests in `super::tests`.

use super::nodes::node_text;
use crate::lang::call_target::build_call_target;
use crate::types::{KgEdge, KgEdgeKind};
use tree_sitter::Node;

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
pub(crate) fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
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

/// Strip surrounding quotes from a Go interpreted string literal so import
/// targets are clean (`"fmt"` → `fmt`).
pub(crate) fn unquote_import(s: &str) -> String {
    let trimmed = s.trim();
    trimmed
        .trim_start_matches('"')
        .trim_end_matches('"')
        .to_string()
}

/// Extract unquoted import paths (paired with their originating
/// `import_spec` node, for line-number attribution) from an
/// `import_declaration` subtree.
///
/// Why: An `import_declaration` may wrap a single `import_spec` or an
/// `import_spec_list` of many; callers just want the flat list of clean
/// import paths plus the AST node each came from so the emitted `KgNode`
/// carries accurate line numbers.
///
/// What: Depth-first (stack-based) descent that stops recursing once an
/// `import_spec` is found, preferring its `path` field
/// (`interpreted_string_literal`) and falling back to the spec's own text.
/// Empty paths (after unquoting) are skipped.
///
/// Test: Exercised via `go_extracts_imports` and `go_extracts_single_import`.
pub(crate) fn extract_import_specs<'a>(node: Node<'a>, src: &[u8]) -> Vec<(String, Node<'a>)> {
    let mut stack = vec![node];
    let mut out = Vec::new();
    while let Some(cur) = stack.pop() {
        if cur.kind() == "import_spec" {
            let raw = cur
                .child_by_field_name("path")
                .map(|p| node_text(p, src))
                .unwrap_or_else(|| node_text(cur, src));
            let unquoted = unquote_import(&raw);
            if !unquoted.is_empty() {
                out.push((unquoted, cur));
            }
            continue;
        }
        let mut c = cur.walk();
        for child in cur.children(&mut c) {
            stack.push(child);
        }
    }
    out
}
