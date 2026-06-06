//! `get_symbol` — return a named symbol's source plus call-graph context.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ast::kg::SymbolGraph;
use crate::ast::symbol::list_symbols;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `get_symbol` — return a named symbol's source plus call-graph context.
pub struct GetSymbolTool;

#[async_trait]
impl ToolExecutor for GetSymbolTool {
    fn name(&self) -> &str {
        "get_symbol"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "get_symbol",
                "description": "Locate a named symbol (function/struct/class/etc.) in a source file and return its source code along with callers/callees from the file's symbol graph. Use this before editing to understand a symbol's role.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string", "description": "Path to a source file (.rs/.py/.js/.go)."},
                        "name": {"type": "string", "description": "Exact symbol name (case-sensitive)."}
                    },
                    "required": ["file", "name"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("get_symbol: missing 'file'");
        };
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("get_symbol: missing 'name'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "lookup".into(),
            detail: format!("symbol `{name}` in {file}"),
        });
        let path = PathBuf::from(file);

        // #347 follow-up: Consult the pre-indexed registry first.
        //
        // Why: Workflow runs over existing codebases pre-populate a global
        // SymbolRegistry before the phase loop starts. Hitting that cache
        // avoids a fresh disk read + tree-sitter parse on every `get_symbol`
        // call against an already-known file.
        // What: When the registry is installed AND it has at least one entry
        // tagged with this file via `assigned_file`, build a JSON response
        // from the registry entry directly (line numbers default to 0 — the
        // registry is line-agnostic; callers that need ranges should call a
        // future `get_symbol_lines` helper). Fall back to on-demand parse
        // when the file isn't in the index (newly created during the run).
        // Test: `get_symbol_uses_pre_indexed_registry` below.
        if let Some(registry_arc) = crate::ast::get_pre_indexed_registry()
            && let Ok(registry) = registry_arc.read()
        {
            let mut hit: Option<&crate::ast::SymbolEntry> = None;
            for (id, entry) in registry.iter() {
                if entry.assigned_file.as_deref() == Some(path.as_path())
                    && (id.as_str() == name || id.as_str().ends_with(&format!("::{name}")))
                {
                    hit = Some(entry);
                    break;
                }
            }
            if let Some(entry) = hit {
                let out = json!({
                    "name": name,
                    "kind": entry.kind,
                    "file": file,
                    "start_line": 0,
                    "end_line": 0,
                    "source": entry.source,
                    "callers": [],
                    "callees": [],
                    "source_of_truth": "pre_indexed_registry",
                });
                return ToolResult::ok(out.to_string());
            }
        }

        // Fall back to on-demand parse (for files created during the run, or
        // when no pre-index was performed).
        let symbols = match list_symbols(&path) {
            Ok(s) => s,
            Err(e) => return ToolResult::err(format!("get_symbol: {e}")),
        };
        let Some(sym) = symbols.into_iter().find(|s| s.name == name) else {
            return ToolResult::err(format!("get_symbol: '{name}' not found in {file}"));
        };

        // KG context. Failure to build the graph is non-fatal — we still
        // return the symbol so the LLM has something to work with.
        let (callers, callees) = match SymbolGraph::build_from_file(&path) {
            Ok(g) => {
                let callers: Vec<Value> = g
                    .callers_of(name)
                    .into_iter()
                    .map(|n| json!({"name": n.name, "kind": n.kind, "start_line": n.start_line}))
                    .collect();
                let callees: Vec<Value> = g
                    .callees_of(name)
                    .into_iter()
                    .map(|n| json!({"name": n.name, "kind": n.kind, "start_line": n.start_line}))
                    .collect();
                (callers, callees)
            }
            Err(_) => (Vec::new(), Vec::new()),
        };

        let out = json!({
            "name": sym.name,
            "kind": sym.kind,
            "file": file,
            "start_line": sym.start_line,
            "end_line": sym.end_line,
            "source": sym.source,
            "callers": callers,
            "callees": callees,
        });
        ToolResult::ok(out.to_string())
    }
}
