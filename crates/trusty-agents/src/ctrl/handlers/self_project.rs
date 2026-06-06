//! Self-project (trusty-agents running from its own checkout) CTRL tools.
//!
//! Why: When CTRL runs from trusty-agents's own checkout, the user can dispatch
//! self-development tasks without manually adding the project — these tools
//! surface that affordance.
//! What: `SelfProjectStatusTool`, `InitiateSelfTaskTool`, plus the small
//! `read_self_version` and `read_recent_git_log` helpers they share.
//! Test: `self_project_status_returns_version_when_path_set`,
//! `initiate_self_task_queues_self_project_path`,
//! `self_project_status_errors_when_no_self_path`,
//! `initiate_self_task_errors_when_no_self_path`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// `self_project_status()` — return version + recent commits for the
/// detected trusty-agents self-project. (#182)
///
/// Why: Lets the user (or the LLM) inspect what version is running and what
/// the most recent commits did, without leaving the CTRL prompt.
/// What: Reads `[package] version` from `<self_path>/Cargo.toml`, runs
/// `git -C <self_path> log --oneline -3`, and returns the JSON envelope.
/// Test: `self_project_status_returns_version_when_path_set`.
pub(crate) struct SelfProjectStatusTool {
    pub(crate) self_path: Option<PathBuf>,
}

#[async_trait]
impl ToolExecutor for SelfProjectStatusTool {
    fn name(&self) -> &str {
        "self_project_status"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "self_project_status",
                "description": "Report version + last 3 git commits for the trusty-agents self-project (when running from its own checkout).",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let Some(path) = self.self_path.as_ref() else {
            return ToolResult::err(
                "self_project_status: no self-project detected (not running from trusty-agents checkout)",
            );
        };
        let version = read_self_version(path).unwrap_or_else(|_| "unknown".to_string());
        let log = read_recent_git_log(path, 3)
            .await
            .unwrap_or_else(|e| format!("(git unavailable: {e})"));
        let body = json!({
            "version": version,
            "self_project_path": path.display().to_string(),
            "git_log": log,
        });
        ToolResult::ok(body.to_string())
    }
}

/// Read `[package] version = "..."` from `<path>/Cargo.toml`. (#182)
///
/// Why: We can't depend on `env!("CARGO_PKG_VERSION")` for the *target*
/// project — that's the version of whatever crate compiled this binary,
/// which only matches when the running binary was built from the detected
/// self-project. Reading the file lets a remote or stale binary report the
/// correct on-disk version.
pub(crate) fn read_self_version(self_path: &Path) -> Result<String> {
    let cargo_toml = self_path.join("Cargo.toml");
    let text = std::fs::read_to_string(&cargo_toml)
        .with_context(|| format!("read {}", cargo_toml.display()))?;
    let parsed: toml::Value = toml::from_str(&text).context("parse Cargo.toml")?;
    let v = parsed
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .context("Cargo.toml missing [package] version")?;
    Ok(v.to_string())
}

/// Run `git -C <path> log --oneline -<n>` and return stdout. (#182)
pub(crate) async fn read_recent_git_log(self_path: &Path, n: usize) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(self_path)
        .arg("log")
        .arg("--oneline")
        .arg(format!("-{n}"))
        .output()
        .await
        .context("spawn git log")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        anyhow::bail!("git log failed: {err}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `initiate_self_task(task)` — queue a `start_pm` against the self-project.
/// (#182)
///
/// Why: Single-call shortcut for the common "ask CTRL to work on itself"
/// pattern. Reuses the `start_pm` queueing path so the post-tool drain in
/// `ctrl_chat_turn` actually spawns the PM and connects to it.
/// What: Captures `task` text in a shared slot; the caller is responsible
/// for forwarding it to the PM after the connection is established. We also
/// queue the self-project path into the existing `pending_connect` slot so
/// the CTRL turn-completion logic spawns the PM.
/// Test: `initiate_self_task_queues_self_project_path`.
pub(crate) struct InitiateSelfTaskTool {
    pub(crate) self_path: Option<PathBuf>,
    pub(crate) pending_connect: Arc<Mutex<Option<String>>>,
    pub(crate) pending_self_task: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ToolExecutor for InitiateSelfTaskTool {
    fn name(&self) -> &str {
        "initiate_self_task"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "initiate_self_task",
                "description": "Start (or attach to) a PM for the trusty-agents self-project and queue this task for it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "Development task to run against trusty-agents itself." }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path) = self.self_path.as_ref() else {
            return ToolResult::err(
                "initiate_self_task: no self-project detected (not running from trusty-agents checkout)",
            );
        };
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return ToolResult::err("initiate_self_task: missing 'task'");
        };
        let path_str = path.display().to_string();
        if let Ok(mut slot) = self.pending_connect.lock() {
            *slot = Some(path_str.clone());
        }
        if let Ok(mut slot) = self.pending_self_task.lock() {
            *slot = Some(task.to_string());
        }
        ToolResult::ok(format!("queued self-task against {path_str}"))
    }
}
