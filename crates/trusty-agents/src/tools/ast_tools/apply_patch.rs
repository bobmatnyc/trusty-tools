//! `apply_patch` — commit a pending AST patch to disk.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ast::editor::apply_patch as do_apply_patch;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::patch_store::{PatchStore, take_patch};

/// `apply_patch` — commit a pending patch to disk.
///
/// Why: Reads from the bundle's shared `PatchStore` to find a patch produced
/// by an earlier `edit_symbol` / `insert_symbol` call. Must use the *same*
/// store the producer wrote to, hence the injected `Arc`.
/// What: Carries a clone of the bundle's `PatchStore`.
/// Test: `edit_then_apply_round_trips`.
pub struct ApplyPatchTool {
    patch_store: PatchStore,
}

impl ApplyPatchTool {
    /// Build with an explicit `PatchStore` clone.
    ///
    /// Why: Producers and `apply_patch` must reference the same store.
    /// `ast_native_tools` and any test that exercises the produce-then-apply
    /// flow constructs the store once and passes the clone here.
    /// What: Stores the clone.
    /// Test: `edit_then_apply_round_trips`.
    pub fn new(patch_store: PatchStore) -> Self {
        Self { patch_store }
    }
}

#[async_trait]
impl ToolExecutor for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Commit a pending patch (created by `edit_symbol` or `insert_symbol`) to disk. The patch is consumed (one-shot).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch_id": {"type": "string"}
                    },
                    "required": ["patch_id"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("patch_id").and_then(Value::as_str) else {
            return ToolResult::err("apply_patch: missing 'patch_id'");
        };
        let Some(patch) = take_patch(&self.patch_store, id) else {
            return ToolResult::err(format!("apply_patch: no pending patch with id '{id}'"));
        };
        let lines_changed = patch
            .diff
            .lines()
            .filter(|l| {
                (l.starts_with('+') && !l.starts_with("+++"))
                    || (l.starts_with('-') && !l.starts_with("---"))
            })
            .count();
        let file = patch.file.clone();
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "patch".into(),
            detail: format!("{lines_changed} line(s) → {}", file.display()),
        });
        match do_apply_patch(&patch) {
            Ok(()) => ToolResult::ok(
                json!({
                    "file": file.display().to_string(),
                    "lines_changed": lines_changed,
                    "status": "applied"
                })
                .to_string(),
            ),
            Err(e) => ToolResult::err(format!("apply_patch: {e}")),
        }
    }
}
