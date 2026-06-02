//! tcode — entry point for the trusty-code CLI.
//!
//! Why: provides the `tcode` binary that operators, agents, and TUI frontends
//! use to interact with the per-project MPM orchestration harness. Phase 0
//! defines the CLI surface (subcommands + flags) so that callers can depend on
//! a stable interface while the underlying implementation is extracted from
//! open-mpm across Phases 1–N of epic #587.
//!
//! What: thin clap CLI with stub subcommands that fail fast with a clear
//! "not yet implemented" message and the tracking phase/issue reference.
//!
//! Test: `cargo run -p trusty-code -- --version` must exit 0 and print the
//! crate version. Each stub subcommand must exit non-zero.

use std::path::PathBuf;
use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

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
/// Test: `tcode --help` lists all variants; each stub exits non-zero.
#[derive(Subcommand)]
enum Command {
    /// Start the per-project orchestration server.
    ///
    /// Binds an IPC socket (or HTTP endpoint) and accepts task requests from
    /// CLI clients, TUI frontends, and MCP callers. One instance per project.
    Serve {
        /// Path to the project root (must contain a `.claude/` directory).
        #[arg(long, short, value_name = "PATH")]
        project: PathBuf,
    },

    /// Delegate a single task to a named agent and wait for the result.
    ///
    /// Connects to the running `tcode serve` instance for the project and
    /// dispatches <task> to <agent>. Prints the agent's response to stdout
    /// and exits 0 on success, non-zero on error.
    RunTask {
        /// Agent name as declared in `.claude/agents/<name>.toml`.
        agent: String,

        /// Free-form task description passed to the agent's system prompt.
        task: String,
    },

    /// Execute a named MPM workflow end-to-end.
    ///
    /// Loads the workflow definition from `.claude/workflows/<name>.toml` (or
    /// `.open-mpm/workflows/<name>.toml`) and runs it through the PM
    /// main-loop. All mandatory workflow stages are enforced.
    RunWorkflow {
        /// Workflow name (matches the filename without extension).
        name: String,
    },
}

fn main() -> Result<()> {
    // Initialise tracing to stderr (never stdout — stdout is the MCP transport).
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve { project } => {
            eprintln!(
                "tcode serve: not yet implemented (#587 Phase 1) [project={}]",
                project.display()
            );
            process::exit(1);
        }

        Command::RunTask { agent, task } => {
            eprintln!(
                "tcode run-task: not yet implemented (#587 Phase 2) [agent={agent}, task={task}]"
            );
            process::exit(1);
        }

        Command::RunWorkflow { name } => {
            eprintln!("tcode run-workflow: not yet implemented (#587 Phase 2) [name={name}]");
            process::exit(1);
        }
    }
}
