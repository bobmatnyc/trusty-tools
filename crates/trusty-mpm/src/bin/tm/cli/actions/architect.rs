//! `tm architect` — the Architect's deterministic commands (#9822, #9171 A4).
//!
//! Why: `start-fleet-poll.sh` and the Architect's skills ran five Python
//! scripts; each verb here takes the same arguments as the script it
//! replaces, so a caller swaps one for the other.
//! What: [`ArchitectAction`] — `poll`, `serve` (#9779), `classify`, `input-state`, `quiet`,
//! `self-ctx`. The work is `trusty_architect::verbs`. #10063 adds
//! `dashboard`, the dashboard store's verbs ([`DashboardArgs`]). #11773 adds
//! `drift`, the read-only managed-file report, and `up --strict`.
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
        /// Seconds between cycles (a positive whole number). Default:
        /// `poll_interval_s` in `local/architect.json`, else 60.
        // #11369: no clap default, so every start path reads one cadence.
        #[arg(long, allow_hyphen_values = true)]
        interval: Option<String>,
        /// One cycle that captures and classifies only: prints to stdout,
        /// writes no file, sends no keys.
        #[arg(long)]
        dry_run: bool,
        /// Shadow run beside the live poller: writes only `inbox/shadow/`,
        /// takes no lock, sends no keys, relaunches nothing (#9779).
        #[arg(long)]
        shadow: bool,
    },
    /// The poll cycle as a service, run on local events (#9779).
    ///
    /// Runs the same cycle as `poll` soon after an inbox write, tmux pane
    /// activity (polled; no tmux client is attached) or a transcript
    /// append: 2 s debounce, at most one event cycle per 15 s
    /// (`SERVE_MIN_EVENT_GAP`), and at least every 60 s. GitHub reads keep their own cadence; no port is
    /// opened. Takes `inbox/poll.lock` for its life, so `poll` cannot run
    /// beside it. A watcher that fails is logged and raised as a `serve`
    /// alert while the 60 s tick keeps cycling. SIGTERM or SIGINT exits
    /// after the running cycle.
    Serve {
        /// Shadow run beside the live poller: writes only `inbox/shadow/`,
        /// takes no lock, sends no keys, relaunches, notifies and pushes
        /// nothing.
        #[arg(long)]
        shadow: bool,
        /// Run one cycle and exit (exit 1 when it fails).
        #[arg(long)]
        once: bool,
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
    /// Architect. Before any launch it deploys this binary's Architect bundle
    /// (#11368), keeps each replaced file for `tm architect restore`, and
    /// restarts a running poller or dashboard once when one of its scripts
    /// changed (never inside Claude Code). A failed deploy launches nothing.
    /// Exit codes: 0 up, 1 a step failed, 2 refused.
    Up(UpArgs),
    /// Attach to the Architect's tmux session (switch-client inside tmux).
    Attach,
    /// Report how each managed file differs from this binary's bundle (#11773).
    ///
    /// Read-only: writes, locks and fetches nothing. Per managed path it
    /// compares the bytes on disk, the sha the installed
    /// `docs/architect/bundle-provenance.json` records, and the bytes this
    /// binary's seed ships, and prints one state: `current` (equal to the
    /// seed), `stale` (an earlier shipped copy; the next `tm fleet init`
    /// refreshes it), `edited` (no shipped copy matches), `absent`, `extra`
    /// (an unseeded file under `scripts/` or `.claude/skills/`, or an
    /// orphaned `*.shipped`), `owner` (`AGENTS.md`) or `instance` (a changed
    /// `local/` file). Bytes from a content release newer than this binary's
    /// shipped history read `edited`. Exit codes: 0 nothing edited, 1 a file
    /// is edited, 2 the directory or the seed cannot be read.
    Drift {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// The Architect directory (default: the one `tm fleet init` recorded).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Put back the files the last launch deploy replaced (#11368).
    ///
    /// Every `tm architect up` and `restart` deploys this binary's bundle and
    /// keeps the bytes of each file it replaced under
    /// `.trusty-mpm/architect-bundle/<tm version>/` (one release; older ones
    /// are pruned). This restores them: a file holding the bytes that deploy
    /// wrote, or absent, gets the kept copy back; an edited file is left as
    /// is and reported. Writes files only; restarts nothing. The next `up`
    /// with the same tm deploys its bundle again. Exit codes: 0 restored,
    /// 1 a file was edited or failed, or there is no kept copy, 2 the
    /// directory cannot be resolved.
    Restore {
        /// The release to restore from (default: the one kept release).
        #[arg(long)]
        version: Option<String>,
        /// The Architect directory (default: the one `tm fleet init` recorded).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Restart the Architect behind the checkpoint gate (#11365).
    ///
    /// Waits for an idle box (3 reads 5 s apart, exit 2), asks the Architect
    /// for an Intent Ledger checkpoint and waits up to 90 s for the write
    /// (exit 4), then stops and relaunches only the Architect's `claude`,
    /// bound with resume, restores the poller and types one resume line.
    /// Other sessions, the daemon and launchd are untouched. Refused inside
    /// Claude Code and when no Architect is recorded (exit 2). No bypass
    /// flag (ruling Dpg): the owner overrides with `tm fleet relaunch`.
    // See G0b architect restart
    Restart {
        /// Keep waiting for idle up to this many seconds.
        #[arg(long)]
        wait: Option<u64>,
        /// The Architect directory (default: the one `tm fleet init` recorded).
        #[arg(long)]
        dir: Option<String>,
        /// Do not ask for a checkpoint; the existing one must still be fresh.
        #[arg(long)]
        no_checkpoint: bool,
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
    /// Refuse (exit 1, nothing written) when a managed `scripts/*` file is
    /// edited; see `tm architect drift` (#11773).
    #[arg(long)]
    pub(crate) strict: bool,
}
