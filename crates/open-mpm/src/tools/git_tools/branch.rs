//! Branch-oriented git tools: list, create, and checkout branches.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::git;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::{fn_schema, open_repo};

pub(super) struct GitBranchesTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitBranchesTool {
    fn name(&self) -> &str {
        "git_branches"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_branches",
            "List local branches (and optionally remote-tracking branches) with current-branch and upstream metadata.",
            json!({
                "type": "object",
                "properties": {
                    "include_remote": {"type": "boolean", "description": "Include remote-tracking branches (default false)"}
                },
                "required": [],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let include_remote = args
            .get("include_remote")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let repo = match open_repo(&self.root) {
            Ok(r) => r,
            Err(e) => return ToolResult::err(e),
        };
        match git::branch::list_branches(&repo, include_remote) {
            Ok(branches) => {
                if branches.is_empty() {
                    return ToolResult::ok("No branches".to_string());
                }
                let mut s = String::with_capacity(branches.len() * 32);
                for b in branches {
                    let marker = if b.is_current { "* " } else { "  " };
                    let upstream = b.upstream.map(|u| format!(" -> {u}")).unwrap_or_default();
                    s.push_str(&format!("{marker}{}{upstream}\n", b.name));
                }
                ToolResult::ok(s)
            }
            Err(e) => ToolResult::err(format!("git_branches failed: {e}")),
        }
    }
}

pub(super) struct GitCreateBranchTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitCreateBranchTool {
    fn name(&self) -> &str {
        "git_create_branch"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_create_branch",
            "Create a new local branch from HEAD and check it out.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "New branch name"}
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("'name' is required");
        };
        match git::branch::create_branch(name, &self.root).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("git_create_branch failed: {e}")),
        }
    }
}

pub(super) struct GitCheckoutTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitCheckoutTool {
    fn name(&self) -> &str {
        "git_checkout"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_checkout",
            "Check out an existing branch, tag, or commit.",
            json!({
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "Branch name, tag, or commit-ish"}
                },
                "required": ["target"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(target) = args.get("target").and_then(Value::as_str) else {
            return ToolResult::err("'target' is required");
        };
        match git::branch::checkout(target, &self.root).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("git_checkout failed: {e}")),
        }
    }
}
