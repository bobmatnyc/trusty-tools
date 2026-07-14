//! `tm sessctl` SESSCTL control-plane command group (WI-2 #1593).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`SessctlAction`] — `run`/`connect`/`stop`/`auth`/`list`.
//! Test: `cli_parses_sessctl_*` in `tests.rs`.

use clap::Subcommand;

/// Verbs for the `tm sessctl` SESSCTL control-plane command group (WI-2 #1593).
///
/// Why: Phase 2 exposes the full session lifecycle over the daemon HTTP API;
/// a sub-subcommand group makes each verb discoverable and individually
/// parseable without growing the top-level command surface.
/// What: `Run` spawns a session, `Connect` streams SSE events, `Stop` sends
/// a stop command, `Auth` polls the auth state, `List` enumerates sessions.
/// Test: `cli_parses_sessctl_*` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum SessctlAction {
    /// Spawn a SESSCTL session for a project via the daemon HTTP API.
    ///
    /// Why: delegates session spawning to the daemon so a single shared
    /// registry owns all live sessions.
    /// What: POSTs to `POST /api/v1/control/sessions/run` and prints the
    /// allocated session ID.
    /// Test: `cli_parses_sessctl_run`.
    Run {
        /// Registered project ID.
        project_id: String,
        /// Use the tmux backend instead of stream-JSON.
        #[arg(long)]
        tmux: bool,
        /// Path to a system-prompt file (`--append-system-prompt-file`).
        #[arg(long)]
        prompt_file: Option<String>,
        /// Working directory override (daemon resolves from project-id by default).
        #[arg(long)]
        workdir: Option<String>,
    },
    /// Connect to a session (writer if write-lock available, observer otherwise).
    ///
    /// Why: streams SSE events from the daemon so the CLI can display live
    /// session output without embedding the registry.
    /// What: POSTs to `POST /api/v1/control/sessions/{id}/connect` and prints
    /// each event to stdout.
    /// Test: `cli_parses_sessctl_connect`.
    Connect {
        /// SESSCTL session ID (e.g. `my-proj-0`).
        session_id: String,
    },
    /// Stop a session (graceful by default, --force for immediate).
    ///
    /// Why: mirrors `tm sessions stop` for the SESSCTL surface.
    /// What: POSTs to `POST /api/v1/control/sessions/{id}/stop?force=<bool>`.
    /// Test: `cli_parses_sessctl_stop`.
    Stop {
        /// SESSCTL session ID.
        session_id: String,
        /// Send ForceStop instead of graceful Stop.
        #[arg(long)]
        force: bool,
    },
    /// Show the auth state for a session.
    ///
    /// Why: lets the operator poll for `awaiting-auth` without subscribing to
    /// the full SSE stream.
    /// What: GETs `GET /api/v1/control/sessions/{id}/auth` and prints the result.
    /// Test: `cli_parses_sessctl_auth`.
    Auth {
        /// SESSCTL session ID.
        session_id: String,
    },
    /// List all SESSCTL sessions.
    ///
    /// Why: provides a table or JSON view of every live control-plane session.
    /// What: GETs `GET /api/v1/control/sessions?project=<filter>` and renders
    /// the result as a table or JSON.
    /// Test: `cli_parses_sessctl_list`.
    #[command(name = "list")]
    List {
        /// Filter by project ID.
        #[arg(long)]
        project: Option<String>,
        /// Output format: `table` (default) or `json`.
        #[arg(long, default_value = "table", value_parser = ["table", "json"])]
        format: String,
    },
}
