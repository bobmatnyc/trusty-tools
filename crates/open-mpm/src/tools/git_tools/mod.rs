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

use std::path::PathBuf;
use std::sync::Arc;

use crate::tools::traits::ToolExecutor;

mod branch;
mod helpers;
mod inspect;
mod remote;
mod write;

use branch::{GitBranchesTool, GitCheckoutTool, GitCreateBranchTool};
use inspect::{GitLogTool, GitSearchCommitsTool, GitStatusTool};
use remote::{GitFetchTool, GitPullTool, GitPushTool};
use write::{GitCommitTool, GitStageTool, GitStashTool};

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
