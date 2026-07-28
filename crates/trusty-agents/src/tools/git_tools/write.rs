//! Working-tree-mutating git tools: stage, commit, and stash.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::agents::CrossProjectScope;
use crate::git;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::{scoped_root, scoped_schema};

pub(super) struct GitStageTool {
    pub(super) scope: Arc<CrossProjectScope>,
}

#[async_trait]
impl ToolExecutor for GitStageTool {
    fn name(&self) -> &str {
        "git_stage"
    }
    fn schema(&self) -> Value {
        scoped_schema(
            &self.scope,
            "git_stage",
            "Stage one or more files for the next commit.",
            json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Repo-relative file paths to stage"
                    }
                },
                "required": ["files"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let files: Vec<String> = match args.get("files").and_then(Value::as_array) {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            None => return ToolResult::err("'files' (array of strings) is required"),
        };
        if files.is_empty() {
            return ToolResult::err("'files' must contain at least one path");
        }
        let root = match scoped_root(&self.scope, &args, "git_stage") {
            Ok(r) => r,
            Err(e) => return ToolResult::err(e),
        };
        match git::commit::stage_files(&files, &root).await {
            Ok(out) => {
                let body = if out.is_empty() {
                    format!("Staged {} file(s)", files.len())
                } else {
                    out
                };
                ToolResult::ok(body)
            }
            Err(e) => ToolResult::err(format!("git_stage failed: {e}")),
        }
    }
}

pub(super) struct GitCommitTool {
    pub(super) scope: Arc<CrossProjectScope>,
}

#[async_trait]
impl ToolExecutor for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }
    fn schema(&self) -> Value {
        scoped_schema(
            &self.scope,
            "git_commit",
            "Create a commit from staged changes. Honors hooks and signing via the git CLI.",
            json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string", "description": "Commit message"}
                },
                "required": ["message"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(message) = args.get("message").and_then(Value::as_str) else {
            return ToolResult::err("'message' is required");
        };
        let root = match scoped_root(&self.scope, &args, "git_commit") {
            Ok(r) => r,
            Err(e) => return ToolResult::err(e),
        };
        match git::commit::create_commit(message, &root).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("git_commit failed: {e}")),
        }
    }
}

pub(super) struct GitStashTool {
    pub(super) scope: Arc<CrossProjectScope>,
}

#[async_trait]
impl ToolExecutor for GitStashTool {
    fn name(&self) -> &str {
        "git_stash"
    }
    fn schema(&self) -> Value {
        scoped_schema(
            &self.scope,
            "git_stash",
            "Stash management — push (save), pop (restore), or list.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["push", "pop", "list"], "description": "Stash action to perform"},
                    "message": {"type": "string", "description": "Optional message for 'push' action"}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(a) => a,
            None => return ToolResult::err("'action' is required (push, pop, or list)"),
        };
        let root = match scoped_root(&self.scope, &args, "git_stash") {
            Ok(r) => r,
            Err(e) => return ToolResult::err(e),
        };
        let res = match action {
            "push" => {
                let msg = args.get("message").and_then(Value::as_str);
                git::stash::stash_push(msg, &root).await
            }
            "pop" => git::stash::stash_pop(&root).await,
            "list" => git::stash::stash_list(&root).await,
            other => {
                return ToolResult::err(format!(
                    "unknown stash action '{other}' — expected push, pop, or list"
                ));
            }
        };
        match res {
            Ok(out) => ToolResult::ok(if out.trim().is_empty() {
                format!("git stash {action} ok")
            } else {
                out
            }),
            Err(e) => ToolResult::err(format!("git_stash {action} failed: {e}")),
        }
    }
}
