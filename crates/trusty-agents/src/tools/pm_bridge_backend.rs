//! Backend seam for `dispatch_task` (epic #3052, PR B, lane 3): routes a
//! `BridgeRoute` to either `tcode` (one-shot subprocess) or `tm` (MCP over
//! stdio) and returns its raw output for the tool layer to scrub and return.
//!
//! Why: `PmBridgeTool` (see `crate::tools::pm_bridge`) must be testable
//! without spawning real subprocesses — mirrors the `AgentRunner` DI seam
//! `DelegateToAgentTool` already uses (`crate::tools::traits::AgentRunner`,
//! tested via `delegate.rs`'s `RecordingRunner`). `PmBridgeBackend` is the
//! same shape: one async method, object-safe, `Send + Sync`.
//! What: `PmBridgeBackend` is the trait; `ProcessPmBridge` is the production
//! implementation. Tcode invocation is a one-shot blocking subprocess —
//! `tcode run-task pm <task> --project <dir> --json` (`crates/trusty-code/
//! src/main.rs`'s `RunTask` variant, `crates/trusty-code/src/cli/run_task.rs`)
//! — which itself blocks until the daemon-owned session reaches a terminal
//! state before exiting, so a single subprocess call is genuinely
//! synchronous end-to-end. Tm invocation spawns `tm serve --stdio`
//! (`StdioMcpClient::spawn`) and calls the managed-session lifecycle tools;
//! see `run_tm`'s docs for why this is a documented MVP deviation from a
//! true one-shot RPC (tm has none — see the docs there).
//! Test: `pm_bridge_backend_tests` covers `binary_on_path`, graceful
//! "binary absent" errors for both routes, and binary-gated integration
//! smokes against the real `tcode`/`tm` binaries when present on PATH.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::sleep;

use crate::intent::route::BridgeRoute;
use crate::plugins::stdio_mcp::StdioMcpClient;

/// How many times `run_tm` polls `session_activity` before giving up and
/// returning whatever transcript has been observed so far.
///
/// Why: tm's managed-session lifecycle is inherently async (tmux-attached,
/// provisioned from a git clone) — there is no synchronous "run this and
/// block until done" RPC (see `run_tm`'s docs). A bounded poll keeps
/// `dispatch_task` from hanging the calling LLM turn indefinitely while
/// still giving a fast-completing task a real chance to finish.
const MAX_POLL_ATTEMPTS: u32 = 10;

/// Delay between `session_activity` polls.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Number of trailing tmux pane lines requested per `session_activity` poll.
const ACTIVITY_LINES: u64 = 200;

/// Executes a routed `dispatch_task` call against a real backend.
///
/// Why: see the module docs — the DI seam that lets `PmBridgeTool` be unit
/// tested with a `RecordingBackend` instead of spawning real subprocesses.
/// What: `run` takes the already-decided `route` (never re-derives it) and
/// the raw task text, and returns the backend's raw (un-scrubbed) output.
/// Scrubbing is the TOOL layer's job (`crate::tools::pm_bridge::scrub_branding`),
/// not the backend's — keeping backend identity out of this trait's contract
/// would be a false promise given `ProcessPmBridge` inherently talks to
/// processes named `tcode`/`tm`.
/// Test: `RecordingBackend` in `pm_bridge_tests.rs`; `ProcessPmBridge` in
/// `pm_bridge_backend_tests.rs`.
#[async_trait]
pub trait PmBridgeBackend: Send + Sync {
    async fn run(&self, route: BridgeRoute, task: &str) -> Result<String>;
}

/// Production `PmBridgeBackend`: subprocess `tcode` for `Tcode`, MCP-over-
/// stdio `tm` for `Tm`.
pub struct ProcessPmBridge {
    project_dir: PathBuf,
}

impl ProcessPmBridge {
    /// Construct a backend rooted at `project_dir` — the repo/project both
    /// backends operate against.
    ///
    /// Why: `dispatch_task` always acts on the calling session's own
    /// project, never an LLM-supplied path (that would be an injection
    /// vector); the project directory is threaded in once at registration
    /// time (see `crate::ctrl::pm_task::dispatch::history` and
    /// `crate::runtime::pm_mode`), not read from tool arguments.
    /// What: stores `project_dir` for later use by both `run_tcode` and
    /// `run_tm`.
    /// Test: `process_pm_bridge_tcode_route_fails_closed_without_binary`,
    /// `process_pm_bridge_tm_route_fails_closed_without_binary`.
    pub fn from_project(project_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_dir: project_dir.into(),
        }
    }

    /// Run the `Tcode` route: `tcode run-task pm <task> --project <dir> --json`.
    ///
    /// Why: see the module docs — this subcommand blocks until the
    /// daemon-owned session it creates reaches a terminal state, so one
    /// subprocess call is a genuine synchronous round trip. `--json` emits
    /// the final `Session` snapshot (id/status/mode) as the process's only
    /// stdout; it is NOT a full per-step transcript (no tool-call/diff
    /// detail) — a documented scope limit, see the module docs' pointer to
    /// `cli/run_task.rs`.
    /// What: spawns the subprocess, waits for it to exit, and returns
    /// stdout (falling back to stderr if stdout is empty) as the raw
    /// transcript. Fails closed with a generic-shaped error when `tcode` is
    /// not on PATH, mirroring `TrustySearchPlugin::try_spawn`'s
    /// graceful-degradation contract.
    /// Test: `process_pm_bridge_tcode_route_fails_closed_without_binary`;
    /// `tcode_route_smoke` (binary-gated).
    async fn run_tcode(&self, task: &str) -> Result<String> {
        if !binary_on_path("tcode") {
            bail!("tcode backend unavailable: binary not found on PATH");
        }
        let output = Command::new("tcode")
            .arg("run-task")
            .arg("pm")
            .arg(task)
            .arg("--project")
            .arg(&self.project_dir)
            .arg("--json")
            .output()
            .await
            .context("failed to spawn tcode run-task subprocess")?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() && stdout.trim().is_empty() {
            bail!(
                "tcode run-task exited with {}: {}",
                output.status,
                stderr.trim()
            );
        }
        Ok(if stdout.trim().is_empty() {
            stderr
        } else {
            stdout
        })
    }

    /// Run the `Tm` route: spawn `tm serve --stdio` and drive the
    /// managed-session lifecycle.
    ///
    /// Why (MVP DEVIATION — flagged design call): the owner's default
    /// suggestion was `agent_delegate`, but that tool is explicitly
    /// documented on the tm side as "a TRACKING/GATING companion, NOT an
    /// execution path — it does not spawn the agent"
    /// (`crates/trusty-mpm/src/mcp/tools/core.rs`). It cannot execute
    /// anything. The only tm MCP surface that actually RUNS work is the
    /// managed-session lifecycle (`session_new` / `session_activity` /
    /// `session_decommission`), which is inherently asynchronous and
    /// tmux-attached (it clones a repo into a fresh workspace and launches
    /// a full harness) — there is no synchronous "run this task headlessly
    /// and return the result" RPC on tm today. This implementation spawns
    /// an EPHEMERAL managed session against `project_dir` (as a `file://`
    /// URL) and does a BOUNDED poll of `session_activity` for its tmux pane
    /// content, then best-effort tears the session down. For a task that
    /// completes within the poll window this returns real output; for a
    /// longer-running orchestration task it returns a partial transcript.
    /// Recommend filing a follow-up for a proper synchronous tm "run task"
    /// RPC so this bridge does not have to reimplement session polling.
    /// What: `session_new(repo_url=file://<project_dir>, ref=HEAD, task,
    /// ephemeral=true)` -> poll `session_activity` up to
    /// `MAX_POLL_ATTEMPTS` times, `POLL_INTERVAL` apart, capturing the pane
    /// content and stopping early once `runtime_active` goes false ->
    /// best-effort `session_decommission` -> return the last captured pane
    /// content.
    /// Test: `process_pm_bridge_tm_route_fails_closed_without_binary`;
    /// `tm_route_smoke` (binary-gated).
    async fn run_tm(&self, task: &str) -> Result<String> {
        if !binary_on_path("tm") {
            bail!("tm backend unavailable: binary not found on PATH");
        }
        let mut client = StdioMcpClient::spawn("tm", &["serve", "--stdio"], "trusty-agents")
            .await
            .context("failed to spawn tm serve --stdio")?;
        client
            .initialize()
            .await
            .context("tm MCP handshake failed")?;

        let repo_url = format!("file://{}", self.project_dir.display());
        let new_result = client
            .call_tool(
                "session_new",
                json!({
                    "repo_url": repo_url,
                    "ref": "HEAD",
                    "task": task,
                    "ephemeral": true,
                }),
            )
            .await
            .context("tm session_new failed")?;
        let session_id = extract_session_id(&new_result)
            .context("tm session_new response did not include a session id")?;

        let mut transcript = String::new();
        for _ in 0..MAX_POLL_ATTEMPTS {
            sleep(POLL_INTERVAL).await;
            match client
                .call_tool(
                    "session_activity",
                    json!({ "session_id": session_id, "lines": ACTIVITY_LINES }),
                )
                .await
            {
                Ok(activity) => {
                    if let Some(pane) = extract_pane_content(&activity) {
                        transcript = pane;
                    }
                    if !is_runtime_active(&activity) {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "tm session_activity poll failed");
                    break;
                }
            }
        }

        // Best-effort cleanup — an ephemeral session left running is
        // eventually reaped, but tearing it down promptly avoids leaking
        // fleet capacity across many dispatch_task calls. Never fails the
        // overall call, but a failure here MUST be logged (code-critic HIGH
        // finding 3): silently swallowing it left zero trace of an orphaned
        // session/workspace, undiagnosable in production.
        if let Err(e) = client
            .call_tool("session_decommission", json!({ "session_id": session_id }))
            .await
        {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "tm session_decommission failed; session may be orphaned"
            );
        }

        if transcript.trim().is_empty() {
            bail!("tm session produced no observable output within the poll window");
        }
        Ok(transcript)
    }
}

#[async_trait]
impl PmBridgeBackend for ProcessPmBridge {
    async fn run(&self, route: BridgeRoute, task: &str) -> Result<String> {
        match route {
            BridgeRoute::Tcode => self.run_tcode(task).await,
            BridgeRoute::Tm => self.run_tm(task).await,
        }
    }
}

/// Decide whether `name` is on `$PATH`, without spawning a subprocess.
///
/// Why: same rationale as `plugins::trusty_search::binary_on_path` (avoid a
/// `which` fork/exec on every check) — kept as a small local copy rather
/// than reusing that one because it is `pub(super)`-scoped to `plugins` and
/// this module lives in `tools`, a sibling tree; duplicating ~5 lines is
/// cheaper than widening that visibility for one caller.
/// What: returns true iff `<dir>/<name>` is a regular file for some `dir` on
/// `$PATH`.
/// Test: `binary_on_path_recognises_sh` in `pm_bridge_backend_tests.rs`.
pub(super) fn binary_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
        .unwrap_or(false)
}

/// Pull a session id out of an MCP `tools/call` result for `session_new`.
///
/// Why: the tm daemon wraps its JSON result as `{"content":[{"type":"text",
/// "text":"<json>"}]}` (see `crates/trusty-mpm/src/mcp/mod.rs`'s
/// `success_result`), matching the same convention
/// `plugins::trusty_search::extract_text_results` already decodes.
/// What: parses the first `content[].text` frame as JSON and returns its
/// `id` (falling back to `session_id`) field as a string.
/// Test: `extract_session_id_parses_wrapped_text_frame`,
/// `extract_session_id_missing_id_returns_none`.
fn extract_session_id(call_tool_result: &Value) -> Option<String> {
    let inner = decode_first_text_frame(call_tool_result)?;
    inner
        .get("id")
        .or_else(|| inner.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Pull the raw tmux pane content out of a `session_activity` MCP result.
///
/// Why: `session_activity`'s daemon-side payload carries `pane_content`
/// (the raw trailing tmux lines) alongside structured lifecycle fields —
/// this is the closest thing tm has to a "transcript" for a managed
/// session.
/// What: decodes the wrapped text frame (see `extract_session_id`) and
/// returns its `pane_content` field, or `None` if absent/non-string.
/// Test: `extract_pane_content_parses_wrapped_text_frame`.
fn extract_pane_content(call_tool_result: &Value) -> Option<String> {
    let inner = decode_first_text_frame(call_tool_result)?;
    inner
        .get("pane_content")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Whether a `session_activity` result reports the runtime as still active.
///
/// Why: the poll loop in `run_tm` stops early once the session's harness is
/// done rather than always burning the full `MAX_POLL_ATTEMPTS` window.
/// What: decodes the wrapped text frame and reads its `runtime_active`
/// boolean field, defaulting to `true` (keep polling) when absent/malformed
/// — a fail-safe default that costs at most a few extra poll iterations
/// rather than truncating a still-running session's transcript.
/// Test: `is_runtime_active_defaults_true_when_absent`.
fn is_runtime_active(call_tool_result: &Value) -> bool {
    decode_first_text_frame(call_tool_result)
        .and_then(|inner| inner.get("runtime_active").and_then(Value::as_bool))
        .unwrap_or(true)
}

/// Shared decode step for `extract_session_id` / `extract_pane_content` /
/// `is_runtime_active`: parse the first `content[].text` frame of an MCP
/// `tools/call` result as JSON.
fn decode_first_text_frame(call_tool_result: &Value) -> Option<Value> {
    let text = call_tool_result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)?;
    serde_json::from_str::<Value>(text).ok()
}

#[cfg(test)]
#[path = "pm_bridge_backend_tests.rs"]
mod pm_bridge_backend_tests;
