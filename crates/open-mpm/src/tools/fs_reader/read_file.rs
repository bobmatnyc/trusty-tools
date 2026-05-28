//! The `read_file` read-only tool.
//!
//! Why: An explorer agent needs to read a file's contents without any ability
//! to modify it; the CWD guard prevents path traversal out of the project.
//! What: `ReadFileTool` implements `ToolExecutor`; returns file contents
//! truncated at `READ_FILE_MAX_CHARS`.
//! Test: `super::read_file_*` cases in the parent module's test block.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::cwd::{READ_FILE_MAX_CHARS, resolve_within_cwd};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Read-only file reader.
pub struct ReadFileTool;

impl ReadFileTool {
    /// Construct a new `ReadFileTool`. Zero-sized; state lives in the
    /// working directory resolved at call time.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file's contents. Paths outside the current working directory are rejected. Output is truncated at ~50k chars.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative (from CWD) or absolute path; must resolve inside CWD."
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
            return ToolResult::err("read_file: missing 'path'");
        };
        let canon = match resolve_within_cwd(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("read_file: {e}")),
        };
        if !canon.is_file() {
            return ToolResult::err(format!(
                "read_file: not a regular file: {}",
                canon.display()
            ));
        }
        match tokio::fs::read_to_string(&canon).await {
            Ok(text) => {
                if text.chars().count() <= READ_FILE_MAX_CHARS {
                    ToolResult::ok(text)
                } else {
                    let truncated: String = text.chars().take(READ_FILE_MAX_CHARS).collect();
                    ToolResult::ok(format!(
                        "{truncated}\n...[truncated at {READ_FILE_MAX_CHARS} chars]"
                    ))
                }
            }
            Err(e) => ToolResult::err(format!("read_file: {e}")),
        }
    }
}
