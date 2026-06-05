//! tcode — entry point for the trusty-code CLI.
//!
//! Why: provides the `tcode` binary that operators, agents, and TUI frontends
//! use to interact with the per-project MPM orchestration harness. Phase 0
//! defines the CLI surface (subcommands + flags) so that callers can depend on
//! a stable interface while the underlying implementation is extracted from
//! open-mpm across Phases 1–N of epic #587.
//!
//! What: thin clap CLI with subcommands. `run-task` is functional as of Phase 6;
//! `serve` and `run-workflow` remain stubs.
//!
//! Test: `cargo run -p trusty-code -- --version` must exit 0 and print the
//! crate version. `tcode run-task <agent> <task> --project <path>` must
//! locate the agent config, validate it, and report readiness.

use std::path::{Path, PathBuf};
use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

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
/// replaced by real implementations as each phase of #587 lands.
/// What: clap enum; each variant maps to one subcommand.
/// Test: `tcode --help` lists all variants; functional commands exit 0.
#[derive(Subcommand)]
enum Command {
    /// Start the per-project orchestration server.
    ///
    /// Binds an IPC socket and accepts task requests from CLI clients, TUI
    /// frontends, and MCP callers. One instance per project.
    Serve {
        /// Path to the project root (must contain a `.claude/` directory).
        #[arg(long, short, value_name = "PATH")]
        project: PathBuf,
    },

    /// Delegate a single task to a named agent and print the result.
    ///
    /// Loads the agent config from `<project>/.claude/agents/<agent>.toml`,
    /// validates it, and reports the agent's capabilities. In a future phase
    /// this will spawn the agent subprocess and return its result.
    RunTask {
        /// Agent name as declared in `.claude/agents/<name>.toml`.
        agent: String,

        /// Free-form task description passed to the agent's system prompt.
        task: String,

        /// Path to the project root (must contain a `.claude/` directory).
        /// Defaults to the current working directory.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
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

fn main() -> Result<()> {
    // Initialise tracing to stderr (never stdout — stdout is the API transport).
    trusty_code::logging::init_tracing();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve { project } => {
            eprintln!(
                "tcode serve: not yet implemented (#587 Phase 5+) [project={}]",
                project.display()
            );
            process::exit(1);
        }

        Command::RunTask {
            agent,
            task,
            project,
        } => run_task(&agent, &task, &project),

        Command::RunWorkflow { name, project } => {
            eprintln!(
                "tcode run-workflow: not yet implemented (#587 Phase 5+) [name={name}, project={}]",
                project.display()
            );
            process::exit(1);
        }
    }
}

/// Validate that `agent_name` contains only safe filesystem characters.
///
/// Why: The LLM supplies `agent_name` which is joined into a filesystem path
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

/// Execute `tcode run-task`: validate agent config and report readiness.
///
/// Why: Phase 6 establishes the public API entry point for single-task
/// dispatch so callers and integration tests have a stable interface to target
/// before the subprocess runner is wired in (Phase 3b/5).
/// What: Locates the project `.claude/agents/<agent>.toml`, loads and validates
/// the config, then prints a JSON-structured status line to stdout. Exits 0 on
/// success, non-zero on error.
/// Test: `cargo run -p trusty-code -- run-task engineer "write a test"
///        --project /path/to/project` exits 0 when engineer.toml exists.
fn run_task(agent_name: &str, task: &str, project: &Path) -> Result<()> {
    // Guard against path traversal via LLM-supplied agent_name.
    if let Err(e) = validate_agent_name(agent_name) {
        eprintln!("tcode run-task: {e}");
        process::exit(1);
    }

    // Resolve canonical project root.
    let project_root = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());

    // Locate the agents directory — prefer `.claude/agents`, fall back to
    // `.open-mpm/agents` for open-mpm-compatible projects.
    let agents_dir = locate_agents_dir(&project_root);

    info!(
        agent = agent_name,
        project = %project_root.display(),
        agents_dir = %agents_dir.display(),
        "tcode run-task: locating agent config"
    );

    // Discover available agents for helpful error messages.
    let available = trusty_code::agents::discover_agents(&agents_dir);
    let available_names: Vec<&str> = available.iter().map(|(n, _)| n.as_str()).collect();

    // Locate the agent's TOML.
    let agent_toml = agents_dir.join(format!("{agent_name}.toml"));
    if !agent_toml.exists() {
        let available_str = if available_names.is_empty() {
            "(none found)".to_string()
        } else {
            available_names.join(", ")
        };
        eprintln!(
            "tcode run-task: unknown agent '{agent_name}'. \
             Available agents: {available_str}. \
             Check that {agent_toml} exists.",
            agent_toml = agent_toml.display()
        );
        process::exit(1);
    }

    // Load and validate the config.
    let cfg = trusty_code::agents::AgentConfig::load(&agent_toml).unwrap_or_else(|e| {
        eprintln!("tcode run-task: failed to load agent config: {e}");
        process::exit(1);
    });

    let model = cfg
        .agent
        .model
        .as_deref()
        .or(cfg.llm.model_override.as_deref())
        .unwrap_or("(default)");

    // Char-safe truncation: slicing bytes at offset 80 panics when a multi-byte
    // UTF-8 character straddles the boundary. Collecting the first 80 chars is
    // always valid regardless of the Unicode content of the task string.
    let task_preview: String = task.chars().take(80).collect();

    info!(
        agent = agent_name,
        model,
        task_preview = task_preview.as_str(),
        "tcode run-task: agent config validated"
    );

    // Emit a structured status line so machine callers can parse it.
    // In a future phase this transitions to actually spawning the agent.
    println!(
        "{}",
        serde_json::json!({
            "status": "ready",
            "agent": agent_name,
            "model": model,
            "task_len": task.len(),
            "config": agent_toml.display().to_string(),
            "note": "subprocess dispatch not yet wired (#587 Phase 3b/5)"
        })
    );

    Ok(())
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
    /// Why: Guards the `agents_dir.join(format!("{agent_name}.toml"))` call in
    /// `run_task` against LLM-supplied names that escape the agents directory.
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
    /// What: Asserts that common agent names (`engineer`, `qa-agent`, etc.) pass.
    /// Test: This test.
    #[test]
    fn validate_agent_name_accepts_valid() {
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
