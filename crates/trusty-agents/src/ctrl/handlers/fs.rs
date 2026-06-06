//! File-system mutation CTRL tools (`move_file`, `create_dir`).
//!
//! Why: CTRL's digital-twin persona reorganizes project layouts (moving stray
//! scripts into `scripts/`, scaffolding empty directories). Direct file moves
//! let it reshape projects without spawning a PM/engineer round-trip.
//! What: `MoveFileTool` (rename / cross-device fallback), `CreateDirTool`
//! (`mkdir -p` semantics with `~` expansion).
//! Test: `move_file_tool_renames_basic`, `move_file_tool_into_directory`,
//! `move_file_tool_missing_source_errors`, `create_dir_tool_makes_nested_dir`,
//! `create_dir_tool_idempotent_on_existing`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// `move_file(from, to)` — rename or relocate a file on disk.
///
/// Why: CTRL's digital-twin persona reorganizes project layouts (e.g. moving
/// stray scripts into `scripts/`). Direct file moves let it reshape projects
/// without spawning a PM/engineer round-trip.
/// What: Canonicalizes `from`, computes the final destination (if `to` is a
/// directory, append `from`'s file name), creates intermediate parents, and
/// renames. Falls back to copy+delete on cross-device errors (EXDEV).
/// Test: `move_file_tool_renames_basic`, `move_file_tool_into_directory`.
pub(crate) struct MoveFileTool;

#[async_trait]
impl ToolExecutor for MoveFileTool {
    fn name(&self) -> &str {
        "move_file"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "move_file",
                "description": "Move or rename a file. If 'to' is an existing directory, the source is moved into it; otherwise it is renamed to the exact 'to' path. Intermediate parent directories of 'to' are created if missing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Source path of the file to move" },
                        "to":   { "type": "string", "description": "Destination path or directory" }
                    },
                    "required": ["from", "to"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(from_raw) = args.get("from").and_then(Value::as_str) else {
            return ToolResult::err("move_file: missing 'from'");
        };
        let Some(to_raw) = args.get("to").and_then(Value::as_str) else {
            return ToolResult::err("move_file: missing 'to'");
        };
        let from = match PathBuf::from(from_raw).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!("move_file: cannot resolve '{from_raw}': {e}"));
            }
        };
        if !from.exists() {
            return ToolResult::err(format!("move_file: source not found: {}", from.display()));
        }
        // Resolve the destination. If `to` is an existing directory, move the
        // source into it preserving its file name. Otherwise treat `to` as the
        // exact target path.
        let to_input = PathBuf::from(to_raw);
        let dest = if to_input.is_dir() {
            match from.file_name() {
                Some(n) => to_input.join(n),
                None => {
                    return ToolResult::err(format!(
                        "move_file: source has no file name: {}",
                        from.display()
                    ));
                }
            }
        } else {
            to_input
        };
        // Ensure the destination's parent exists.
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::err(format!(
                "move_file: cannot create parent {}: {e}",
                parent.display()
            ));
        }
        match tokio::fs::rename(&from, &dest).await {
            Ok(()) => ToolResult::ok(format!("Moved: {} → {}", from.display(), dest.display())),
            Err(e) => {
                // EXDEV — rename across devices fails on most platforms; fall
                // back to copy + remove.
                if e.raw_os_error() == Some(18) || e.kind() == std::io::ErrorKind::CrossesDevices {
                    if let Err(e2) = tokio::fs::copy(&from, &dest).await {
                        return ToolResult::err(format!("move_file: copy fallback failed: {e2}"));
                    }
                    if let Err(e2) = tokio::fs::remove_file(&from).await {
                        return ToolResult::err(format!(
                            "move_file: copy succeeded but source delete failed: {e2}"
                        ));
                    }
                    ToolResult::ok(format!(
                        "Moved (copy+delete): {} → {}",
                        from.display(),
                        dest.display()
                    ))
                } else {
                    ToolResult::err(format!("move_file: rename failed: {e}"))
                }
            }
        }
    }
}

/// `create_dir(path)` — create a directory (and any missing parents).
///
/// Why: CTRL scaffolds project layouts and reorganizes existing trees;
/// `mkdir -p` semantics let it stage empty directories before delegating
/// work to a PM.
/// What: Expands `~` to the user's home dir, then calls `create_dir_all`.
/// Test: `create_dir_tool_makes_nested_dir`.
pub(crate) struct CreateDirTool;

#[async_trait]
impl ToolExecutor for CreateDirTool {
    fn name(&self) -> &str {
        "create_dir"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "create_dir",
                "description": "Create a directory, including any missing intermediate parents (mkdir -p semantics).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path to create. Leading '~' is expanded to the user's home directory." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("create_dir: missing 'path'");
        };
        let expanded: PathBuf = if let Some(rest) = raw.strip_prefix("~/") {
            match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(rest),
                None => PathBuf::from(raw),
            }
        } else if raw == "~" {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(raw))
        } else {
            PathBuf::from(raw)
        };
        if expanded.is_dir() {
            return ToolResult::ok(format!("Directory already exists: {}", expanded.display()));
        }
        match tokio::fs::create_dir_all(&expanded).await {
            Ok(()) => ToolResult::ok(format!("Created directory: {}", expanded.display())),
            Err(e) => ToolResult::err(format!(
                "create_dir: failed to create {}: {e}",
                expanded.display()
            )),
        }
    }
}
