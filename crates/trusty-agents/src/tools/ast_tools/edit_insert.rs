//! `edit_symbol` and `insert_symbol` — stage pending AST patches.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ast::editor::{insert_after_symbol, replace_symbol};
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::patch_store::{PatchStore, store_patch};

/// `edit_symbol` — splice replacement source into a named symbol's range.
///
/// Why: Pending patches need to land in a store that `apply_patch` can later
/// drain, but the store must not be process-global (see `PatchStore` docs).
/// Holding an `Arc<PatchStore>` lets every tool in a bundle share one store
/// while different bundles (tests, separate agent runs) stay isolated.
/// What: Carries a clone of the bundle's `PatchStore` and writes pending
/// patches into it on each `execute()` call.
/// Test: `edit_then_apply_round_trips`.
pub struct EditSymbolTool {
    patch_store: PatchStore,
}

impl EditSymbolTool {
    /// Build with an explicit `PatchStore` clone.
    ///
    /// Why: Lets bundle constructors (`ast_native_tools`) and tests inject the
    /// shared store explicitly rather than reaching for a global.
    /// What: Stores the clone; `execute()` writes pending patches into it.
    /// Test: Used by every test that constructs `EditSymbolTool`.
    pub fn new(patch_store: PatchStore) -> Self {
        Self { patch_store }
    }
}

#[async_trait]
impl ToolExecutor for EditSymbolTool {
    fn name(&self) -> &str {
        "edit_symbol"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "edit_symbol",
                "description": "Replace a named symbol's full source with `new_source`. Validates syntax and stages the change as a pending patch (returned `patch_id`). Call `apply_patch` to commit. Disk is NOT modified by this call.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "name": {"type": "string", "description": "Symbol name to replace."},
                        "new_source": {"type": "string", "description": "Full replacement source for the symbol."}
                    },
                    "required": ["file", "name", "new_source"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("edit_symbol: missing 'file'");
        };
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("edit_symbol: missing 'name'");
        };
        let Some(new_source) = args.get("new_source").and_then(Value::as_str) else {
            return ToolResult::err("edit_symbol: missing 'new_source'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "edit".into(),
            detail: format!("`{name}` in {file}"),
        });
        match replace_symbol(Path::new(file), name, new_source) {
            Ok(p) => {
                let id = p.id.clone();
                let diff = p.diff.clone();
                store_patch(&self.patch_store, p);
                ToolResult::ok(
                    json!({
                        "patch_id": id,
                        "diff": diff,
                        "status": "pending"
                    })
                    .to_string(),
                )
            }
            Err(e) => ToolResult::err(format!("edit_symbol: {e}")),
        }
    }
}

/// `insert_symbol` — insert a new symbol after an anchor symbol.
///
/// Why: Like `EditSymbolTool`, must stage a pending patch into the bundle's
/// shared `PatchStore` without touching a process-global static.
/// What: Carries a clone of the bundle's `PatchStore`.
/// Test: `edit_then_apply_round_trips` and the insert path is exercised by
/// integration tests in `ast::editor`.
pub struct InsertSymbolTool {
    patch_store: PatchStore,
}

impl InsertSymbolTool {
    /// Build with an explicit `PatchStore` clone.
    ///
    /// Why: Mirror of `EditSymbolTool::new` — explicit injection, no globals.
    /// What: Stores the clone.
    /// Test: Used by `ast_native_tools` and tests that build the tool directly.
    pub fn new(patch_store: PatchStore) -> Self {
        Self { patch_store }
    }
}

#[async_trait]
impl ToolExecutor for InsertSymbolTool {
    fn name(&self) -> &str {
        "insert_symbol"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "insert_symbol",
                "description": "Insert new source code (a function, struct, etc.) immediately after the named anchor symbol. Validates syntax and stages a pending patch. Disk is NOT modified.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "after": {"type": "string", "description": "Anchor symbol name; new code is inserted after its closing byte."},
                        "source": {"type": "string", "description": "Source code to insert."}
                    },
                    "required": ["file", "after", "source"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("insert_symbol: missing 'file'");
        };
        let Some(after) = args.get("after").and_then(Value::as_str) else {
            return ToolResult::err("insert_symbol: missing 'after'");
        };
        let Some(source) = args.get("source").and_then(Value::as_str) else {
            return ToolResult::err("insert_symbol: missing 'source'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "insert".into(),
            detail: format!("after `{after}` in {file}"),
        });
        match insert_after_symbol(Path::new(file), after, source) {
            Ok(p) => {
                let id = p.id.clone();
                let diff = p.diff.clone();
                store_patch(&self.patch_store, p);
                ToolResult::ok(
                    json!({
                        "patch_id": id,
                        "diff": diff,
                        "status": "pending"
                    })
                    .to_string(),
                )
            }
            Err(e) => ToolResult::err(format!("insert_symbol: {e}")),
        }
    }
}
