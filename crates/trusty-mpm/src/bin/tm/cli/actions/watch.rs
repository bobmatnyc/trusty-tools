//! `tm watch` board-watch command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`WatchCmd`] (`poll`/`listen`) and the shared [`WatchArgs`]
//! flag set flattened into both.
//! Test: `cli_parses_watch_*` in `tests.rs`.

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum WatchCmd {
    /// One-shot: list label-matched issues, dispatch each, then exit.
    Poll {
        /// Shared watch flags (project + label/state/safety/runtime).
        #[command(flatten)]
        args: WatchArgs,
    },
    /// Long-running: poll on `--interval-secs`, dispatching new issues each cycle.
    Listen {
        /// Shared watch flags (project + label/interval/state/safety/runtime).
        #[command(flatten)]
        args: WatchArgs,
    },
}

/// The shared flag set for both `tm watch poll` and `tm watch listen`.
///
/// Why: poll and listen take the same inputs (board, routing label, issue state,
/// the dry-run/execute safety gate, and the spawn runtime), plus listen's poll
/// interval. Flattening one struct into both keeps the surface identical and the
/// dispatch wiring DRY.
/// What: the `<project>` positional (`owner/repo` or a configured name) and the
/// `--label` / `--interval-secs` / `--state` / `--dry-run` / `--execute` /
/// `--runtime` flags. Safety: default is dry-run; `--execute` is required to
/// actually spawn work, and `--dry-run` (the default) always wins if both appear.
/// Test: `cli_parses_watch_*` assert the parsed fields.
#[derive(Debug, clap::Args)]
pub(crate) struct WatchArgs {
    /// Board to watch: an `owner/repo` (e.g. `bobmatnyc/trusty-tools`) OR a
    /// registered project name resolved via the `watch:` config section.
    pub(crate) project: String,

    /// Routing label; only issues carrying it are picked up (default `tm-agent`).
    #[arg(long)]
    pub(crate) label: Option<String>,

    /// `listen`-mode poll interval in seconds (default 60).
    #[arg(
        long = "interval-secs",
        help = "Poll interval in seconds (listen mode only; ignored by poll)"
    )]
    pub(crate) interval_secs: Option<u64>,

    /// Which issues to consider: `open` (default) or `all`.
    #[arg(long, value_enum)]
    pub(crate) state: Option<crate::commands::watch::github::IssueState>,

    /// List matched issues and what WOULD run without spawning anything.
    ///
    /// This is the DEFAULT behaviour; the flag exists to make the safe intent
    /// explicit and, if both `--dry-run` and `--execute` are passed, dry-run wins.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Actually spawn a managed session per matched issue (the explicit opt-in).
    ///
    /// Without this flag, `tm watch` only describes what it would do. This guard
    /// makes accidental mass-execution against real repos impossible.
    #[arg(long)]
    pub(crate) execute: bool,

    /// Runtime backend for spawned sessions: `claude-code` (default) or `tcode`.
    #[arg(long, default_value = "claude-code", value_enum)]
    pub(crate) runtime: trusty_mpm::runtime::RuntimeKind,
}
