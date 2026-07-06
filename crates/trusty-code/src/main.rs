//! tcode — entry point for the trusty-code CLI.
//!
//! Why: provides the `tcode` binary that operators, agents, and TUI frontends
//! use to interact with the per-project MPM orchestration harness. Phase 0
//! defines the CLI surface (subcommands + flags); the `run-task` path is now a
//! full PM→engineer execution (epic #1039 / #1034).
//!
//! What: thin clap CLI. `run-task` resolves the agents dir, validates the agent
//! name + project path, builds the real OpenRouter `LlmClient` from env, and
//! delegates to `trusty_code::run_task::execute_run_task`, then prints a human or
//! `--json` report and exits with the report's meaningful exit code. `serve` and
//! `run-workflow` remain stubs.
//!
//! Test: `cargo run -p trusty-code -- --version` must exit 0 and print the
//! crate version. The execution path is covered by `trusty_code::run_task::tests`
//! (offline, mocked LLM); the binary handler is a thin wrapper over that.

use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

use trusty_code::llm::{LlmClient, LlmClientConfig, LlmClientTrait};
use trusty_code::run_task::{ExitCode, RunTaskParams, execute_run_task};

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
    /// and MCP callers. One instance per project.
    Serve {
        /// Path to the project root (must contain a `.claude/` directory).
        #[arg(long, short, value_name = "PATH")]
        project: PathBuf,

        /// Serve JSON-RPC 2.0 over stdio (NDJSON on stdin/stdout), matching
        /// the trusty-memory/trusty-search MCP stdio convention.
        ///
        /// This is currently the only supported transport (#2053); an HTTP
        /// `POST /rpc` transport is tracked as a follow-up.
        #[arg(long)]
        stdio: bool,
    },

    /// Delegate a single task to a named agent and run it end-to-end.
    ///
    /// Loads the agent config from `<project>/.claude/agents/<agent>.toml`,
    /// assembles its system prompt (with project `CLAUDE.md` context), runs the
    /// PM through the agent loop, lets it delegate to the python-engineer
    /// in-process, and prints the resulting diff, transcript, and usage.
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
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise tracing to stderr (never stdout — stdout is the API transport,
    // and `--json` mode requires stdout to carry only the JSON report).
    trusty_code::logging::init_tracing();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve { project, stdio } => run_serve(project, stdio).await,

        Command::RunTask {
            agent,
            task,
            project,
            json,
            engineer_model,
        } => run_task(&agent, &task, &project, json, engineer_model).await,

        Command::RunWorkflow { name, project } => {
            eprintln!(
                "tcode run-workflow: not yet implemented (#587 Phase 5+) [name={name}, project={}]",
                project.display()
            );
            process::exit(1);
        }
    }
}

/// Execute `tcode serve`: currently only the `--stdio` transport is wired.
///
/// Why: keeps `main`'s match arm a one-liner, matching the shape of the
/// `run_task` wrapper below. The binary layer owns only the CLI-shaped
/// concern (which transport was requested); `trusty_code::serve` owns the
/// router assembly and the transport loop, both fully unit-tested offline.
/// What: `--stdio` delegates to `trusty_code::serve::run_stdio`, which runs
/// until stdin EOF or SIGTERM, logging to stderr only. Without `--stdio`,
/// prints an actionable error — HTTP `POST /rpc` is listed in the parent
/// issue (#2053) but deferred to a follow-up ticket — and exits 1.
/// Test: exercised manually (`tcode serve --project . --stdio`);
/// `trusty_code::serve::tests` and `serve::transport::tests` cover the
/// router/transport logic this delegates to.
async fn run_serve(project: PathBuf, stdio: bool) -> Result<()> {
    if !stdio {
        eprintln!(
            "tcode serve: only --stdio is implemented today (#2053); HTTP POST /rpc is a follow-up [project={}]",
            project.display()
        );
        process::exit(1);
    }

    if let Err(e) = trusty_code::serve::run_stdio(project).await {
        eprintln!("tcode serve --stdio: fatal error: {e:#}");
        process::exit(1);
    }
    Ok(())
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

    let agents_dir = locate_agents_dir(&project_root);

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

/// Locate the agents directory for the given project root.
///
/// Why: Projects may use either `.claude/agents` (Claude Code native) or
/// `.open-mpm/agents` (open-mpm legacy). Checking both preserves compatibility.
/// What: Returns the first directory that exists; falls back to `.claude/agents`.
/// Test: Indirect via `run_task` integration.
fn locate_agents_dir(project_root: &std::path::Path) -> PathBuf {
    let claude_agents = project_root.join(".claude").join("agents");
    if claude_agents.exists() {
        return claude_agents;
    }
    let open_mpm_agents = project_root.join(".open-mpm").join("agents");
    if open_mpm_agents.exists() {
        return open_mpm_agents;
    }
    // Default to .claude/agents (may not exist yet).
    claude_agents
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
