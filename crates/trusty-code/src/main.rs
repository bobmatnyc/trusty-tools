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
//! That legacy path's own wrapper lives in `crate::cli::legacy_run_task`
//! (#4434), alongside every other subcommand handler — this file holds no
//! subcommand body except `run_serve`.
//!
//! What: thin clap CLI. `serve` (#2053) delegates to
//! `trusty_code::serve::{run_stdio, run_http}` for its two transports.
//! `run-workflow` remains a stub. `workstream` (`ws` alias, #3296, DOC-48
//! §5.4) is the same thin-client shape over `workstream.*` — see
//! [`WorkstreamCommand`] and `crate::cli::workstream`. `tui` (#4424, DOC-50
//! §4.1/AC-2.4) is the odd one out: instead of spawning its own ephemeral
//! `--stdio` child, it attaches to an ALREADY-RUNNING `tcode serve --http`
//! daemon and hands `trusty_code::tui_client::CodeEngine` to
//! `trusty_tui::run::run` — see `crate::cli::tui`.
//!
//! Test: `cargo run -p trusty-code -- --version` must exit 0 and print the
//! crate version. The thin-client subcommands are covered end-to-end by
//! `tests/cli_e2e.rs` (spawns the real binary); the legacy in-process path
//! remains covered by `trusty_code::run_task::tests`.

use std::path::{Path, PathBuf};
use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand};

use trusty_code::run_task::ExitCode;

mod cli;

/// tcode — per-project Claude-Code-compatible MPM orchestration harness.
#[derive(Parser)]
#[command(
    name = "tcode",
    // Not clap's bare `version` (which prints the semver alone): a semver
    // cannot identify WHICH build produced a run's artifacts (#2823), so
    // `--version` carries the git SHA + commit date too.
    version = trusty_code::build_info::LONG_VERSION,
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
        ///
        /// OPTIONAL: omit it to serve PROJECTLESS — a first-class state for
        /// chat/planning before a project is chosen, and the state the shell's
        /// entry screen renders. A projectless daemon indexes nothing, has no
        /// diff target, and scopes no memory palace; agents resolve from the
        /// user-level `~/.claude/agents`. The path must be an existing
        /// directory when given; it need NOT be a git repo (a plain directory
        /// binds and indexes just like a repo, minus git affordances).
        #[arg(long, short, value_name = "PATH")]
        project: Option<PathBuf>,

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

    /// Launch the interactive TUI REPL, starting a `tcode serve --http`
    /// daemon if one isn't already running (#4424; auto-spawn #4512).
    ///
    /// The daemon is located by `TCODE_DAEMON_URL`, else the `http_addr`
    /// discovery file, and is liveness-pinged before use. When nothing
    /// answers, one is STARTED automatically (#4512, reversing DOC-50 §4.1's
    /// deferral) — and LEFT RUNNING when the REPL exits, since the daemon
    /// owns PM lifecycle and agent dispatch and this TUI is only one of its
    /// attached clients. A `TCODE_DAEMON_URL` that is set but unreachable is
    /// an error rather than a spawn, so an explicit address is never quietly
    /// replaced with a different one, and a daemon bound to a DIFFERENT
    /// project than `--project` is refused rather than attached to. See
    /// `crate::cli::tui` for the wiring and `crate::cli::daemon_autospawn`
    /// for the policy.
    Tui {
        /// Path to the project root the REPL's session binds to.
        ///
        /// OPTIONAL: omit it for a PROJECTLESS session — the same
        /// first-class state `serve` without `--project` and `session.create`
        /// without a `project` already support. Must name an existing
        /// directory when given.
        #[arg(long, short, value_name = "PATH")]
        project: Option<PathBuf>,
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
    /// transcript/usage report at the end — see `crate::cli::legacy_run_task`)
    /// — kept available rather than deleted since it is still exercised by
    /// `trusty_code::run_task::tests` and some callers may depend on its
    /// diff-report output, which the daemon-driven path does not (yet)
    /// compute.
    RunTask {
        /// Agent name as declared in `.claude/agents/<name>.md` (e.g. `pm`).
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

        /// Per-call `HarnessMode` override (#2059, vision spec §5.9):
        /// `daily-driver` or `parity`. Passed as `task.run`'s `mode` param
        /// (thin-client path only — ignored under `--legacy-in-process`,
        /// which has no mode concept). `TRUSTY_CODE_MODE` still overrides
        /// this per the resolution precedence in `crate::mode`'s docs.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,

        /// Override the run's wall-clock deadline, in seconds (#2207).
        /// Applies to BOTH the PM's own loop and the delegated engineer's
        /// loop, in either execution path (`--legacy-in-process` or the
        /// default thin-client/daemon path). Falls back to the
        /// `TCODE_RUN_DEADLINE_SECONDS` env var, then a generous 1800s (30
        /// minute) default — see `crate::provider::resolve_deadline_secs`'s
        /// docs for the full precedence. Use a large value (e.g. 7200-10800)
        /// for multi-hour tasks.
        #[arg(long, value_name = "SECONDS")]
        timeout_seconds: Option<u64>,
    },

    /// Execute a named MPM workflow end-to-end.
    ///
    /// Loads the workflow definition from `.claude/workflows/<name>.toml` and
    /// runs it through the PM main-loop.
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

    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405).
    Config(trusty_common::inference::config::ConfigCommand),

    /// Manage workstreams — durable named groupings of sessions bound to the
    /// daemon's project (#3296, DOC-48 §5.4). `tcode ws` is a short alias
    /// for this whole family (DOC-48 AC-5.3).
    #[command(alias = "ws")]
    Workstream {
        #[command(subcommand)]
        action: WorkstreamCommand,
    },
}

/// `tcode workstream <action>` / `tcode ws <action>` subcommands (#3296,
/// DOC-48 §5.4).
///
/// Every variant carries its own `--project` (defaulting to `.`), matching
/// [`SessionCommand`]/[`Command::Attach`]/[`Command::Cancel`]/
/// [`Command::Transcript`]'s existing per-leaf convention rather than
/// hoisting it onto the parent [`Command::Workstream`] variant (clap
/// subcommand args aren't inherited from an enclosing variant without a
/// shared flattened struct, which would be a bigger change than this
/// ticket's scope).
#[derive(Subcommand)]
enum WorkstreamCommand {
    /// List workstreams in the daemon's project (tm-ls-style table; DOC-48
    /// AC-5.1).
    List {
        /// Include closed workstreams (default: false, DOC-48 §4.4).
        #[arg(long)]
        include_closed: bool,

        /// Path to the project root the daemon should be rooted at.
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Show one workstream's full record as JSON.
    Get {
        /// The workstream id — a FULL UUID only (prefix-matching is a
        /// documented future nicety; see `crate::cli::workstream`'s module
        /// docs for why it isn't implemented client-side today).
        id: String,

        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Create a new workstream.
    Create {
        /// Human-readable name (defaults to empty/untitled if omitted).
        #[arg(long)]
        name: Option<String>,

        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Activate a workstream, making it the daemon's one active workstream
    /// (DOC-48 §6.1, AC-5.2).
    Activate {
        /// The workstream id to activate (full UUID only).
        id: String,

        /// Force-switch even if a DIFFERENT workstream is currently active
        /// (deactivates the prior one; DOC-48 §6.1). Without this flag,
        /// activating while another workstream is active fails with a clear
        /// message naming the active workstream.
        #[arg(long)]
        force: bool,

        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Deactivate the currently active workstream, if any (no-op otherwise).
    Deactivate {
        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
    },

    /// Irreversibly close a workstream (DOC-48 §4.4).
    Close {
        /// The workstream id to close (full UUID only).
        id: String,

        #[arg(long, short, value_name = "PATH", default_value = ".")]
        project: PathBuf,
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

        // #4424: the launch point for the TUI REPL — reuses `run_thin_client`
        // so a daemon-resolution failure prints `tcode tui: <actionable
        // message>` and exits nonzero, exactly like every other subcommand's
        // failure.
        // #4512: that failure is now rare — a missing daemon is started, not
        // reported.
        Command::Tui { project } => run_thin_client(cli::tui::run(project), "tui").await,

        Command::RunTask {
            agent,
            task,
            project,
            json,
            engineer_model,
            legacy_in_process,
            mode,
            timeout_seconds,
        } => {
            if legacy_in_process {
                // #4434: the in-process path (and the agent-name validation
                // and LLM-client construction only it needs) lives in
                // `cli::legacy_run_task` — `main.rs` had reached 498 of the
                // 500-SLOC cap and is now clap definitions plus dispatch.
                cli::legacy_run_task::run(
                    &agent,
                    &task,
                    &project,
                    json,
                    engineer_model,
                    timeout_seconds,
                )
                .await
            } else {
                let project = canonicalize_project_or_exit(&project, "run-task");
                match cli::run_task::run(
                    &project,
                    &agent,
                    &task,
                    json,
                    engineer_model,
                    mode,
                    timeout_seconds,
                )
                .await
                {
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

        Command::Config(cmd) => cmd.run().await,

        Command::Workstream { action } => match action {
            WorkstreamCommand::List {
                include_closed,
                project,
            } => {
                let project = canonicalize_project_or_exit(&project, "workstream list");
                run_thin_client(
                    cli::workstream::list(&project, include_closed),
                    "workstream list",
                )
                .await
            }
            WorkstreamCommand::Get { id, project } => {
                let project = canonicalize_project_or_exit(&project, "workstream get");
                run_thin_client(cli::workstream::get(&project, &id), "workstream get").await
            }
            WorkstreamCommand::Create { name, project } => {
                let project = canonicalize_project_or_exit(&project, "workstream create");
                run_thin_client(cli::workstream::create(&project, name), "workstream create").await
            }
            WorkstreamCommand::Activate { id, force, project } => {
                let project = canonicalize_project_or_exit(&project, "workstream activate");
                run_thin_client(
                    cli::workstream::activate(&project, &id, force),
                    "workstream activate",
                )
                .await
            }
            WorkstreamCommand::Deactivate { project } => {
                let project = canonicalize_project_or_exit(&project, "workstream deactivate");
                run_thin_client(
                    cli::workstream::deactivate(&project),
                    "workstream deactivate",
                )
                .await
            }
            WorkstreamCommand::Close { id, project } => {
                let project = canonicalize_project_or_exit(&project, "workstream close");
                run_thin_client(cli::workstream::close(&project, &id), "workstream close").await
            }
        },
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
/// `cli::*` subcommand wrappers. The binary layer owns only the CLI-shaped
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
/// `project` is optional: `None` serves PROJECTLESS. It is resolved into a
/// typed `ProjectBinding` HERE, at the boundary, so an unusable `--project`
/// (missing, or not a directory) fails fast with an actionable message rather
/// than surfacing later as a confusing per-task error.
/// Test: exercised manually (`tcode serve --project . --stdio` /
/// `tcode serve --stdio` projectless / `--http [--port N]`);
/// `trusty_code::serve::tests`, `serve::transport::tests`, and
/// `serve::http::tests` cover the router/transport logic this delegates to.
async fn run_serve(
    project: Option<PathBuf>,
    stdio: bool,
    http: bool,
    port: Option<u16>,
) -> Result<()> {
    let binding = match trusty_code::binding::ProjectBinding::resolve(project) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("tcode serve: {e}");
            eprintln!(
                "hint: pass an existing directory to --project, or omit --project \
                 entirely to serve projectless (chat/planning; no index, no diff)."
            );
            process::exit(1);
        }
    };
    if http {
        let port = port.unwrap_or(trusty_code::serve::DEFAULT_HTTP_PORT);
        if let Err(e) = trusty_code::serve::run_http(binding, port).await {
            eprintln!("tcode serve --http: fatal error: {e:#}");
            process::exit(1);
        }
        return Ok(());
    }

    if stdio {
        if let Err(e) = trusty_code::serve::run_stdio(binding).await {
            eprintln!("tcode serve --stdio: fatal error: {e:#}");
            process::exit(1);
        }
        return Ok(());
    }

    eprintln!(
        "tcode serve: pick a transport — `--stdio` (NDJSON on stdin/stdout) or \
         `--http [--port N]` (POST /rpc + GET /health) [project={}]",
        binding.label().unwrap_or_else(|| "<projectless>".into())
    );
    process::exit(1);
}
