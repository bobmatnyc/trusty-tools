//! Subprocess spawn + NDJSON IPC round-trip for sub-agent execution.
//!
//! Why: All sub-agent invocation funnels through a single `Command` setup +
//! stdin/stdout reader/writer pattern (with the #147 non-zero-exit rescue and
//! #186 mistake logging). Housing it here keeps the `SubprocessAgentRunner`
//! trait impl in `mod.rs` focused on policy rather than process mechanics.
//! What: The public `spawn_subagent_and_run*` entry points, the internal
//! `SpawnConfig` + `spawn_and_run_inner`, the `config_dir` variant, and the
//! fire-and-forget mistake logger.
//! Test: Exercised end-to-end by the PM/workflow smoke tests; the rescue path
//! is unit-tested in `subprocess::tests`.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::ipc::{IpcMessage, parse_message, serialize_message};
use crate::session::HistoryMessage;
use crate::tools::traits::RunContext;

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
pub(super) struct SpawnConfig<'a> {
    pub(super) agent_name: &'a str,
    pub(super) task: &'a str,
    pub(super) out_dir: Option<&'a std::path::Path>,
    /// #222: optional separate code dir, forwarded to the child as
    /// `OPEN_MPM_CODE_DIR` so the code-agent's WriteFileTool can root there
    /// instead of the artifacts dir.
    pub(super) code_dir: Option<&'a std::path::Path>,
    pub(super) history: &'a [HistoryMessage],
    pub(super) ctx: Option<&'a RunContext>,
    pub(super) config_dir: Option<&'a std::path::Path>,
    /// #410: Optional project directory forwarded as `OPEN_MPM_PROJECT_DIR`.
    /// Also used as the child's `current_dir` when `ctx.working_dir` is None
    /// so agents see the source tree rather than the artifacts directory.
    pub(super) project_dir: Option<&'a std::path::Path>,
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
pub(super) async fn spawn_and_run_inner(cfg: SpawnConfig<'_>) -> Result<IpcMessage> {
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
pub(super) async fn spawn_subagent_with_config_dir(
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
