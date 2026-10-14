//! `tm fleet` — set up and inspect the Architect, the one supervisor per user (#8436).
//!
//! Why: the name must not collide with `tm supervisor` (the unattended
//! auto-resumer) or `tm overseer` (ruling 2026-09-23).
//! What: [`FleetAction`] — `init` and `status`. `add` and `remove` arrive in
//! phase P3 of #8436.
//! Test: `cli_parses_fleet_init`, `cli_parses_fleet_status`.

use clap::Subcommand;

/// Actions for the `fleet` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum FleetAction {
    /// Set up the Architect project and start its session.
    ///
    /// Creates the project directory with a local git repo and no remote,
    /// writes `profile = "supervisor"` to its `.trusty-mpm.toml`, adds its
    /// canonical path to `[supervisor] projects` in
    /// `~/.trusty-mpm/config.toml` (also when a PM runs this), and starts the
    /// session detached as tmux session `tm-architect` (or `--session`) on
    /// the `opus` alias. Writes no twin grant. A second run changes nothing
    /// and says so. Refuses when another Architect is already set up. The
    /// PM-guard lift is the owner's choice and off by default (#8436).
    Init {
        /// Project directory (default: the Architect recorded in `[supervisor]
        /// projects`, else `~/trusty-mpm-projects/architect`).
        #[arg(long)]
        dir: Option<String>,
        /// Set up the project and the grant, but do not start the session.
        #[arg(long)]
        no_launch: bool,
        /// Architect tmux session name (default: the recorded name, else
        /// `tm-architect`); the poller runs as `<name>-poll`. Recorded as
        /// `[supervisor] session` so later runs and `status` use it (#8878).
        #[arg(long)]
        session: Option<String>,
        /// Lift the PM guard for the Architect: set
        /// `TRUSTY_MPM_PM_UNRESTRICTED=1` in its `.claude/settings.json`,
        /// keeping every other key. Without either flag a terminal run asks
        /// `[y/N]` and any other run leaves the guard as it is.
        #[arg(long, conflicts_with = "no_guard_lift")]
        guard_lift: bool,
        /// Remove the guard lift if it is set; the PM guard stays on.
        #[arg(long)]
        no_guard_lift: bool,
    },
    /// Report whether the Architect is set up and running (read-only).
    ///
    /// Checks the `[supervisor] projects` entry, the project's
    /// `profile = "supervisor"`, the Architect's tmux session, and that
    /// session's supervisor launch stamp. Exits 1 when any check fails.
    /// Also reports whether that session's `claude` is bound (#8878).
    Status {
        /// Project directory (default: the Architect recorded in `[supervisor]
        /// projects`, else `~/trusty-mpm-projects/architect`).
        #[arg(long)]
        dir: Option<String>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Architect tmux session name (default: the name `init` recorded,
        /// else `tm-architect`).
        #[arg(long)]
        session: Option<String>,
    },
}
