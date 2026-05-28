//! Native git tool surface for the LLM (#247).
//!
//! Why: Coordinating agents (ctrl, pm, research, observe) need first-class
//! git tools instead of relying on `shell_exec`. Twelve narrow tools each
//! with a typed schema let the LLM call the right operation without
//! constructing shell commands.
//! What: `git_tools(root)` returns `Vec<Arc<dyn ToolExecutor>>` for the
//! twelve operations: status, log, branches, create_branch, checkout,
//! stage, commit, push, pull, fetch, stash, search_commits.
//! Test: See unit tests below — count, schemas, and required-argument
//! validation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::git::{self, GitRepo};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Build the 12 git tools bound to the given working-tree `root`.
///
/// Why: A single factory keeps registration sites compact and ensures
/// every tool is constructed with the same project root, so an
/// LLM-instructed `git_status` and a follow-up `git_commit` both target
/// the same repo.
/// What: Returns an `Arc<dyn ToolExecutor>` for each of the 12 tools.
/// Test: `git_tools_count_is_12`.
pub fn git_tools(root: PathBuf) -> Vec<Arc<dyn ToolExecutor>> {
    vec![
        Arc::new(GitStatusTool { root: root.clone() }),
        Arc::new(GitLogTool { root: root.clone() }),
        Arc::new(GitBranchesTool { root: root.clone() }),
        Arc::new(GitCreateBranchTool { root: root.clone() }),
        Arc::new(GitCheckoutTool { root: root.clone() }),
        Arc::new(GitStageTool { root: root.clone() }),
        Arc::new(GitCommitTool { root: root.clone() }),
        Arc::new(GitPushTool { root: root.clone() }),
        Arc::new(GitPullTool { root: root.clone() }),
        Arc::new(GitFetchTool { root: root.clone() }),
        Arc::new(GitStashTool { root: root.clone() }),
        Arc::new(GitSearchCommitsTool { root }),
    ]
}

fn fn_schema(name: &str, description: &str, params: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": params
        }
    })
}

fn open_repo(root: &Path) -> std::result::Result<GitRepo, String> {
    GitRepo::open(root).map_err(|e| format!("failed to open git repo at {}: {e}", root.display()))
}

// ---------------------------------------------------------------------------
// git_status
// ---------------------------------------------------------------------------

struct GitStatusTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_log
// ---------------------------------------------------------------------------

struct GitLogTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_branches
// ---------------------------------------------------------------------------

struct GitBranchesTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_create_branch
// ---------------------------------------------------------------------------

struct GitCreateBranchTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_checkout
// ---------------------------------------------------------------------------

struct GitCheckoutTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_stage
// ---------------------------------------------------------------------------

struct GitStageTool {
    root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitStageTool {
    fn name(&self) -> &str {
        "git_stage"
    }
    fn schema(&self) -> Value {
        fn_schema(
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
        match git::commit::stage_files(&files, &self.root).await {
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

// ---------------------------------------------------------------------------
// git_commit
// ---------------------------------------------------------------------------

struct GitCommitTool {
    root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }
    fn schema(&self) -> Value {
        fn_schema(
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
        match git::commit::create_commit(message, &self.root).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("git_commit failed: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// git_push
// ---------------------------------------------------------------------------

struct GitPushTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_pull
// ---------------------------------------------------------------------------

struct GitPullTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_fetch
// ---------------------------------------------------------------------------

struct GitFetchTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// git_stash
// ---------------------------------------------------------------------------

struct GitStashTool {
    root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GitStashTool {
    fn name(&self) -> &str {
        "git_stash"
    }
    fn schema(&self) -> Value {
        fn_schema(
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
        let res = match action {
            "push" => {
                let msg = args.get("message").and_then(Value::as_str);
                git::stash::stash_push(msg, &self.root).await
            }
            "pop" => git::stash::stash_pop(&self.root).await,
            "list" => git::stash::stash_list(&self.root).await,
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

// ---------------------------------------------------------------------------
// git_search_commits
// ---------------------------------------------------------------------------

struct GitSearchCommitsTool {
    root: PathBuf,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd_root() -> PathBuf {
        std::env::current_dir().unwrap()
    }

    #[test]
    fn git_tools_count_is_12() {
        let tools = git_tools(cwd_root());
        assert_eq!(tools.len(), 12);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for expected in [
            "git_status",
            "git_log",
            "git_branches",
            "git_create_branch",
            "git_checkout",
            "git_stage",
            "git_commit",
            "git_push",
            "git_pull",
            "git_fetch",
            "git_stash",
            "git_search_commits",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool '{expected}' in {names:?}"
            );
        }
    }

    #[test]
    fn git_status_tool_has_no_required_params() {
        let tools = git_tools(cwd_root());
        let s = tools
            .iter()
            .find(|t| t.name() == "git_status")
            .unwrap()
            .schema();
        let required = s["function"]["parameters"]["required"]
            .as_array()
            .expect("required is array");
        assert!(required.is_empty());
    }

    #[test]
    fn git_commit_tool_requires_message() {
        let tools = git_tools(cwd_root());
        let s = tools
            .iter()
            .find(|t| t.name() == "git_commit")
            .unwrap()
            .schema();
        let required: Vec<String> = s["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(required.contains(&"message".to_string()));
    }

    #[test]
    fn git_log_tool_schema_valid() {
        let tools = git_tools(cwd_root());
        let s = tools
            .iter()
            .find(|t| t.name() == "git_log")
            .unwrap()
            .schema();
        assert_eq!(s["type"], "function");
        assert_eq!(s["function"]["name"], "git_log");
        let props = &s["function"]["parameters"]["properties"];
        assert!(props.get("limit").is_some());
        assert!(props.get("search").is_some());
    }

    #[tokio::test]
    async fn git_status_executes_against_open_mpm_repo() {
        let tools = git_tools(cwd_root());
        let status_tool = tools.iter().find(|t| t.name() == "git_status").unwrap();
        let out = status_tool.execute(json!({})).await;
        // We don't assert specific content; just that the tool ran without
        // failing at the open()/get_status() level.
        assert!(
            !out.is_error(),
            "git_status returned error: {}",
            out.content()
        );
    }

    #[tokio::test]
    async fn git_commit_rejects_missing_message() {
        let tools = git_tools(cwd_root());
        let tool = tools.iter().find(|t| t.name() == "git_commit").unwrap();
        let out = tool.execute(json!({})).await;
        assert!(out.is_error());
        assert!(out.content().contains("message"));
    }

    #[tokio::test]
    async fn git_stage_rejects_empty_files() {
        let tools = git_tools(cwd_root());
        let tool = tools.iter().find(|t| t.name() == "git_stage").unwrap();
        let out = tool.execute(json!({"files": []})).await;
        assert!(out.is_error());
    }

    #[tokio::test]
    async fn git_stash_rejects_unknown_action() {
        let tools = git_tools(cwd_root());
        let tool = tools.iter().find(|t| t.name() == "git_stash").unwrap();
        let out = tool.execute(json!({"action": "bogus"})).await;
        assert!(out.is_error());
        assert!(out.content().contains("bogus"));
    }
}
