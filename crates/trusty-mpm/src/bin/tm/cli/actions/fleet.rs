//! `tm fleet` — set up and inspect the Architect, the one supervisor per user (#8436).
//!
//! Why: the name must not collide with `tm supervisor` (the unattended
//! auto-resumer) or `tm overseer` (ruling 2026-09-23).
//! What: [`FleetAction`] — `init`, `status`, `relaunch` (#9092), `bind`
//! (#8878 PR2a) and `project` (#10895), whose [`FleetProjectAction`] edits the
//! Architect's watch list.
//! Test: `cli_parses_fleet_init`, `cli_parses_fleet_status`,
//! `cli_parses_fleet_relaunch`, `cli_parses_fleet_bind`,
//! `cli_parses_fleet_project`.

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
    /// the `opus` alias. Writes no twin grant. Creates `local/watch.json`
    /// from the live `tm-*` sessions when it is absent and never changes an
    /// existing one (#11012). A second run changes nothing and says so.
    /// Refuses when another Architect is already set up.
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
        /// Move a dashboard this Architect already runs by hand under the
        /// login start (stop it, start it from launchd, put it back if that
        /// fails). Without it a running dashboard is reported and left alone.
        #[arg(long)]
        dashboard_takeover: bool,
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
    /// Restart the Architect's `claude` in its running session and bind it.
    ///
    /// Stops the `claude` in the Architect's tmux session, starts a new one
    /// with the flags `init` uses, and records it as the Architect, so its
    /// anchor writes are allowed again. Removes the replaced launch record
    /// and re-registers with the daemon. Use this instead of a bare `tm` in
    /// the Architect's pane (#9092). Exits 1 when no session runs; start one
    /// with `tm fleet init`.
    Relaunch {
        /// Project directory (default: the Architect recorded in `[supervisor]
        /// projects`, else `~/trusty-mpm-projects/architect`).
        #[arg(long)]
        dir: Option<String>,
        /// Architect tmux session name (default: the name `init` recorded,
        /// else `tm-architect`).
        #[arg(long)]
        session: Option<String>,
    },
    /// Bind the running Architect `claude` this command runs under (#8878).
    ///
    /// Run from the Architect's own tmux pane when its `claude` is not bound
    /// (no launch record). Asks for Touch ID or the login password, then
    /// records that `claude` as the Architect and grants presence. Refuses,
    /// writing nothing, for any other session.
    Bind,
    /// Add, remove or list the projects the Architect watches (#10895).
    ///
    /// The list lives in the Architect's `local/watch.json`. Keys are project
    /// registry keys; the commands read the registry from disk, so they work
    /// while the daemon is down. The first command run with no list imports
    /// the hand-written `## Watch set` lines of the Architect's `CLAUDE.md`.
    Project {
        /// Watch-list action to perform.
        #[command(subcommand)]
        action: FleetProjectAction,
    },
}

/// Actions for `tm fleet project` (#10895).
#[derive(Debug, Subcommand)]
pub(crate) enum FleetProjectAction {
    /// Watch a registered project: write its record, then add it to the list.
    ///
    /// Writes `local/projects/<key>.md` from the record template when it is
    /// absent, then adds the key to the list. Refuses, writing nothing, a
    /// project the registry does not hold.
    Add {
        /// Registry key, or the project's checkout directory.
        project: String,
        /// Architect directory (default: the recorded Architect, else
        /// `~/trusty-mpm-projects/architect`).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Stop watching a project; its record is kept and marked removed.
    ///
    /// Appends `- Watch: removed <time>` under the record's first heading and
    /// drops the key from the list. Refuses, writing nothing, a key the list
    /// does not hold.
    Remove {
        /// Registry key of a watched project.
        key: String,
        /// Architect directory (default: the recorded Architect, else
        /// `~/trusty-mpm-projects/architect`).
        #[arg(long)]
        dir: Option<String>,
    },
    /// List the watched projects with their path, alias and live sessions.
    List {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
        /// Architect directory (default: the recorded Architect, else
        /// `~/trusty-mpm-projects/architect`).
        #[arg(long)]
        dir: Option<String>,
    },
}
