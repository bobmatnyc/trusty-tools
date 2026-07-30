//! Read-only GitHub Actions inspection tools: `gh_run_list`, `gh_run_view`
//! (#4170).
//!
//! Why: `gh pr checks` answers "are this PR's checks green"; it does NOT
//! answer "which job failed and why", nor "what has CI been doing on main".
//! An orchestration turn reconciling a snapshot against live CI state needs
//! both — the per-PR aggregate AND the run/job drill-down. `actions_status`
//! (`tools::native_ticketing::actions`) is not a substitute: it requires a
//! configured `TicketingClient`, takes a workflow FILE name rather than a run
//! id, and returns re-shaped JSON rather than gh's own output.
//! What: Two `ToolExecutor`s over `gh run list` / `gh run view`, argv built
//! from validated operands only. Deliberately NOT included: `gh run watch`
//! (blocks for the lifetime of a CI run, which a bounded tool turn cannot
//! absorb — poll `gh_run_view` instead) and `gh run rerun`/`gh workflow run`
//! (mutating; see this module's parent doc comment).
//! Test: `gh_ci_tools_are_read_only_subcommands`,
//! `gh_run_view_rejects_a_flag_shaped_run_id`,
//! `gh_run_list_rejects_an_unknown_status`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::{enum_arg, fn_schema, limit_arg, plain_arg, repo_arg, run_gh};

/// The `--status` values `gh run list` accepts.
const RUN_STATUSES: &[&str] = &[
    "queued",
    "completed",
    "in_progress",
    "requested",
    "waiting",
    "pending",
    "action_required",
    "cancelled",
    "failure",
    "neutral",
    "skipped",
    "stale",
    "startup_failure",
    "success",
    "timed_out",
];

/// `gh run list` — enumerate GitHub Actions runs.
pub(super) struct GhRunListTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GhRunListTool {
    fn name(&self) -> &str {
        "gh_run_list"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "gh_run_list",
            "List recent GitHub Actions workflow runs via the GitHub CLI (read-only). \
             Returns gh's own table output verbatim.",
            json!({
                "type": "object",
                "properties": {
                    "repo": {
                        "type": "string",
                        "description": "Optional 'owner/repo' to target. Omit to use the \
                                        repository of the current working directory."
                    },
                    "branch": {
                        "type": "string",
                        "description": "Optional branch name to filter runs by."
                    },
                    "workflow": {
                        "type": "string",
                        "description": "Optional workflow file name (e.g. 'ci.yml') to \
                                        filter runs by."
                    },
                    "status": {
                        "type": "string",
                        "enum": RUN_STATUSES,
                        "description": "Optional run status to filter by."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max runs to return, 1-100 (default 20)."
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let mut argv = vec!["run".to_string(), "list".to_string()];
        if let Some(repo) = args.get("repo").and_then(Value::as_str) {
            match repo_arg(repo) {
                Ok(v) => argv.extend(["--repo".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        if let Some(branch) = args.get("branch").and_then(Value::as_str) {
            match plain_arg("branch", branch) {
                Ok(v) => argv.extend(["--branch".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        if let Some(workflow) = args.get("workflow").and_then(Value::as_str) {
            match plain_arg("workflow", workflow) {
                Ok(v) => argv.extend(["--workflow".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        if let Some(status) = args.get("status").and_then(Value::as_str) {
            match enum_arg("status", status, RUN_STATUSES) {
                Ok(v) => argv.extend(["--status".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        let limit = limit_arg(args.get("limit").and_then(Value::as_u64), 20);
        argv.extend(["--limit".to_string(), limit.to_string()]);
        run_gh(&self.root, &argv, false).await
    }
}

/// `gh run view` — read one Actions run, optionally a failing job's log.
pub(super) struct GhRunViewTool {
    pub(super) root: PathBuf,
}

#[async_trait]
impl ToolExecutor for GhRunViewTool {
    fn name(&self) -> &str {
        "gh_run_view"
    }
    fn schema(&self) -> Value {
        fn_schema(
            "gh_run_view",
            "Show one GitHub Actions run's jobs and their conclusions via the GitHub \
             CLI (read-only). Set log_failed to also print the failed steps' logs. \
             Returns gh's own output verbatim.",
            json!({
                "type": "object",
                "properties": {
                    "run_id": {
                        "type": "string",
                        "description": "The workflow run id (as shown by gh_run_list)."
                    },
                    "repo": {
                        "type": "string",
                        "description": "Optional 'owner/repo' to target. Omit to use the \
                                        repository of the current working directory."
                    },
                    "log_failed": {
                        "type": "boolean",
                        "description": "Include the log output of failed steps only \
                                        (default false)."
                    }
                },
                "required": ["run_id"],
                "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(run_id) = args.get("run_id").and_then(Value::as_str) else {
            return ToolResult::err("'run_id' is required");
        };
        let run_id = match plain_arg("run_id", run_id) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(e),
        };
        let mut argv = vec!["run".to_string(), "view".to_string(), run_id];
        if let Some(repo) = args.get("repo").and_then(Value::as_str) {
            match repo_arg(repo) {
                Ok(v) => argv.extend(["--repo".to_string(), v]),
                Err(e) => return ToolResult::err(e),
            }
        }
        if args.get("log_failed").and_then(Value::as_bool) == Some(true) {
            argv.push("--log-failed".to_string());
        }
        run_gh(&self.root, &argv, false).await
    }
}
