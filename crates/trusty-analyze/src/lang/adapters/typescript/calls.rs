//! Intra-function call extraction for the TypeScript/JSX adapter.
//!
//! Why: a function's outgoing call graph is one of the most useful pieces of
//! static analysis we can derive cheaply and is required for graph traversal
//! queries ("what calls auth?"). Lifting it out of the walker keeps `mod.rs`
//! under the 500-SLOC cap (see #1195).
//! What: walks a function/method body, collects direct `call_expression`
//! nodes, and returns deduplicated `Calls` edges keyed by callee name.
//! Test: exercised via the adapter tests in `super`.

use super::nodes::node_text;
use crate::lang::call_target::build_call_target;
use crate::types::{KgEdge, KgEdgeKind};
use tree_sitter::Node;

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
pub(crate) fn extract_calls(
    body: Node,
    src: &[u8],
    caller_id: &str,
    language: &str,
) -> Vec<KgEdge> {
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
