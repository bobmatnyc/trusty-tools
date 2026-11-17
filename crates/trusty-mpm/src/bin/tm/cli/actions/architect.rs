//! `tm architect` — the Architect's deterministic commands (#9822, #9171 A4).
//!
//! Why: `start-fleet-poll.sh` and the Architect's skills ran five Python
//! scripts; each verb here takes the same arguments as the script it
//! replaces, so a caller swaps one for the other.
//! What: [`ArchitectAction`] — `poll`, `classify`, `input-state`, `quiet`,
//! `self-ctx`. The work is `trusty_architect::verbs`. #10063 adds
//! `dashboard`, the dashboard store's verbs ([`DashboardArgs`]).
//! Test: `cli_parses_architect_verbs`, `cli_parses_architect_dashboard_verbs`.

use std::path::PathBuf;

use clap::Subcommand;

use super::DashboardArgs;

/// Actions for the `architect` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum ArchitectAction {
    /// The deterministic fleet poller (`fleet-poll.py`).
    ///
    /// Lists the tmux panes, classifies each `tm-*` PM pane, appends new
    /// alerts to `inbox/alerts.jsonl` and wakes the Architect's pane with a
    /// one-line pointer. The Architect directory is `ARCHITECT_PROJECT_DIR`,
    /// else the current directory. `--once` and `--dry-run` exit 1 when the
    /// cycle fails or is skipped.
    Poll {
        /// Run one cycle and exit.
        #[arg(long)]
        once: bool,
        /// Seconds between cycles (a positive whole number).
        #[arg(long, default_value = "60", allow_hyphen_values = true)]
        interval: String,
        /// One cycle that captures and classifies only: prints to stdout,
        /// writes no file, sends no keys.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print one pane capture's facts as JSON (`parse_pane`).
    ///
    /// Reads the capture (`tmux capture-pane -e -p`) from FILE, or from stdin
    /// when FILE is absent or `-`. Exits 1 on an empty or non-UTF-8 capture.
    Classify {
        /// The capture file; `-` or absent reads stdin.
        file: Option<PathBuf>,
    },
    /// Classify each pane's input box (`input-state.py`).
    ///
    /// Prints `<target> <empty|suggestion|typed> | <text>`, or
    /// `<target> no-prompt |`, one line per target. A target it cannot
    /// capture prints `<target> missing |` and the command exits 1. Each
    /// target is exact: `%N` or `=<session>:`. `TMUX_SOCKET` selects the server.
    InputState {
        /// Exact tmux targets.
        #[arg(required = true)]
        targets: Vec<String>,
    },
    /// Print the quiet-session list, sorted, one name per line.
    ///
    /// The poller drops `idle` alerts for these sessions. Exits 1 when the
    /// file cannot be read, a missing file included. The Architect directory
    /// is `ARCHITECT_PROJECT_DIR`, else the one `tm fleet init` recorded,
    /// else `~/trusty-mpm-projects/architect`; never the current directory.
    Quiet {
        /// The list (default: `ARCHITECT_QUIET_SESSIONS_FILE`, else
        /// `<Architect dir>/local/quiet-sessions.txt`).
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Print the Architect's own context in tokens (`self-ctx.py`).
    ///
    /// Reads the newest interactive transcript in DIR. Exits 1 when there is
    /// nothing to measure.
    SelfCtx {
        /// The transcript directory (default: `SELF_CTX_DIR`, else the
        /// Architect project's Claude Code transcript folder).
        dir: Option<PathBuf>,
    },
    /// Read and write the Architect's dashboard store (`dashboard-db.py`).
    ///
    /// Exit codes: 0 ok, 1 a stale `--if-version` (or `fsck`/`validate`
    /// found a problem), 2 a refused request, 3 an unreadable or locked store.
    // #10063: owner ruling D34 fixes the conflict exit at 1.
    Dashboard(DashboardArgs),
    /// Bring the Architect up from any state, then attach (#11364).
    ///
    /// Idempotent and locked: starts what is missing, never stops or
    /// restarts a live `claude`. Inside Claude Code it only ensures a bound
    /// Architect. Exit codes: 0 up, 1 a step failed, 2 refused.
    Up(UpArgs),
    /// Attach to the Architect's tmux session (switch-client inside tmux).
    Attach,
    /// Restart the Architect behind the checkpoint gate (#11365).
    ///
    /// Refuses unless the Intent Ledger checkpoint is fresh (exit 4) and the
    /// box is idle for 3 reads 5 s apart (exit 2). Then stops `claude`,
    /// relaunches it bound with resume, restores the poller and types one
    /// resume line. Refused inside Claude Code. No bypass flag.
    Restart {
        /// Keep waiting for idle up to this many seconds.
        #[arg(long)]
        wait: Option<u64>,
        /// The Architect directory (default: the one `tm fleet init` recorded).
        #[arg(long)]
        dir: Option<String>,
    },
}

/// `tm architect up` flags (#11364).
#[derive(Debug, Default, Clone, clap::Args)]
pub(crate) struct UpArgs {
    /// Bring it up but do not attach.
    #[arg(long)]
    pub(crate) no_attach: bool,
    /// Print the step plan and change nothing.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Print the report as JSON (implies --no-attach).
    #[arg(long)]
    pub(crate) json: bool,
    /// The Architect directory (default: the one `tm fleet init` recorded).
    #[arg(long)]
    pub(crate) dir: Option<String>,
}
