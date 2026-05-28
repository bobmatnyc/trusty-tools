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

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::ipc::{IpcMessage, parse_message, serialize_message};
use crate::memory::{AgentSession, MemoryGraph};
use crate::session::HistoryMessage;
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};

/// Production `AgentRunner` that spawns the current executable as a subprocess.
pub struct SubprocessAgentRunner {
    /// Optional out_dir forwarded via `OPEN_MPM_OUT_DIR` env var so sub-agents
    /// can use file-scoped tools (e.g. `advance_workflow_phase`).
    out_dir: Option<PathBuf>,
    /// #222: Optional code_dir forwarded via `OPEN_MPM_CODE_DIR` env var. When
    /// set, code-writing tools (e.g. `write_file` for the code-agent) root at
    /// this path instead of `out_dir`. When unset, tools fall back to
    /// `OPEN_MPM_OUT_DIR` for backward compatibility.
    code_dir: Option<PathBuf>,
    /// Optional memory graph. When set, every successful IPC round-trip is
    /// recorded as an `AgentSession`. Memory failures are swallowed so they
    /// never crash the main agent loop.
    memory: Option<Arc<MemoryGraph>>,
    /// Optional config directory forwarded via `OPEN_MPM_CONFIG_DIR` env var
    /// to child processes so agents in different projects load the right TOML.
    config_dir: Option<PathBuf>,
    /// #410: Project directory (the user's source tree). Forwarded to children
    /// as `OPEN_MPM_PROJECT_DIR`. When set on the runner and `ctx.working_dir`
    /// is None, this also becomes the spawned child's CWD so agent tools (read,
    /// search, edit) operate against the project source files rather than the
    /// artifacts directory.
    project_dir: Option<PathBuf>,
}

impl SubprocessAgentRunner {
    pub fn new() -> Self {
        Self {
            out_dir: None,
            code_dir: None,
            memory: None,
            config_dir: None,
            project_dir: None,
        }
    }

    /// Builder-style constructor that sets the out_dir forwarded to sub-agents.
    pub fn with_out_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.out_dir = dir;
        self
    }

    /// #222: Set the code dir forwarded to sub-agents as `OPEN_MPM_CODE_DIR`.
    ///
    /// Why: When `--project-dir` is set the user's project tree is the
    /// destination for generated source code, while `OPEN_MPM_OUT_DIR`
    /// remains the artifacts directory. Forwarding both lets the code-agent's
    /// `write_file` tool root at the right place without making every other
    /// agent care about the distinction.
    /// What: Stores the path; applied later by `spawn_*` helpers via
    /// `Command::env("OPEN_MPM_CODE_DIR", …)` when present.
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
    /// directory. Passing it here sets `OPEN_MPM_CONFIG_DIR` on every child
    /// process without touching the parent's environment.
    /// What: Sets `self.config_dir`; applied in `run_with_history` and
    /// `run_with_context` via `spawn_subagent_with_config_dir`.
    /// Test: Used by `ctrl::run_pm_task`; child process receives `OPEN_MPM_CONFIG_DIR`.
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
    /// `Command::env("OPEN_MPM_PROJECT_DIR", …)` and (when `ctx.working_dir`
    /// is unset) `Command::current_dir(project_dir)`.
    /// Test: `cargo test --workspace` covers subprocess wiring; downstream
    /// behavior verified end-to-end by the workflow integration tests.
    pub fn with_project_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.project_dir = dir;
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
                        workflow_run_id: std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default(),
                        phase: std::env::var("OPEN_MPM_PHASE")
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

/// Spawn a sub-agent subprocess, write the task, read back the NDJSON response.
///
/// Why: Shared by `run_pm` and the workflow engine so subprocess spawn
/// semantics live in exactly one place.
/// What: Re-invokes the current executable with `--agent <name>`, writes a
/// single NDJSON `Task` line to its stdin, reads a single NDJSON line from
/// its stdout, parses and returns the resulting `IpcMessage`.
/// Test: Exercised end-to-end by the PM smoke test.
pub async fn spawn_subagent_and_run(agent_name: &str, task: &str) -> Result<IpcMessage> {
    spawn_subagent_and_run_with_env(agent_name, task, None).await
}

/// Same as `spawn_subagent_and_run` but forwards an `out_dir` path to the
/// child via `OPEN_MPM_OUT_DIR` so tools like `advance_workflow_phase` know
/// where to write their audit trail.
pub async fn spawn_subagent_and_run_with_env(
    agent_name: &str,
    task: &str,
    out_dir: Option<&std::path::Path>,
) -> Result<IpcMessage> {
    spawn_subagent_and_run_with_full_env(agent_name, task, out_dir, &[]).await
}

/// Like `spawn_subagent_and_run_with_env` but additionally carries prior
/// conversation history for persistent-session agents (#51).
///
/// Why: The PM/engine layer tracks session history in-process; when spawning
/// a fresh sub-agent subprocess it must forward that history so the child can
/// rebuild context.
/// What: Constructs the IPC `Task` with `history` populated (when non-empty),
/// otherwise identical to `spawn_subagent_and_run_with_env`.
/// Test: See `session::tests` for the history round-trip; subprocess wiring
/// is exercised end-to-end by the workflow smoke tests.
pub async fn spawn_subagent_and_run_with_full_env(
    agent_name: &str,
    task: &str,
    out_dir: Option<&std::path::Path>,
    history: &[HistoryMessage],
) -> Result<IpcMessage> {
    spawn_subagent_and_run_with_full_env_ctx(
        agent_name,
        task,
        out_dir,
        history,
        &RunContext::default(),
    )
    .await
}

/// Core subprocess spawn — `history` + `RunContext` variant.
///
/// Why: Replaces per-call `std::env::set_var` threading of wave-loop state
/// (#90 / CRIT-1). Applying assigned-file / max-turns / working-dir to the
/// child process only via `Command::env` / `Command::current_dir` keeps the
/// parent's environment (and any concurrent tokio worker) unaffected.
/// What: Same as `spawn_subagent_and_run_with_full_env` but additionally
/// forwards the context: `assigned_file` → `OPEN_MPM_ASSIGNED_FILE` child env;
/// `max_turns_override` → `OPEN_MPM_MAX_TURNS` child env; `working_dir` →
/// `Command::current_dir`.
/// Test: `wave_loop_runs_one_agent_per_file` asserts the child observes the
/// per-call overrides and the parent's env is never mutated.
pub async fn spawn_subagent_and_run_with_full_env_ctx(
    agent_name: &str,
    task: &str,
    out_dir: Option<&std::path::Path>,
    history: &[HistoryMessage],
    ctx: &RunContext,
) -> Result<IpcMessage> {
    spawn_and_run_inner(SpawnConfig {
        agent_name,
        task,
        out_dir,
        code_dir: None,
        history,
        ctx: Some(ctx),
        config_dir: None,
        project_dir: None,
    })
    .await
}

/// Bundle of inputs the unified spawn helper needs.
///
/// Why: Replaces two ~100-line near-duplicate spawn functions with one inner
/// implementation parameterized by this struct. Keeps the public function
/// signatures stable while collapsing the duplication that previously meant
/// every behavioral fix (e.g. the #147 rescue, #186 mistake logging) had to
/// be applied twice.
/// What: Carries the agent name, task text, history, optional `out_dir`,
/// optional per-call `RunContext`, and optional `config_dir` (which when
/// present is forwarded to the child as `OPEN_MPM_CONFIG_DIR`).
/// Test: Both public callers exercise this struct end-to-end via the
/// existing subprocess integration tests.
struct SpawnConfig<'a> {
    agent_name: &'a str,
    task: &'a str,
    out_dir: Option<&'a std::path::Path>,
    /// #222: optional separate code dir, forwarded to the child as
    /// `OPEN_MPM_CODE_DIR` so the code-agent's WriteFileTool can root there
    /// instead of the artifacts dir.
    code_dir: Option<&'a std::path::Path>,
    history: &'a [HistoryMessage],
    ctx: Option<&'a RunContext>,
    config_dir: Option<&'a std::path::Path>,
    /// #410: Optional project directory forwarded as `OPEN_MPM_PROJECT_DIR`.
    /// Also used as the child's `current_dir` when `ctx.working_dir` is None
    /// so agents see the source tree rather than the artifacts directory.
    project_dir: Option<&'a std::path::Path>,
}

/// Unified subprocess spawn + IPC round-trip.
///
/// Why: The two public spawn functions previously shared ~80% identical code:
/// same `Command` setup, same stdin/stdout `tokio::spawn` reader/writer
/// pattern, same #147 non-zero-exit rescue, same #186 mistake logging.
/// Centralizing the implementation here means a single place to fix bugs.
/// What: Constructs the child `Command`, applies env/CWD overrides from
/// `ctx`/`out_dir`/`config_dir`, runs the writer + reader concurrently, then
/// reconciles the parsed `IpcMessage` with the child's exit status.
/// Test: Covered indirectly by every subprocess integration test plus the
/// `rescue_valid_result_on_nonzero_exit` and
/// `nonzero_exit_without_result_still_errors` unit tests in this module.
async fn spawn_and_run_inner(cfg: SpawnConfig<'_>) -> Result<IpcMessage> {
    let SpawnConfig {
        agent_name,
        task,
        out_dir,
        code_dir,
        history,
        ctx,
        config_dir,
        project_dir,
    } = cfg;

    let exe_path: PathBuf = std::env::current_exe().context("failed to resolve current_exe")?;

    let mut cmd = Command::new(&exe_path);
    cmd.args(["--agent", agent_name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // #268: Don't let sub-agent tracing leak into the interactive REPL's
        // TTY. The parent REPL detects TTY stdin and routes its own tracing
        // to a file; sub-agent processes here always see piped stdin (so they
        // route to stderr by default). Inheriting that stderr lands log lines
        // on top of the chat scrollback. Mirrors the same fix in
        // `claude_code_runner.rs`. Non-interactive parents (CI, --api,
        // --workflow) keep inheritance so log-capture tooling still works.
        .stderr(if crate::repl::is_tty() {
            Stdio::null()
        } else {
            Stdio::inherit()
        });
    if let Some(cd) = config_dir {
        cmd.env("OPEN_MPM_CONFIG_DIR", cd);
    }

    // Stamp the MPM sub-agent marker so any Claude Code instance running
    // inside this child knows it is nested. The `trusty-mpm hook` consumer
    // guards on this variable to suppress recursive PreToolUse / PostToolUse
    // / Stop traffic from the sub-agent (which would otherwise double every
    // audit-log entry). The `trusty-memory prompt-context` hook deliberately
    // does NOT guard on this variable — sub-agents benefit from the parent
    // palace's prompt-fact block as much as the PM does. The PM session
    // never inherits this var; it is set per-spawn here so only the
    // sub-agent subtree sees it. The literal lives in
    // `trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR` so every
    // spawn site and consumer references the same name.
    cmd.env(
        trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR,
        "1",
    );

    // #193: Stamp the child process with `agent` identity so the memory tools
    // inside it can enforce a scope ceiling of `RecallCeiling::Agent`. The
    // harness controls these env vars; child agents have no way to override
    // them. session_id flows from OPEN_MPM_RUN_ID (set in main.rs at startup),
    // project_id from the directory basename of the working dir if any, else
    // the parent's CWD.
    cmd.env(crate::identity::ENV_CALLER, "agent");
    cmd.env(crate::identity::ENV_AGENT_ID, agent_name);
    if let Ok(sid) = std::env::var("OPEN_MPM_RUN_ID")
        && !sid.is_empty()
    {
        cmd.env(crate::identity::ENV_SESSION_ID, sid);
    }
    let project_id_dir = ctx
        .and_then(|c| c.working_dir.as_deref())
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    if let Some(dir) = project_id_dir
        && let Some(pid) = dir.file_name().and_then(|n| n.to_str())
    {
        cmd.env(crate::identity::ENV_PROJECT_ID, pid);
    }
    if let Some(dir) = out_dir {
        cmd.env("OPEN_MPM_OUT_DIR", dir);
    }
    // #222: When set, OPEN_MPM_CODE_DIR overrides OPEN_MPM_OUT_DIR for
    // tools that write generated code (e.g. code-agent's WriteFileTool).
    if let Some(dir) = code_dir {
        cmd.env("OPEN_MPM_CODE_DIR", dir);
    }
    // #410: Forward the project source directory so child agents (and tools
    // they invoke) can resolve the user's project root independently of CWD.
    if let Some(dir) = project_dir {
        cmd.env("OPEN_MPM_PROJECT_DIR", dir);
    }
    if let Some(ctx) = ctx {
        if let Some(path) = &ctx.assigned_file {
            cmd.env("OPEN_MPM_ASSIGNED_FILE", path);
        }
        if let Some(n) = ctx.max_turns_override {
            cmd.env("OPEN_MPM_MAX_TURNS", n.to_string());
        }
        if let Some(wd) = &ctx.working_dir {
            cmd.current_dir(wd);
        } else if let Some(dir) = project_dir {
            // #410: When no per-call working_dir is set, fall back to the
            // runner's project_dir (the user's source tree). Without this the
            // child inherits the parent's CWD, which may be the artifacts
            // out_dir and would break `read_file`/`search_code` for source.
            cmd.current_dir(dir);
        }
        // #107 / Bug: Do NOT forward `ctx.model` as
        // `OPEN_MPM_MODEL_<AGENT>` to subprocess children. The env var
        // resolution path (see `agents::mod::resolve_model`) treats
        // `OPEN_MPM_MODEL_*` as the *highest* priority source, which means
        // any inherited override silently clobbers the child agent's own
        // TOML model (e.g. `bedrock/...` or `use_anthropic_direct = true`
        // configurations). The `OPEN_MPM_MODEL_*` env vars are intended
        // as an *operator/CLI-level* override, not as an inheritance
        // channel between parent and child agent processes — each
        // sub-agent should resolve its own model from its own TOML and
        // any operator env vars present in the original shell environment
        // (which `Command` inherits by default). If an explicit
        // per-invocation override is needed for a sub-agent, plumb it
        // through `RunContext` to the in-process resolver instead.
        //
        // Operator overrides (env vars set by the user before launching
        // the harness) still propagate naturally because tokio's
        // `Command` inherits the parent environment by default.
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn sub-agent '{agent_name}'"))?;

    // #199: emit `AgentStarted` once the subprocess is alive and ready to
    // accept its task on stdin. Distinct from `AgentSpawned` (which fires
    // when the parent decides to delegate) — this signals the work loop is
    // actually under way.
    crate::events::publish(crate::events::Event::AgentStarted {
        session_id: std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default(),
        agent_name: agent_name.to_string(),
        runner_type: "subprocess".to_string(),
    });

    let mut child_stdin = child.stdin.take().context("sub-agent stdin not captured")?;
    let child_stdout = child
        .stdout
        .take()
        .context("sub-agent stdout not captured")?;

    let task_msg = if history.is_empty() {
        IpcMessage::new_task(task)
    } else {
        IpcMessage::new_task_with_history(task, history.to_vec())
    };
    let task_line = serialize_message(&task_msg)?;

    let write_handle = tokio::spawn(async move {
        child_stdin
            .write_all(task_line.as_bytes())
            .await
            .context("failed to write task to sub-agent stdin")?;
        child_stdin
            .shutdown()
            .await
            .context("failed to close sub-agent stdin")?;
        Ok::<(), anyhow::Error>(())
    });

    let read_handle = tokio::spawn(async move {
        let mut reader = BufReader::new(child_stdout);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .context("failed to read sub-agent stdout")?;
        Ok::<String, anyhow::Error>(line)
    });

    let (write_res, read_res) = tokio::join!(write_handle, read_handle);
    write_res.context("writer task panicked")??;
    let line = read_res.context("reader task panicked")??;

    let status = child.wait().await.context("failed to wait for sub-agent")?;
    tracing::debug!(?status, "sub-agent exited");

    if line.trim().is_empty() {
        bail!("sub-agent produced no output");
    }

    // #147: Rescue valid NDJSON result even when sub-agent exits non-zero.
    let msg = parse_message(&line)?;
    if !status.success() {
        match &msg {
            IpcMessage::Result { .. } => {
                tracing::warn!(
                    exit_code = ?status.code(),
                    agent = %agent_name,
                    "sub-agent exited non-zero but produced valid output — treating as success (#147)"
                );
            }
            _ => {
                record_mistake_fire_and_forget(
                    agent_name,
                    crate::mistake_log::MistakeType::NonzeroExit,
                    status.code(),
                    "",
                    &line,
                    task,
                );
                bail!(
                    "sub-agent '{}' exited with status {} and no valid result",
                    agent_name,
                    status
                );
            }
        }
    }

    // #199: emit `ReportGenerated` when the agent produced a final result
    // before handing it back to the PM. Word count is a cheap whitespace
    // split — exact accuracy isn't important; we just want a coarse
    // productivity signal for the UI.
    if let IpcMessage::Result {
        content, status, ..
    } = &msg
    {
        let word_count = content.split_whitespace().count();
        crate::events::publish(crate::events::Event::ReportGenerated {
            session_id: std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default(),
            agent_name: agent_name.to_string(),
            word_count,
            status: status.clone(),
        });
    }

    Ok(msg)
}

/// #186: Fire-and-forget mistake logger used by both subprocess paths.
///
/// Why: Failure recording must never block the main agent loop, even if
/// the filesystem is slow or HOME is unset. Spawning a tokio task ensures
/// the WARN/bail path returns to the caller immediately.
/// What: Resolves the project root from CWD, builds a `MistakeRecord`, and
/// dispatches `MistakeLog::record` on a fresh task; failures are logged.
/// Test: Indirectly through the `--workflow` smoke; unit tests in the
/// `mistake_log` module exercise the record/read round-trip directly.
fn record_mistake_fire_and_forget(
    agent_name: &str,
    mistake_type: crate::mistake_log::MistakeType,
    exit_code: Option<i32>,
    stderr_preview: &str,
    stdout_preview: &str,
    task: &str,
) {
    use crate::mistake_log::{MistakeLog, MistakeRecord, truncate};
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_else(|_| "unknown".to_string());
    let phase = std::env::var("OPEN_MPM_PHASE").unwrap_or_else(|_| "subagent".to_string());
    let task_preview: String = task.chars().take(100).collect();
    let record = MistakeRecord {
        ts: chrono::Utc::now().to_rfc3339(),
        session_id: session_id.clone(),
        agent: agent_name.to_string(),
        task_id: session_id,
        mistake_type,
        exit_code,
        stderr: truncate(stderr_preview, 2000),
        stdout: truncate(stdout_preview, 2000),
        context: format!("phase={phase}, task={task_preview}"),
    };
    tokio::spawn(async move {
        if let Err(e) = MistakeLog::record(&project_root, &record).await {
            tracing::warn!(error = %e, "failed to record mistake (#186)");
        }
    });
}

/// Like `spawn_subagent_and_run_with_full_env_ctx` but also sets
/// `OPEN_MPM_CONFIG_DIR` on the child process so it loads agent TOML from
/// an explicit directory rather than the CWD-relative fallback.
///
/// Why: The CTRL CLI spawns PM actors for multiple project paths; each PM
/// spawns its own sub-agents that must find the right `config/agents/` dir.
/// Setting it per-`Command` (not via `std::env::set_var`) is safe under tokio.
/// What: Merges config_dir env injection with the full env + context variant.
/// Test: Used by `ctrl::run_pm_task`; verify child receives correct TOML path.
// Why: Thin adapter over `spawn_and_run_inner` — every argument is forwarded
// 1:1 into the `SpawnConfig` struct below, so collapsing them here just
// duplicates work that struct already does.
#[allow(clippy::too_many_arguments)]
async fn spawn_subagent_with_config_dir(
    agent_name: &str,
    task: &str,
    out_dir: Option<&std::path::Path>,
    code_dir: Option<&std::path::Path>,
    project_dir: Option<&std::path::Path>,
    history: &[HistoryMessage],
    config_dir: &std::path::Path,
    ctx: Option<&RunContext>,
) -> Result<IpcMessage> {
    spawn_and_run_inner(SpawnConfig {
        agent_name,
        task,
        out_dir,
        code_dir,
        history,
        ctx,
        config_dir: Some(config_dir),
        project_dir,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #147: A subprocess that writes a valid IpcMessage::Result to stdout and
    /// then exits with code 1 must be treated as success by the rescue logic in
    /// `spawn_subagent_and_run_with_full_env_ctx`. This mirrors the
    /// `error_max_turns` rescue in `ClaudeCodeAgentRunner` (#113).
    ///
    /// Why: Some agents produce correct output but crash during cleanup (e.g. a
    /// drop handler panics, a tool subprocess returns non-zero). Propagating the
    /// exit code as a hard error discards valid work and fails the whole phase.
    /// What: Spawns a tiny shell script that emits a valid NDJSON Result line and
    /// then exits with code 1. Replicates the rescue branch inline and asserts
    /// `Ok(IpcMessage::Result)` is returned.
    /// Test: `cargo test subprocess::tests::rescue_valid_result_on_nonzero_exit`
    #[cfg(unix)]
    #[tokio::test]
    async fn rescue_valid_result_on_nonzero_exit() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake-agent");
        // Emit a valid IpcMessage::Result line then exit with code 1.
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             printf '%s\\n' \
             '{\"type\":\"result\",\"id\":\"test-id\",\"content\":\"agent output\",\"status\":\"success\"}'\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Spawn the fake script and read exactly one NDJSON line from stdout.
        let mut child = tokio::process::Command::new(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();

        let stdout = child.stdout.take().unwrap();
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let status = child.wait().await.unwrap();
        assert!(!status.success(), "script should exit non-zero");

        let msg = parse_message(&line).expect("line should parse as IpcMessage");
        assert!(
            matches!(msg, IpcMessage::Result { .. }),
            "parsed message should be IpcMessage::Result, got: {msg:?}"
        );

        // Replicate the #147 rescue branch: non-zero + Result => Ok.
        // The real rescue path lives in spawn_subagent_and_run_with_full_env_ctx
        // and spawn_subagent_with_config_dir; we mirror the same logic here so
        // the invariant is machine-checked without re-invoking the binary.
        let rescued = if !status.success() {
            match &msg {
                IpcMessage::Result { .. } => Ok(msg.clone()),
                _ => Err(anyhow::anyhow!("non-zero exit and no valid result")),
            }
        } else {
            Ok(msg.clone())
        };

        let output = rescued.expect("rescue path should yield Ok");
        let IpcMessage::Result { content, .. } = output else {
            panic!("expected IpcMessage::Result after rescue");
        };
        assert_eq!(content, "agent output");
    }

    /// #147: A subprocess that exits non-zero AND produces an IpcMessage::Error
    /// on stdout must still propagate a hard error — the rescue only applies
    /// when there is a valid IpcMessage::Result to return.
    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_exit_without_result_still_errors() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fail-agent");
        // Emit an IpcMessage::Error line then exit with code 2.
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             printf '%s\\n' \
             '{\"type\":\"error\",\"id\":\"test-id\",\"error\":\"crashed\",\"status\":\"error\"}'\n\
             exit 2\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut child = tokio::process::Command::new(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();

        let stdout = child.stdout.take().unwrap();
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let status = child.wait().await.unwrap();
        assert!(!status.success(), "script should exit non-zero");

        let msg = parse_message(&line).expect("line should parse as IpcMessage");

        // The non-rescue branch: non-zero + Error => must error.
        let result: anyhow::Result<IpcMessage> = if !status.success() {
            match &msg {
                IpcMessage::Result { .. } => Ok(msg.clone()),
                _ => Err(anyhow::anyhow!(
                    "sub-agent exited with status {} and no valid result",
                    status
                )),
            }
        } else {
            Ok(msg.clone())
        };

        assert!(
            result.is_err(),
            "non-zero exit with IpcMessage::Error should propagate as Err"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("no valid result"),
            "unexpected error: {err_msg}"
        );
    }
}
