//! Import extraction and intra-function call extraction for the Python
//! adapter.
//!
//! Why: import/call-edge extraction is a self-contained concern lifted out of
//! the walker so `mod.rs` stays under the 500-SLOC cap (see #1195).
//! What: emits `Import` nodes + `Imports` edges from `import`/`from import`
//! statements, and walks a function/method body to produce deduplicated
//! `Calls` edges keyed by callee name.
//! Test: exercised via the adapter tests in `super::tests`.

use super::nodes::{make_node, node_text};
use crate::lang::call_target::build_call_target;
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::Node;

/// Emit `Import` nodes + `Imports` edges from a Python import statement.
///
/// Why: Import edges drive the file/module-level dependency graph; one node
/// per imported target gives the graph a clean fan-out instead of a single
/// concatenated string.
/// What: For `import a, b.c` emits one node per dotted name. For
/// `from foo import bar, baz` emits `foo.bar` and `foo.baz`. Falls back to
/// the raw statement text if the AST shape is unexpected.
/// Test: `python_extracts_imports` and `python_extracts_from_imports`.
pub(crate) fn emit_imports(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
) {
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
pub(crate) fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
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
