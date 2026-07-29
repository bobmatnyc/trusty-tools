//! Subprocess-based `AgentRunner` — re-invokes this binary in `--agent` mode.
//!
//! Why: Sub-agents run as isolated processes (one task per spawn) to bound
//! resource use and keep model/tool state per-task. The `AgentRunner` trait
//! hides this pathway so the workflow engine and PM loop don't care about
//! process boundaries.
//! What: `SubprocessAgentRunner::run()` spawns `current_exe --agent <name>`,
//! writes one NDJSON `Task` to stdin, reads one NDJSON line from stdout,
//! unwraps `IpcMessage::Result` into its `content`, or surfaces an Err.
//! Test: Integration-tested via the full PM/workflow flow. Trait-level unit
//! tests use an in-process mock runner.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — `SubprocessAgentRunner` + `AgentRunner` trait impl (policy)
//! - `spawn.rs` — `Command` setup, NDJSON IPC round-trip, mistake logging
//! - `tests.rs` — unit tests for the rescue / non-zero-exit paths

mod spawn;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::ipc::IpcMessage;
use crate::memory::{AgentSession, MemoryGraph};
use crate::session::HistoryMessage;
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};

use spawn::{SpawnConfig, spawn_and_run_inner, spawn_subagent_with_config_dir};

// Re-export the public spawn entry points so callers continue to use
// `crate::subprocess::{spawn_subagent_and_run, ...}` after the split.
pub use spawn::{
    spawn_subagent_and_run, spawn_subagent_and_run_with_env, spawn_subagent_and_run_with_full_env,
    spawn_subagent_and_run_with_full_env_ctx,
};

/// Production `AgentRunner` that spawns the current executable as a subprocess.
pub struct SubprocessAgentRunner {
    /// Optional out_dir forwarded via `TAGENT_OUT_DIR` env var so sub-agents
    /// can use file-scoped tools (e.g. `advance_workflow_phase`).
    out_dir: Option<PathBuf>,
    /// #222: Optional code_dir forwarded via `TAGENT_CODE_DIR` env var. When
    /// set, code-writing tools (e.g. `write_file` for the code-agent) root at
    /// this path instead of `out_dir`. When unset, tools fall back to
    /// `TAGENT_OUT_DIR` for backward compatibility.
    code_dir: Option<PathBuf>,
    /// Optional memory graph. When set, every successful IPC round-trip is
    /// recorded as an `AgentSession`. Memory failures are swallowed so they
    /// never crash the main agent loop.
    memory: Option<Arc<MemoryGraph>>,
    /// Optional config directory forwarded via `TAGENT_CONFIG_DIR` env var
    /// to child processes so agents in different projects load the right TOML.
    config_dir: Option<PathBuf>,
    /// #410: Project directory (the user's source tree). Forwarded to children
    /// as `TAGENT_PROJECT_DIR`. When set on the runner and `ctx.working_dir`
    /// is None, this also becomes the spawned child's CWD so agent tools (read,
    /// search, edit) operate against the project source files rather than the
    /// artifacts directory.
    project_dir: Option<PathBuf>,
    /// Security fix (delegate_to_agent injection-to-RCE path): glob patterns
    /// forwarded to the child as `TAGENT_DELEGATION_TAINT_ALLOW` (JSON array).
    /// `Some(patterns)` — including `Some(vec![])` — marks every spawn this
    /// runner performs as a TAINTED delegation: the child's `run_subagent`
    /// applies `patterns` via `scope_assistant_allowed_tools` regardless of
    /// the child's own declared role or `[tools].allow`, narrowing it to
    /// (at most) the delegator's own tool posture. `None` (every caller
    /// except `build_assistant_tier_registry`) is byte-identical to before
    /// this field existed. See `with_delegation_taint`.
    delegation_taint_allow: Option<Vec<String>>,
}

impl SubprocessAgentRunner {
    pub fn new() -> Self {
        Self {
            out_dir: None,
            code_dir: None,
            memory: None,
            config_dir: None,
            project_dir: None,
            delegation_taint_allow: None,
        }
    }

    /// Builder-style constructor that sets the out_dir forwarded to sub-agents.
    pub fn with_out_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.out_dir = dir;
        self
    }

    /// #222: Set the code dir forwarded to sub-agents as `TAGENT_CODE_DIR`.
    ///
    /// Why: When `--project-dir` is set the user's project tree is the
    /// destination for generated source code, while `TAGENT_OUT_DIR`
    /// remains the artifacts directory. Forwarding both lets the code-agent's
    /// `write_file` tool root at the right place without making every other
    /// agent care about the distinction.
    /// What: Stores the path; applied later by `spawn_*` helpers via
    /// `Command::env("TAGENT_CODE_DIR", …)` when present.
    /// Test: Exercised end-to-end by the workflow integration; child
    /// processes observe the env var via `std::env::var_os` in main.rs.
    pub fn with_code_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.code_dir = dir;
        self
    }

    /// Builder-style setter for the config directory forwarded to sub-agents.
    ///
    /// Why: The CTRL CLI spawns PM actors for multiple project paths; each PM
    /// must spawn its own sub-agents pointing at the right `config/agents/`
    /// directory. Passing it here sets `TAGENT_CONFIG_DIR` on every child
    /// process without touching the parent's environment.
    /// What: Sets `self.config_dir`; applied in `run_with_history` and
    /// `run_with_context` via `spawn_subagent_with_config_dir`.
    /// Test: Used by `ctrl::run_pm_task`; child process receives `TAGENT_CONFIG_DIR`.
    pub fn with_config_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.config_dir = dir;
        self
    }

    /// #410: Set the project directory forwarded to sub-agents.
    ///
    /// Why: Workflows invoked with `--workflow ... --out-dir <artifacts>`
    /// previously left agents with `CWD = out_dir` so their file-reading tools
    /// could not see the user's source tree. Threading a separate
    /// `project_dir` lets the harness keep artifacts in `out_dir` while every
    /// agent runs with its CWD anchored at the actual project root.
    /// What: Stores the path; applied later by `spawn_*` helpers via
    /// `Command::env("TAGENT_PROJECT_DIR", …)` and (when `ctx.working_dir`
    /// is unset) `Command::current_dir(project_dir)`.
    /// Test: `cargo test --workspace` covers subprocess wiring; downstream
    /// behavior verified end-to-end by the workflow integration tests.
    pub fn with_project_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.project_dir = dir;
        self
    }

    /// Mark every spawn this runner performs as a TAINTED delegation
    /// (security fix — delegate_to_agent injection-to-RCE path).
    ///
    /// Why: `scope_assistant_allowed_tools` only ever narrowed a spawned
    /// child's tool registry when the CHILD's own role was `"assistant"`
    /// (see `runtime::tool_registry::scope_assistant_allowed_tools`). An
    /// assistant-tier persona holding live external-content tools
    /// (`get_gmail_message_content`, `web_search`, …) plus `delegate_to_agent`
    /// could hand attacker-controlled task text to `delegate_to_agent(
    /// agent_name="engineer", ...)` — `engineer` declares no `[tools].allow`
    /// at all, so the spawned `claude-code` subprocess got its full
    /// unrestricted tool surface (shell, file write) with no narrowing
    /// whatsoever. Calling this on the runner backing an assistant-tier
    /// `DelegateToAgentTool` closes that: every spawn is tagged with the
    /// delegator's own allow patterns regardless of the target's role.
    /// What: Stores `patterns`; forwarded to the child as
    /// `TAGENT_DELEGATION_TAINT_ALLOW` (JSON array) by `spawn_and_run_inner`.
    /// `run_subagent` reads it back and applies it via
    /// `scope_assistant_allowed_tools(true, ..., patterns, ...)`
    /// unconditionally — i.e. EVERY assistant-tier delegation is tainted,
    /// not only ones whose turn happened to touch a live-fetch tool (the
    /// simpler, fail-safe posture: per-turn tracking would still miss
    /// content fetched in an earlier turn and still present in history).
    /// `None` (every caller except `build_assistant_tier_registry`) leaves
    /// behavior byte-identical to before this field existed.
    /// Test: `delegation_taint_allow_env_set_when_configured`,
    /// `delegation_taint_allow_env_absent_by_default`,
    /// `delegation_taint_allow_env_empty_vec_still_sets_deny_all`
    /// (`subprocess::tests`, exercising `spawn::apply_delegation_taint_env`
    /// directly against a real `Command`); `scope_assistant_allowed_tools_*`
    /// (`tool_registry_tests`).
    pub fn with_delegation_taint(mut self, patterns: Option<Vec<String>>) -> Self {
        self.delegation_taint_allow = patterns;
        self
    }

    /// Builder-style constructor that attaches a memory graph for auto-capture.
    ///
    /// Why: Wiring memory at construction keeps the runner API otherwise
    /// unchanged while letting callers opt in (tests, ephemeral runs can
    /// omit it).
    /// What: Stores the `Arc<MemoryGraph>` used by `run()` after a successful
    /// IPC round-trip.
    /// Test: Memory failures never fail the run — `run()` calls `.ok()` on
    /// the record call; exercised indirectly via integration runs.
    #[allow(dead_code)]
    pub fn with_memory(mut self, memory: Option<Arc<MemoryGraph>>) -> Self {
        self.memory = memory;
        self
    }
}

impl Default for SubprocessAgentRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for SubprocessAgentRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        self.run_with_history(agent_name, task, &[], &RunContext::default())
            .await
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        // CRIT-1 / #90: Per-invocation state (assigned file, max-turns cap,
        // working dir) is applied to the child process only via `Command::env`
        // / `Command::current_dir`, never via `std::env::set_var` on the
        // parent — that is unsound under multi-threaded tokio.
        let msg = if let Some(config_dir) = &self.config_dir {
            spawn_subagent_with_config_dir(
                agent_name,
                task,
                self.out_dir.as_deref(),
                self.code_dir.as_deref(),
                self.project_dir.as_deref(),
                &[],
                config_dir,
                Some(ctx),
                self.delegation_taint_allow.as_deref(),
            )
            .await?
        } else {
            // #222: When no config_dir is wired (test/legacy paths), the
            // simpler helper still doesn't carry code_dir. Fall back to
            // spawn_and_run_inner directly so we can forward both env vars.
            spawn_and_run_inner(SpawnConfig {
                agent_name,
                task,
                out_dir: self.out_dir.as_deref(),
                code_dir: self.code_dir.as_deref(),
                history: &[],
                ctx: Some(ctx),
                config_dir: None,
                project_dir: self.project_dir.as_deref(),
                delegation_taint_allow: self.delegation_taint_allow.as_deref(),
            })
            .await?
        };
        self.handle_msg(msg, agent_name, task).await
    }

    async fn run_with_history(
        &self,
        agent_name: &str,
        task: &str,
        history: &[HistoryMessage],
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        // Bug #122: thread `ctx` (working_dir, model, max_turns_override)
        // into the subprocess so persistent-session agents honour the same
        // per-invocation overrides that the non-persistent path already does.
        let msg = if let Some(config_dir) = &self.config_dir {
            spawn_subagent_with_config_dir(
                agent_name,
                task,
                self.out_dir.as_deref(),
                self.code_dir.as_deref(),
                self.project_dir.as_deref(),
                history,
                config_dir,
                Some(ctx),
                self.delegation_taint_allow.as_deref(),
            )
            .await?
        } else {
            spawn_and_run_inner(SpawnConfig {
                agent_name,
                task,
                out_dir: self.out_dir.as_deref(),
                code_dir: self.code_dir.as_deref(),
                history,
                ctx: Some(ctx),
                config_dir: None,
                project_dir: self.project_dir.as_deref(),
                delegation_taint_allow: self.delegation_taint_allow.as_deref(),
            })
            .await?
        };
        self.handle_msg(msg, agent_name, task).await
    }
}

impl SubprocessAgentRunner {
    /// Why: Shared post-IPC handling (memory capture + output mapping) used
    /// by both `run_with_history` and `run_with_context`. Extracted to avoid
    /// duplicating the same match/bail/memory logic.
    /// What: Converts an `IpcMessage::Result` into an `AgentOutput`, records
    /// a memory session if a graph is attached, and bails on error/task
    /// variants.
    /// Test: Exercised indirectly through every runner integration test.
    async fn handle_msg(
        &self,
        msg: IpcMessage,
        agent_name: &str,
        task: &str,
    ) -> Result<AgentOutput> {
        match msg {
            IpcMessage::Result {
                content,
                summary,
                usage,
                ..
            } => {
                if let Some(memory) = &self.memory {
                    let session = AgentSession {
                        id: uuid::Uuid::new_v4().to_string(),
                        agent_name: agent_name.to_string(),
                        workflow_run_id: crate::env_compat::env_var(
                            "TAGENT_RUN_ID",
                            "OPEN_MPM_RUN_ID",
                        )
                        .unwrap_or_default(),
                        phase: crate::env_compat::env_var("TAGENT_PHASE", "OPEN_MPM_PHASE")
                            .unwrap_or_else(|_| "interactive".to_string()),
                        prompt: task.to_string(),
                        response: content.clone(),
                        timestamp: chrono::Utc::now(),
                        parent_id: None,
                        segment: None,
                    };
                    if let Err(e) = memory.record(session).await {
                        tracing::warn!(error = %e, agent = %agent_name, "memory.record failed; continuing");
                    }
                }
                Ok(AgentOutput {
                    content,
                    summary,
                    usage: usage.unwrap_or_default(),
                })
            }
            IpcMessage::Error { error, .. } => bail!("sub-agent '{agent_name}' error: {error}"),
            IpcMessage::Task { .. } => {
                bail!("sub-agent '{agent_name}' returned unexpected Task message")
            }
        }
    }
}
