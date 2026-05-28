//! Remote-interacting git tools: push, pull, and fetch.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::git;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::fn_schema;

pub(super) struct GitPushTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitPushTool {
    fn name(&self) -> &str {
        "git_push"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_push",
            "Push the current (or named) branch to origin.",
            json!({
                "type": "object",
                "properties": {
                    "branch": {"type": "string", "description": "Branch to push (default: current branch)"}
                },
                "required": [],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let branch = args.get("branch").and_then(Value::as_str);
        match git::remote::push(branch, &self.root).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("git_push failed: {e}")),
        }
    }
}

pub(super) struct GitPullTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitPullTool {
    fn name(&self) -> &str {
        "git_pull"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_pull",
            "Pull from upstream. Defaults to rebase mode for linear history.",
            json!({
                "type": "object",
                "properties": {
                    "rebase": {"type": "boolean", "description": "Use --rebase (default true)"}
                },
                "required": [],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let rebase = args.get("rebase").and_then(Value::as_bool).unwrap_or(true);
        match git::remote::pull(rebase, &self.root).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("git_pull failed: {e}")),
        }
    }
}

pub(super) struct GitFetchTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitFetchTool {
    fn name(&self) -> &str {
        "git_fetch"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_fetch",
            "Fetch refs from configured remotes without merging.",
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        )
    }
    async fn execute(&self, _args: Value) -> ToolResult {
        match git::remote::fetch(&self.root).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("git_fetch failed: {e}")),
        }
    }
}
