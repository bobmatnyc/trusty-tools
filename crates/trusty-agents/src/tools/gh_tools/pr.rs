//! Read-only pull-request inspection tools: `gh_pr_list`, `gh_pr_view`,
//! `gh_pr_checks` (#4170).
//!
//! Why: These three close the exact gap epic #4167's gap analysis names —
//! "GitHub PR/CI tooling is absent — no `gh pr list/view/checks`, no check-run
//! status queries." `git_log`/`git_status`/`git_branches` are local and
//! read-only; none of them can answer "is PR #4200 mergeable and are its
//! checks green", which is the first question a PM-tier orchestration turn
//! asks.
//! What: One `ToolExecutor` per `gh` subcommand, argv built from validated
//! operands only, output returned verbatim. NOTHING here mutates: no
//! `pr create`, `pr merge`, `pr edit`, `pr comment`, or `pr close`.
//! Test: `gh_pr_tools_are_read_only_subcommands`,
//! `gh_pr_view_schema_has_the_expected_envelope`,
//! `gh_pr_view_rejects_a_flag_shaped_selector`,
//! `gh_pr_list_rejects_an_unknown_state`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::{enum_arg, fn_schema, limit_arg, plain_arg, repo_arg, run_gh};

/// Shared `repo` property text, so all five tools describe it identically.
fn repo_property() -> Value {
    json!({
        "type": "string",
        "description": "Optional 'owner/repo' to target. Omit to use the repository \
                        of the current working directory."
    })
}

/// `gh pr list` — enumerate pull requests.
pub(super) struct GhPrListTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GhPrListTool {
    fn name(&self) -> &str {
        "gh_pr_list"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "gh_pr_list",
            "List pull requests via the GitHub CLI (read-only). Returns gh's own \
             table output verbatim.",
            json!({
                "type": "object",
                "properties": {
                    "repo": repo_property(),
                    "state": {
                        "type": "string",
                        "enum": ["open", "closed", "merged", "all"],
                        "description": "Filter by PR state (default 'open')."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max PRs to return, 1-100 (default 20)."
                    },
                    "author": {
                        "type": "string",
                        "description": "Optional GitHub login to filter by author."
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let mut argv = vec!["pr".to_string(), "list".to_string()];
        if let Some(repo) = args.get("repo").and_then(Value::as_str) {
            match repo_arg(repo) {
                Ok(v) => argv.extend(["--repo".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        let state = args.get("state").and_then(Value::as_str).unwrap_or("open");
        match enum_arg("state", state, &["open", "closed", "merged", "all"]) {
            Ok(v) => argv.extend(["--state".to_string(), v]),
            Err(e) => return ToolResult::err(e),
        }
        if let Some(author) = args.get("author").and_then(Value::as_str) {
            match plain_arg("author", author) {
                Ok(v) => argv.extend(["--author".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        let limit = limit_arg(args.get("limit").and_then(Value::as_u64), 20);
        argv.extend(["--limit".to_string(), limit.to_string()]);
        run_gh(&self.root, &argv, false).await
    }
}

/// `gh pr view` — read one pull request.
pub(super) struct GhPrViewTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GhPrViewTool {
    fn name(&self) -> &str {
        "gh_pr_view"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "gh_pr_view",
            "Show one pull request's title, state, body, and metadata via the GitHub \
             CLI (read-only). Returns gh's own output verbatim.",
            json!({
                "type": "object",
                "properties": {
                    "pr": {
                        "type": "string",
                        "description": "PR number, branch name, or PR URL."
                    },
                    "repo": repo_property(),
                    "comments": {
                        "type": "boolean",
                        "description": "Include the PR's comments (default false)."
                    }
                },
                "required": ["pr"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(pr) = args.get("pr").and_then(Value::as_str) else {
            return ToolResult::err("'pr' is required");
        };
        let pr = match plain_arg("pr", pr) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(e),
        };
        let mut argv = vec!["pr".to_string(), "view".to_string(), pr];
        if let Some(repo) = args.get("repo").and_then(Value::as_str) {
            match repo_arg(repo) {
                Ok(v) => argv.extend(["--repo".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        if args.get("comments").and_then(Value::as_bool) == Some(true) {
            argv.push("--comments".to_string());
        }
        run_gh(&self.root, &argv, false).await
    }
}

/// `gh pr checks` — the check-run aggregation primitive.
pub(super) struct GhPrChecksTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GhPrChecksTool {
    fn name(&self) -> &str {
        "gh_pr_checks"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "gh_pr_checks",
            "Show every CI check run for one pull request, with its state and \
             duration, via the GitHub CLI (read-only). Reports red and pending \
             checks as normal output, not as a tool failure.",
            json!({
                "type": "object",
                "properties": {
                    "pr": {
                        "type": "string",
                        "description": "PR number, branch name, or PR URL."
                    },
                    "repo": repo_property()
                },
                "required": ["pr"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(pr) = args.get("pr").and_then(Value::as_str) else {
            return ToolResult::err("'pr' is required");
        };
        let pr = match plain_arg("pr", pr) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(e),
        };
        let mut argv = vec!["pr".to_string(), "checks".to_string(), pr];
        if let Some(repo) = args.get("repo").and_then(Value::as_str) {
            match repo_arg(repo) {
                Ok(v) => argv.extend(["--repo".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        // `gh pr checks` exits non-zero for failing/pending checks — see
        // `run_gh`'s `tolerate_nonzero` rationale.
        run_gh(&self.root, &argv, true).await
    }
}
