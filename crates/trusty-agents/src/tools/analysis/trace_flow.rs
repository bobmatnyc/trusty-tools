//! `trace_execution_flow` — BFS the call graph from an entry point (#373).
//!
//! Why: Lets the LLM understand "what does this function ultimately call?"
//! or "who reaches this function?" without manually grepping every file.
//! What: Builds a `SymbolGraph` (from the pre-indexed registry if present,
//! else falls back to walking the project root), then BFSes
//! callers/callees up to `max_depth`.
//!
//! Why the anchor is `resolve_symbol` (#6232): this tool used to anchor with
//! `nodes().find(|n| n.name == entry)` — first in insertion order — and then
//! re-query `callees_of`/`callers_of` by BARE name at every step. Two
//! definitions sharing a name meant the reported root file/line could describe
//! one of them while the listed callees came from the other, and a
//! `<path>::<symbol>` entry point missed outright. It now anchors through
//! `SymbolGraph::resolve_symbol` (which #6229 taught to answer `Unique` for a
//! path-qualified spelling), reports the alternatives when a name is
//! ambiguous, and traverses by each node's `<file>::<symbol>` key rather than
//! its bare name.
//! Test: `trace_flow_missing_entry_errors`,
//! `same_named_definitions_do_not_mix_in_one_trace`,
//! `path_qualified_entry_point_anchors_on_the_file_it_names`,
//! `an_ambiguous_bare_entry_reports_its_alternatives`.

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::analyze_project::collect_source_files;
use crate::tools::traits::{ToolExecutor, ToolResult};

use trusty_common::symgraph::graph::{SymbolGraph, SymbolMatch, SymbolNode};
use trusty_common::symgraph::registry::SymbolRegistry;

pub struct TraceExecutionFlowTool;

#[async_trait]
impl ToolExecutor for TraceExecutionFlowTool {
    fn name(&self) -> &str {
        "trace_execution_flow"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "trace_execution_flow",
                "description": "BFS the call graph from an entry-point function. Direction = 'outgoing' walks callees; 'incoming' walks callers; 'both' walks both. Returns a tree of (name, file, line, callees) up to max_depth.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "entry_point": {"type": "string", "description": "Function to start from — a bare name, or '<path>::<name>' to pick one of several definitions sharing a name."},
                        "direction": {"type": "string", "description": "'outgoing' | 'incoming' | 'both'. Default 'outgoing'."},
                        "max_depth": {"type": "integer", "description": "Maximum BFS depth. Default 5."}
                    },
                    "required": ["entry_point"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(entry) = args.get("entry_point").and_then(Value::as_str) else {
            return ToolResult::err("trace_execution_flow: missing 'entry_point'");
        };
        let direction = args
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or("outgoing")
            .to_string();
        let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(5) as usize;

        let graph = build_graph();
        match trace(&graph, entry, &direction, max_depth) {
            Ok(out) => ToolResult::ok(out.to_string()),
            Err(message) => ToolResult::err(message),
        }
    }
}

/// One definition, as the tool reports it.
fn describe(node: &SymbolNode) -> Value {
    json!({
        "name": node.name,
        "file": node.file.display().to_string(),
        "line": node.start_line,
    })
}

/// The `<file>::<symbol>` spelling that names ONE definition (#6232).
///
/// Why: `callees_of`/`callers_of` take a name, and a bare name is answered by
/// every definition that shares it — which is how a trace mixed two same-named
/// functions from different files. `resolve_symbol` accepts a `<path>::<symbol>`
/// key and narrows to the definitions in that file, so keying the traversal on
/// it keeps each hop inside the definition it descended from.
/// What: joins the node's own file and name. A node with no assigned file
/// produces `::<name>`, which the resolver falls back to answering by bare name
/// — the pre-#6232 behaviour, and the best available for a node whose file the
/// registry never recorded.
/// Test: `same_named_definitions_do_not_mix_in_one_trace`.
fn node_key(node: &SymbolNode) -> String {
    format!("{}::{}", node.file.display(), node.name)
}

/// Anchor `entry` in `graph` and BFS from it (#6232).
///
/// Why: split out of `execute` so the anchoring and traversal rules can be
/// tested against a hand-built graph holding two same-named definitions — the
/// case the tool got wrong — instead of only against whatever the pre-indexed
/// registry happens to hold.
/// What: resolves `entry` through `SymbolGraph::resolve_symbol`. `NotFound` is
/// the one error. `Ambiguous` is NOT an error: the most-connected definition
/// leads, and every alternative is reported under `ambiguous_with` so the
/// caller can re-ask with a `<path>::<symbol>` spelling. `ambiguous_with` is
/// always present, empty when the name resolved uniquely.
/// Test: `same_named_definitions_do_not_mix_in_one_trace`,
/// `path_qualified_entry_point_anchors_on_the_file_it_names`,
/// `an_ambiguous_bare_entry_reports_its_alternatives`.
fn trace(
    graph: &SymbolGraph,
    entry: &str,
    direction: &str,
    max_depth: usize,
) -> Result<Value, String> {
    let (root, alternatives) = match graph.resolve_symbol(entry) {
        SymbolMatch::NotFound => {
            return Err(format!(
                "trace_execution_flow: '{entry}' not found in symbol graph"
            ));
        }
        SymbolMatch::Unique(node) => (node.clone(), Vec::new()),
        SymbolMatch::Ambiguous {
            chosen,
            alternatives,
        } => (
            chosen.clone(),
            alternatives.iter().map(|n| describe(n)).collect(),
        ),
    };

    let mut total = 0usize;
    let tree = bfs(graph, &root, direction, max_depth, &mut total);

    Ok(json!({
        "entry": entry,
        "direction": direction,
        "resolved_to": describe(&root),
        "ambiguous_with": alternatives,
        "call_tree": tree,
        "total_nodes": total,
    }))
}

/// Build a `SymbolGraph` from the pre-indexed registry if available;
/// otherwise walk the cwd.
fn build_graph() -> SymbolGraph {
    if let Some(reg_arc) = crate::ast::get_pre_indexed_registry()
        && let Ok(reg) = reg_arc.read()
        && !reg.is_empty()
    {
        return SymbolGraph::build_from_registry(&reg);
    }

    // Fallback: walk the project, build a registry on the fly.
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut reg = SymbolRegistry::new(root.clone());
    for path in collect_source_files(&root) {
        if let Ok(entries) = trusty_common::symgraph::parser::parse_file(&path, &root) {
            for mut e in entries {
                e.assigned_file = Some(path.clone());
                reg.insert(e);
            }
        }
    }
    SymbolGraph::build_from_registry(&reg)
}

fn bfs(
    graph: &SymbolGraph,
    root: &SymbolNode,
    direction: &str,
    max_depth: usize,
    total: &mut usize,
) -> Value {
    // #6232: keyed on node identity, not bare name — two same-named
    // definitions are two nodes and must not collapse into one visit.
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(node_key(root));
    *total += 1;

    // We materialize children level by level so we can return a tree directly.
    fn build_subtree(
        graph: &SymbolGraph,
        node: &SymbolNode,
        depth: usize,
        max_depth: usize,
        direction: &str,
        visited: &mut HashSet<String>,
        total: &mut usize,
    ) -> Value {
        let mut children: Vec<&SymbolNode> = Vec::new();
        if depth < max_depth {
            // #6232: query by this node's own qualified key, so the hop stays
            // inside the definition it descended from.
            let key = node_key(node);
            if direction == "outgoing" || direction == "both" {
                for c in graph.callees_of(&key) {
                    if visited.insert(node_key(c)) {
                        children.push(c);
                        *total += 1;
                    }
                }
            }
            if direction == "incoming" || direction == "both" {
                for c in graph.callers_of(&key) {
                    if visited.insert(node_key(c)) {
                        children.push(c);
                        *total += 1;
                    }
                }
            }
        }
        let child_vals: Vec<Value> = children
            .iter()
            .map(|c| build_subtree(graph, c, depth + 1, max_depth, direction, visited, total))
            .collect();
        json!({
            "name": node.name,
            "file": node.file.display().to_string(),
            "line": node.start_line,
            "callees": child_vals,
        })
    }

    build_subtree(graph, root, 0, max_depth, direction, &mut visited, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::symgraph::registry::{SymbolEntry, SymbolId, SymbolKind};

    /// One function definition in module `module`, in `file`, calling `calls`.
    ///
    /// The module path matters: `SymbolRegistry` is keyed by id, so two
    /// definitions sharing a bare name need distinct module paths or the second
    /// silently replaces the first.
    fn entry(module: &str, file: &str, name: &str, calls: &[&str]) -> SymbolEntry {
        let mut e = SymbolEntry::new(
            SymbolId::new(module, name),
            SymbolKind::Function,
            format!("fn {name}() {{}}"),
            "rust",
        );
        e.assigned_file = Some(PathBuf::from(file));
        for callee in calls {
            e.dependencies.insert(SymbolId::new(module, callee));
        }
        e
    }

    /// Two files each defining `helper`, each calling a distinct callee — the
    /// shape #6232 describes.
    ///
    /// `a.rs`'s `helper` is registered FIRST but `b.rs`'s is more connected, so
    /// insertion order and node degree disagree. That disagreement is the
    /// defect: the anchor took the first, the traversal took the most
    /// connected, and the two were different definitions.
    fn two_same_named_helpers() -> SymbolGraph {
        let mut reg = SymbolRegistry::new(PathBuf::from("/proj"));
        reg.insert(entry("a", "/proj/a.rs", "helper", &["only_in_a"]));
        reg.insert(entry("a", "/proj/a.rs", "only_in_a", &[]));
        reg.insert(entry(
            "b",
            "/proj/b.rs",
            "helper",
            &["only_in_b", "also_in_b"],
        ));
        reg.insert(entry("b", "/proj/b.rs", "only_in_b", &[]));
        reg.insert(entry("b", "/proj/b.rs", "also_in_b", &[]));
        reg.insert(entry("b", "/proj/b.rs", "caller_of_b", &["helper"]));
        SymbolGraph::build_from_registry(&reg)
    }

    /// One field of every node in a `call_tree`, root first.
    fn tree_field(tree: &Value, field: &str) -> Vec<String> {
        let mut out = vec![tree[field].as_str().unwrap_or_default().to_string()];
        for child in tree["callees"].as_array().into_iter().flatten() {
            out.extend(tree_field(child, field));
        }
        out
    }

    /// Why (#6232): the reported root belonged to one definition while the
    /// listed callees came from a same-named one elsewhere. A trace anchored on
    /// `a.rs`'s `helper` must never list `b.rs`'s callee.
    /// Test: itself.
    #[test]
    fn same_named_definitions_do_not_mix_in_one_trace() {
        let graph = two_same_named_helpers();
        let out = trace(&graph, "/proj/a.rs::helper", "outgoing", 3).expect("entry resolves");

        assert_eq!(out["resolved_to"]["file"], "/proj/a.rs");
        let names = tree_field(&out["call_tree"], "name");
        assert!(
            names.contains(&"only_in_a".to_string()),
            "a.rs's own callee is missing: {names:?}"
        );
        assert!(
            !names.contains(&"only_in_b".to_string()),
            "b.rs's callee leaked into a.rs's trace: {names:?}"
        );

        // A BARE name must produce one definition's trace end to end too. The
        // anchor used to take insertion order and the traversal node degree, so
        // the root and its callees could come from different files.
        let bare = trace(&graph, "helper", "outgoing", 3).expect("entry resolves");
        let root_file = bare["resolved_to"]["file"].as_str().unwrap_or_default();
        for file in tree_field(&bare["call_tree"], "file") {
            assert_eq!(
                file, root_file,
                "one trace spanned two files: {}",
                bare["call_tree"]
            );
        }
    }

    /// Why (#6232): a `<path>::<symbol>` entry point used to miss outright,
    /// because the anchor compared the whole string against a bare node name.
    /// Test: itself.
    #[test]
    fn path_qualified_entry_point_anchors_on_the_file_it_names() {
        let graph = two_same_named_helpers();
        let out = trace(&graph, "/proj/b.rs::helper", "outgoing", 3).expect("entry resolves");

        assert_eq!(out["resolved_to"]["file"], "/proj/b.rs");
        assert_eq!(
            out["ambiguous_with"].as_array().map(Vec::len),
            Some(0),
            "a path-qualified spelling names one definition"
        );
    }

    /// Why (#6232): a bare name several definitions answer to must say so
    /// rather than silently taking one — that silence is what made the wrong
    /// trace look authoritative.
    /// Test: itself.
    #[test]
    fn an_ambiguous_bare_entry_reports_its_alternatives() {
        let graph = two_same_named_helpers();
        let out = trace(&graph, "helper", "outgoing", 3).expect("entry resolves");

        let alternatives = out["ambiguous_with"]
            .as_array()
            .expect("ambiguous_with is always present");
        assert_eq!(
            alternatives.len(),
            1,
            "the other definition must be reported: {out}"
        );
        let chosen = out["resolved_to"]["file"].as_str().unwrap_or_default();
        let other = alternatives[0]["file"].as_str().unwrap_or_default();
        assert_ne!(chosen, other, "an alternative must be a different file");
    }

    /// Why: an unknown name is the one condition that is still an error.
    /// Test: itself.
    #[test]
    fn trace_reports_an_unknown_entry_as_an_error() {
        let graph = two_same_named_helpers();
        assert!(trace(&graph, "no_such_symbol_xyz", "outgoing", 3).is_err());
    }

    #[tokio::test]
    async fn trace_flow_missing_entry_errors() {
        let t = TraceExecutionFlowTool;
        let r = t
            .execute(json!({"entry_point": "definitely_not_a_real_symbol_xyz_123"}))
            .await;
        assert!(r.is_error());
    }
}
