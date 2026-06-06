//! `trace_execution_flow` — BFS the call graph from an entry point (#373).
//!
//! Why: Lets the LLM understand "what does this function ultimately call?"
//! or "who reaches this function?" without manually grepping every file.
//! What: Builds a `SymbolGraph` (from the pre-indexed registry if present,
//! else falls back to walking the project root), then BFSes
//! callers/callees up to `max_depth`.
//! Test: `trace_flow_outgoing_smoke`.

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::analyze_project::collect_source_files;
use crate::tools::traits::{ToolExecutor, ToolResult};

use trusty_common::symgraph::graph::{SymbolGraph, SymbolNode};
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
                        "entry_point": {"type": "string", "description": "Bare function name to start from."},
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

        // Find entry node.
        let entry_node = graph.nodes().into_iter().find(|n| n.name == entry).cloned();
        let Some(root_node) = entry_node else {
            return ToolResult::err(format!(
                "trace_execution_flow: '{entry}' not found in symbol graph"
            ));
        };

        let mut total = 0usize;
        let tree = bfs(&graph, &root_node, &direction, max_depth, &mut total);

        let out = json!({
            "entry": entry,
            "direction": direction,
            "call_tree": tree,
            "total_nodes": total
        });
        ToolResult::ok(out.to_string())
    }
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
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(root.name.clone());
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
            if direction == "outgoing" || direction == "both" {
                for c in graph.callees_of(&node.name) {
                    if visited.insert(c.name.clone()) {
                        children.push(c);
                        *total += 1;
                    }
                }
            }
            if direction == "incoming" || direction == "both" {
                for c in graph.callers_of(&node.name) {
                    if visited.insert(c.name.clone()) {
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

    #[tokio::test]
    async fn trace_flow_missing_entry_errors() {
        let t = TraceExecutionFlowTool;
        let r = t
            .execute(json!({"entry_point": "definitely_not_a_real_symbol_xyz_123"}))
            .await;
        assert!(r.is_error());
    }
}
