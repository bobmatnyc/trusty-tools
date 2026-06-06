//! The `list_dir` read-only tool.
//!
//! Why: An explorer agent needs a one-level directory listing to understand
//! project layout without recursing or modifying anything; the CWD guard
//! prevents listing outside the project.
//! What: `ListDirTool` implements `ToolExecutor`; returns a sorted, depth-1
//! listing with kind + size per entry.
//! Test: `super::list_dir_*` cases in the parent module's test block.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::cwd::resolve_within_cwd;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Read-only directory lister (depth = 1).
pub struct ListDirTool;

impl ListDirTool {
    /// Construct a new `ListDirTool`. Zero-sized; the target path is
    /// supplied per-call via the tool arguments.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ListDirTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List entries in a directory (non-recursive, depth=1). Rejects paths outside the CWD.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative or absolute path to a directory inside CWD."
                        },
                        "show_hidden": {
                            "type": "boolean",
                            "description": "Whether to include dotfiles. Default false."
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("list_dir: missing 'path'");
        };
        let show_hidden = args
            .get("show_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let canon = match resolve_within_cwd(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("list_dir: {e}")),
        };
        if !canon.is_dir() {
            return ToolResult::err(format!("list_dir: not a directory: {}", canon.display()));
        }

        let mut entries: Vec<(String, String, u64)> = Vec::new();
        let mut rd = match tokio::fs::read_dir(&canon).await {
            Ok(r) => r,
            Err(e) => {
                return ToolResult::err(format!(
                    "list_dir: failed to read {}: {e}",
                    canon.display()
                ));
            }
        };
        loop {
            match rd.next_entry().await {
                Ok(Some(entry)) => {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let (kind, size) = match entry.metadata().await {
                        Ok(md) => {
                            let k = if md.is_dir() {
                                "dir"
                            } else if md.is_file() {
                                "file"
                            } else {
                                "other"
                            };
                            (k.to_string(), md.len())
                        }
                        Err(_) => ("unknown".to_string(), 0),
                    };
                    entries.push((name, kind, size));
                }
                Ok(None) => break,
                Err(e) => {
                    return ToolResult::err(format!("list_dir: iter error: {e}"));
                }
            }
        }

        entries.sort_by(|a, b| a.0.cmp(&b.0));

        if entries.is_empty() {
            return ToolResult::ok(format!("(empty directory: {})", canon.display()));
        }

        let mut out = String::new();
        out.push_str(&format!("# {}\n", canon.display()));
        for (name, kind, size) in entries {
            out.push_str(&format!("{kind:>5}  {size:>10}  {name}\n"));
        }
        ToolResult::ok(out)
    }
}
