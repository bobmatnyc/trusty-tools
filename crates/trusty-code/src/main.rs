//! tcode — entry point for the trusty-code CLI.
//!
//! Why: provides the `tcode` binary that operators, agents, and TUI frontends
//! use to interact with the per-project MPM orchestration harness. As of
//! #2060 (M1 cut-line "replay via thin CLI"), `run-task`, `session list`/
//! `status`, `attach`, `cancel`, and `transcript` are all thin JSON-RPC
//! clients over a `tcode serve --stdio` child this binary spawns itself
//! (`trusty_code::cli_client` + `crate::cli`) — no orchestration logic lives
//! in the CLI. The ORIGINAL in-process `run-task` execution (epic #1039 /
//! #1034, calling `trusty_code::run_task::execute_run_task` directly) is
//! kept available behind `--legacy-in-process` (see [`Command::RunTask`]'s
//! docs for why it was kept rather than deleted) — `trusty_code::run_task`'s
//! own extensive offline test suite (`run_task::tests`) exercises that
//! library function directly and is unaffected by this CLI-layer change.
//!
//! What: thin clap CLI. `serve` (#2053) delegates to
//! `trusty_code::serve::{run_stdio, run_http}` for its two transports.
//! `run-workflow` remains a stub.
//!
//! Test: `cargo run -p trusty-code -- --version` must exit 0 and print the
//! crate version. The thin-client subcommands are covered end-to-end by
//! `tests/cli_e2e.rs` (spawns the real binary); the legacy in-process path
//! remains covered by `trusty_code::run_task::tests`.

use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

use trusty_code::llm::{LlmClient, LlmClientConfig, LlmClientTrait};
use trusty_code::run_task::{ExitCode, RunTaskParams, execute_run_task};

mod cli;

/// Environment variable that overrides the engineer model for a single run (#1035).
const ENGINEER_MODEL_ENV: &str = "TCODE_ENGINEER_MODEL";

/// tcode — per-project Claude-Code-compatible MPM orchestration harness.
#[derive(Parser)]
#[command(
    name = "tcode",
    version,
    about = "Per-project Claude-Code-compatible MPM orchestration harness",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Top-level subcommands for `tcode`.
///
/// Why: defines the stable CLI surface for all Phase 0+ callers. Stubs are
/// replaced by real implementations as each phase of #587/#1039 lands.
/// What: clap enum; each variant maps to one subcommand.
/// Test: `tcode --help` lists all variants; functional commands exit 0.
#[derive(Subcommand)]
enum Command {
    /// Start the per-project orchestration server.
    ///
    /// Accepts JSON-RPC 2.0 task requests from CLI clients, TUI frontends,
    /// and MCP callers. One instance per project. Exactly one transport must
    /// be selected: `--stdio` XOR `--http` (they are independent modes, not
    /// simultaneous — see `trusty_code::serve` module docs).
    Serve {
        /// Path to the project root (must contain a `.claude/` directory).
        #[arg(long, short, value_name = "PATH")]
        project: PathBuf,

        /// Serve JSON-RPC 2.0 over stdio (NDJSON on stdin/stdout), matching
        /// the trusty-memory/trusty-search MCP stdio convention.
        #[arg(long, conflicts_with = "http")]
        stdio: bool,

        /// Serve JSON-RPC 2.0 over HTTP: `POST /rpc` + `GET /health`,
        /// matching the trusty-memory/trusty-search axum daemon convention.
        #[arg(long, conflicts_with = "stdio")]
        http: bool,

        /// TCP port for `--http`. `0` binds an OS-assigned ephemeral port.
        /// Defaults to `trusty_code::serve::DEFAULT_HTTP_PORT`.
        ///
        /// Requires `--http` AND conflicts with `--stdio` — both are needed:
        /// `requires = "http"` alone does not fire when `--stdio` is also
        /// present, because clap evaluates `--stdio`'s `conflicts_with =
        /// "http"` first and short-circuits before the requires-graph check,
        /// so `--stdio --port N` would otherwise be silently accepted with
        /// the port discarded.
        #[arg(long, value_name = "PORT", requires = "http", conflicts_with = "stdio")]
        port: Option<u16>,
    },

    /// Create (or target) a session, run a task through it via the daemon's
    /// `task.run`, stream live events to the terminal as they arrive, and
    /// exit once the run reaches a terminal state (#2060, the M1 cut-line
    /// "replay via thin CLI" verb).
    ///
    /// This is a THIN CLIENT by default: it spawns an ephemeral
    /// `tcode serve --stdio` child (`crate::cli::run_task`) and drives it
    /// entirely over JSON-RPC — no agent-loop logic runs in this process.
    /// `--legacy-in-process` restores the pre-#2060 behaviour (this binary
    /// runs the PM->engineer `AgentLoop` directly, printing a diff/
    /// transcript/usage report at the end) — kept available rather than
    /// deleted since it is still exercised by `trusty_code::run_task::tests`
    /// and some callers may depend on its diff-report output, which the
    /// daemon-driven path does not (yet) compute.
    RunTask {
        /// Agent name as declared in `.claude/agents/<name>.toml` (e.g. `pm`).
        agent: String,

        /// Free-form task description passed to the agent's system prompt.
        task: String,

        /// Path to the project root (must contain a `.claude/` directory).
        /// Defaults to the current working directory.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,

        /// Emit a machine-readable JSON report on stdout instead of human text.
        #[arg(long)]
        json: bool,

        /// Override the python-engineer's model for this run only (#1035).
        /// Falls back to the `TCODE_ENGINEER_MODEL` env var, then the agent
        /// config's own model.
        #[arg(long, value_name = "SLUG")]
        engineer_model: Option<String>,

        /// Use the ORIGINAL in-process execution path instead of the #2060
        /// thin JSON-RPC client. See this variant's docs for why it is kept.
        #[arg(long)]
        legacy_in_process: bool,
    },

    /// Execute a named MPM workflow end-to-end.
    ///
    /// Loads the workflow definition from `.claude/workflows/<name>.toml` (or
    /// `.open-mpm/workflows/<name>.toml`) and runs it through the PM main-loop.
    RunWorkflow {
        /// Workflow name (matches the filename without extension).
        name: String,

        /// Path to the project root.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Inspect and manage daemon-owned sessions (#2060).
    Session {
        #[command(subcommand)]
        action: SessionCommand,
    },

    /// Attach to a live session and stream its events to the terminal until
    /// it reaches a terminal state or you press Ctrl-C (#2060).
    ///
    /// Spawns its OWN ephemeral daemon (see `crate::cli::attach`'s docs) —
    /// only useful against a session created within THIS same invocation
    /// today; M1 sessions are in-memory, per daemon process.
    Attach {
        /// The session id to attach to.
        session_id: String,

        /// Path to the project root the daemon should be rooted at.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Send a cancellation signal to a session (#2060).
    Cancel {
        /// The session id to cancel.
        session_id: String,

        /// Path to the project root the daemon should be rooted at.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Print a session's stored transcript (turns, tool calls, usage, cost)
    /// via `session.get_transcript` (#2058, #2060).
    Transcript {
        /// The session id to inspect.
        session_id: String,

        /// Path to the project root the daemon should be rooted at.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,

        /// Emit the raw JSON result instead of a human-readable rendering.
        #[arg(long)]
        json: bool,
    },
}

/// `tcode session <action>` subcommands (#2060).
#[derive(Subcommand)]
enum SessionCommand {
    /// List every session the daemon currently knows about.
    List {
        /// Path to the project root the daemon should be rooted at.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },
    /// Show one session's current status.
    Status {
        /// The session id to look up.
        session_id: String,

        /// Path to the project root the daemon should be rooted at.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise tracing to stderr (never stdout — stdout is the API transport,
    // and `--json` mode requires stdout to carry only the JSON report).
    trusty_code::logging::init_tracing();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve {
            project,
            stdio,
            http,
            port,
        } => run_serve(project, stdio, http, port).await,

        Command::RunTask {
            agent,
            task,
            project,
            json,
            engineer_model,
            legacy_in_process,
        } => {
            if legacy_in_process {
                run_task(&agent, &task, &project, json, engineer_model).await
            } else {
                let project = canonicalize_project_or_exit(&project, "run-task");
                match cli::run_task::run(&project, &agent, &task, json, engineer_model).await {
                    Ok(code) => process::exit(code),
                    Err(e) => {
                        eprintln!("tcode run-task: {e:#}");
                        process::exit(ExitCode::RunFailure.code());
                    }
                }
            }
        }

        Command::RunWorkflow { name, project } => {
            eprintln!(
                "tcode run-workflow: not yet implemented (#587 Phase 5+) [name={name}, project={}]",
                project.display()
            );
            process::exit(1);
        }

        Command::Session { action } => match action {
            SessionCommand::List { project } => {
                let project = canonicalize_project_or_exit(&project, "session list");
                run_thin_client(cli::session::list(&project), "session list").await
            }
            SessionCommand::Status {
                session_id,
                project,
            } => {
                let project = canonicalize_project_or_exit(&project, "session status");
                run_thin_client(
                    cli::session::status(&project, &session_id),
                    "session status",
                )
                .await
            }
        },

        Command::Attach {
            session_id,
            project,
        } => {
            let project = canonicalize_project_or_exit(&project, "attach");
            run_thin_client(cli::attach::run(&project, &session_id), "attach").await
        }

        Command::Cancel {
            session_id,
            project,
        } => {
            let project = canonicalize_project_or_exit(&project, "cancel");
            run_thin_client(cli::cancel::run(&project, &session_id), "cancel").await
        }

        Command::Transcript {
            session_id,
            project,
            json,
        } => {
            let project = canonicalize_project_or_exit(&project, "transcript");
            run_thin_client(
                cli::transcript::run(&project, &session_id, json),
                "transcript",
            )
            .await
        }
    }
}

/// Canonicalize `project`, printing a `tcode <cmd>:`-prefixed error and
/// exiting with [`ExitCode::ConfigError`] on failure.
///
/// Why: every thin-client subcommand needs the SAME "must be a real,
/// existing directory" validation `run-task`'s legacy path already applies
/// — centralised so each one-line match arm above doesn't repeat it.
/// What: `Path::canonicalize`, exiting on `Err` rather than returning one
/// (matching this binary's existing `process::exit`-on-config-error style).
fn canonicalize_project_or_exit(project: &Path, cmd: &str) -> PathBuf {
    project.canonicalize().unwrap_or_else(|e| {
        eprintln!(
            "tcode {cmd}: invalid --project path '{}': {e}",
            project.display()
        );
        process::exit(ExitCode::ConfigError.code());
    })
}

/// Run a thin-client subcommand's future, printing a `tcode <cmd>:`-prefixed
/// error and exiting nonzero on failure; exits 0 on success.
///
/// Why: every non-`run-task` thin-client subcommand (`session list`/
/// `status`, `attach`, `cancel`, `transcript`) shares the exact same
/// "await, print error verbatim (`RpcClientError`'s `Display` already
/// includes the daemon's JSON-RPC error message/code where applicable),
/// exit nonzero" shape — centralised here instead of repeated per arm.
/// What: `ExitCode::RunFailure`'s numeric value on error (mirrors
/// `run-task`'s exit-code contract); exits 0 (falls through, returning
/// `Ok(())`) on success.
async fn run_thin_client(
    fut: impl std::future::Future<Output = Result<()>>,
    cmd: &str,
) -> Result<()> {
    if let Err(e) = fut.await {
        eprintln!("tcode {cmd}: {e:#}");
        process::exit(ExitCode::RunFailure.code());
    }
    Ok(())
}

/// Execute `tcode serve`: dispatches to whichever transport was selected.
///
/// Why: keeps `main`'s match arm a one-liner, matching the shape of the
/// `run_task` wrapper below. The binary layer owns only the CLI-shaped
/// concern (which transport was requested, and the port); `trusty_code::serve`
/// owns router assembly and both transport loops, all fully unit-tested
/// offline. clap's `conflicts_with`/`requires` on the `Serve` variant already
/// reject `--stdio --http` together and `--port` without `--http` before this
/// function ever runs.
/// What: `--http` delegates to `trusty_code::serve::run_http` (port defaults
/// to `serve::DEFAULT_HTTP_PORT` when `--port` is omitted); `--stdio`
/// delegates to `trusty_code::serve::run_stdio`. Both run until shutdown
/// (SIGTERM/SIGINT, or stdin EOF for `--stdio`), logging to stderr only.
/// Neither flag given prints actionable usage and exits 1 rather than
/// silently doing nothing.
/// Test: exercised manually (`tcode serve --project . --stdio` /
/// `--http [--port N]`); `trusty_code::serve::tests`,
/// `serve::transport::tests`, and `serve::http::tests` cover the
/// router/transport logic this delegates to.
async fn run_serve(project: PathBuf, stdio: bool, http: bool, port: Option<u16>) -> Result<()> {
    if http {
        let port = port.unwrap_or(trusty_code::serve::DEFAULT_HTTP_PORT);
        if let Err(e) = trusty_code::serve::run_http(project, port).await {
            eprintln!("tcode serve --http: fatal error: {e:#}");
            process::exit(1);
        }
        return Ok(());
    }

    if stdio {
        if let Err(e) = trusty_code::serve::run_stdio(project).await {
            eprintln!("tcode serve --stdio: fatal error: {e:#}");
            process::exit(1);
        }
        return Ok(());
    }

    eprintln!(
        "tcode serve: pick a transport — `--stdio` (NDJSON on stdin/stdout) or \
         `--http [--port N]` (POST /rpc + GET /health) [project={}]",
        project.display()
    );
    process::exit(1);
}

/// Validate that `agent_name` contains only safe filesystem characters.
///
/// Why: The agent name is joined into a filesystem path
/// (`<agents_dir>/<agent_name>.toml`). Without this guard a crafted name such
/// as `../../etc/passwd` escapes the agents directory and enables path
/// traversal. Restricting to `[a-zA-Z0-9_-]` is safe, predictable, and covers
/// every real agent name in use.
/// What: Returns `Ok(())` when every character is ASCII alphanumeric, `_`, or
/// `-`, and the name is non-empty. Returns `Err` with a descriptive message
/// otherwise.
/// Test: `validate_agent_name_rejects_traversal` and
/// `validate_agent_name_accepts_valid` in this module.
fn validate_agent_name(agent_name: &str) -> Result<()> {
    if agent_name.is_empty()
        || !agent_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!(
            "invalid agent name '{agent_name}': \
             agent names must be non-empty and contain only [a-zA-Z0-9_-]"
        );
    }
    Ok(())
}

/// Execute `tcode run-task`: run the PM→engineer pipeline and print the report.
///
/// Why: This is the binary-layer wrapper over the library orchestrator. It owns
/// only the concerns that are genuinely CLI-shaped — argument validation, env
/// key/model resolution, output mode, and process exit codes — and delegates all
/// orchestration to `trusty_code::run_task::execute_run_task` (which is fully
/// unit-tested offline). When `--json` is set, only the JSON report is written to
/// stdout; logs always go to stderr.
/// What: Validates the agent name and project path (traversal guards), locates the
/// agents dir, resolves the engineer-model override (CLI flag > `TCODE_ENGINEER_MODEL`
/// env), builds the real `LlmClient` from `OPENROUTER_API_KEY`, runs
/// `execute_run_task`, prints the human or JSON report, and exits with the
/// report's `ExitCode`. A missing API key is a config error (exit 2).
/// Test: Orchestration (incl. the model swap) is covered by `run_task::tests`; the
/// wrapper is exercised manually via `tcode run-task pm "<task>" --project <path>`.
async fn run_task(
    agent_name: &str,
    task: &str,
    project: &Path,
    json: bool,
    engineer_model_flag: Option<String>,
) -> Result<()> {
    if let Err(e) = validate_agent_name(agent_name) {
        eprintln!("tcode run-task: {e}");
        process::exit(ExitCode::ConfigError.code());
    }

    // Resolve canonical project root (path-traversal-safe: canonicalize collapses
    // any `..` so the project root is a real, existing directory).
    let project_root = match project.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "tcode run-task: invalid --project path '{}': {e}",
                project.display()
            );
            process::exit(ExitCode::ConfigError.code());
        }
    };

    let agents_dir = trusty_code::agents::locate_agents_dir(&project_root);

    // Engineer-model override (#1035): CLI flag wins, then the env var; an empty
    // value is treated as "unset" so it falls through to the agent config model.
    let engineer_model = engineer_model_flag
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var(ENGINEER_MODEL_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
        });

    info!(
        agent = agent_name,
        project = %project_root.display(),
        agents_dir = %agents_dir.display(),
        json,
        engineer_model = engineer_model.as_deref().unwrap_or("(agent default)"),
        "tcode run-task: starting"
    );

    // Build the real OpenRouter client from env. A missing key is a config error.
    let llm: Arc<dyn LlmClientTrait> = match build_llm_client() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("tcode run-task: {e}");
            process::exit(ExitCode::ConfigError.code());
        }
    };

    let params = RunTaskParams {
        agent: agent_name.to_string(),
        task: task.to_string(),
        project: project_root,
        agents_dir,
        engineer_model,
    };

    let report = execute_run_task(params, llm).await;

    // Print the report. In `--json` mode stdout carries only the JSON document.
    if json {
        println!("{}", report.render_json());
    } else {
        println!("{}", report.render_human());
    }

    process::exit(report.exit.code());
}

/// Build the real OpenRouter `LlmClient` from `OPENROUTER_API_KEY`.
///
/// Why: The binary is the only place that reads the API key from the environment
/// (library code never touches `std::env` for secrets). Attribution headers help
/// OpenRouter dashboards label tcode traffic.
/// What: Reads the key via `LlmClientConfig::from_env`, attaches referer/title,
/// and constructs the client. Returns a descriptive error when the key is unset.
/// Test: Exercised manually (a live run requires a real key); the offline tests
/// inject a mock client instead.
fn build_llm_client() -> Result<Arc<dyn LlmClientTrait>> {
    let config = LlmClientConfig::from_env()
        .map_err(|e| {
            anyhow::anyhow!(
                "OPENROUTER_API_KEY is required for run-task ({e}). \
                 Export it before running, e.g. `export OPENROUTER_API_KEY=sk-or-...`."
            )
        })?
        .with_referer("https://github.com/bobmatnyc/trusty-tools")
        .with_title("trusty-code run-task");
    let client = LlmClient::from_config(config)
        .map_err(|e| anyhow::anyhow!("failed to build LLM client: {e}"))?;
    Ok(Arc::new(client))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::validate_agent_name;

    /// A path-traversal agent name is rejected.
    ///
    /// Why: Guards the `agents_dir.join(format!("{agent_name}.toml"))` path
    /// construction in the run-task pipeline against names that escape the agents
    /// directory.
    /// What: Asserts that `../../etc/passwd` and similar strings fail validation.
    /// Test: This test.
    #[test]
    fn validate_agent_name_rejects_traversal() {
        assert!(
            validate_agent_name("../../etc/passwd").is_err(),
            "path traversal must be rejected"
        );
        assert!(
            validate_agent_name("../sibling").is_err(),
            "parent-dir component must be rejected"
        );
        assert!(
            validate_agent_name("agent/subdir").is_err(),
            "path separator must be rejected"
        );
        assert!(
            validate_agent_name("agent name").is_err(),
            "space must be rejected"
        );
        assert!(
            validate_agent_name("").is_err(),
            "empty string must be rejected"
        );
        assert!(
            validate_agent_name("agent\0null").is_err(),
            "null byte must be rejected"
        );
    }

    /// A well-formed agent name is accepted.
    ///
    /// Why: Verifies the allowlist does not over-reject legitimate names.
    /// What: Asserts that common agent names (`pm`, `qa-agent`, etc.) pass.
    /// Test: This test.
    #[test]
    fn validate_agent_name_accepts_valid() {
        assert!(validate_agent_name("pm").is_ok());
        assert!(validate_agent_name("engineer").is_ok());
        assert!(validate_agent_name("qa-agent").is_ok());
        assert!(validate_agent_name("python_engineer").is_ok());
        assert!(validate_agent_name("rust-engineer-2024").is_ok());
        assert!(
            validate_agent_name("A").is_ok(),
            "single ASCII letter must pass"
        );
    }
}
