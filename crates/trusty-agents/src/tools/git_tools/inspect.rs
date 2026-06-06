//! Read-only git tools: status, log, and commit search.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::git;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::{fn_schema, open_repo};

pub(super) struct GitStatusTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_status",
            "Show working-tree and index status (modified/added/deleted/untracked/renamed/conflicted files).",
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        )
    }
    async fn execute(&self, _args: Value) -> ToolResult {
        let repo = match open_repo(&self.root) {
            Ok(r) => r,
            Err(e) => return ToolResult::err(e),
        };
        match git::status::get_status(&repo) {
            Ok(entries) => ToolResult::ok(git::status::format_status(&entries)),
            Err(e) => ToolResult::err(format!("git_status failed: {e}")),
        }
    }
}

pub(super) struct GitLogTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitLogTool {
    fn name(&self) -> &str {
        "git_log"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_log",
            "Show recent commits from HEAD. Optionally filter by substring search on commit messages.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "Max commits to return (default 10)"},
                    "search": {"type": "string", "description": "Optional case-insensitive substring filter on commit message"}
                },
                "required": [],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(10)
            .max(1);
        let repo = match open_repo(&self.root) {
            Ok(r) => r,
            Err(e) => return ToolResult::err(e),
        };
        let result = if let Some(q) = args.get("search").and_then(Value::as_str) {
            git::log::search_commits(&repo, q, limit)
        } else {
            git::log::get_log(&repo, limit)
        };
        match result {
            Ok(commits) => ToolResult::ok(git::log::format_log(&commits)),
            Err(e) => ToolResult::err(format!("git_log failed: {e}")),
        }
    }
}

pub(super) struct GitSearchCommitsTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitSearchCommitsTool {
    fn name(&self) -> &str {
        "git_search_commits"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "git_search_commits",
            "Search commit history for a substring in the commit message (case-insensitive).",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Substring to search for"},
                    "limit": {"type": "integer", "description": "Max matches to return (default 10)"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("'query' is required");
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(10)
            .max(1);
        let repo = match open_repo(&self.root) {
            Ok(r) => r,
            Err(e) => return ToolResult::err(e),
        };
        match git::log::search_commits(&repo, query, limit) {
            Ok(commits) => ToolResult::ok(git::log::format_log(&commits)),
            Err(e) => ToolResult::err(format!("git_search_commits failed: {e}")),
        }
    }
}
