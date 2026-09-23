//! `tm issue` state-management command group (#1246).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`IssueCmd`] — `seed-labels`/`transition`/`current`/`states`/
//! `standard`/`seed-config`/`repair`/`audit`.
//! Test: `cli_parses_issue_*` in `tests.rs`.

use clap::Subcommand;

/// Verbs for the `tm issue` state-management command group (#1246).
///
/// Why: each operation (seed labels, transition state, inspect, repair) is a
/// distinct, scriptable verb; a sub-subcommand enum keeps them discoverable and
/// individually parseable.
/// What: `SeedLabels` (idempotent create-missing), `Transition` (validated
/// atomic state change), `Current` (read state from labels), `States` (list the
/// model), `Standard` (print the effective ticketing standard, #6918),
/// `SeedConfig` (write the default lifecycle YAML plus the `agents.ticketing`
/// block, #7067), `Repair` (resolve a multi-state issue), `Audit` (verify a
/// filed issue against the standard, #7097).
/// Test: `cli_parses_issue_*` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum IssueCmd {
    /// Create any missing labels (states + extra families) in the repo.
    SeedLabels {
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Print what would be created without creating anything.
        #[arg(long)]
        dry_run: bool,
        /// Seed only these labels (#7983). Repeatable. A value ending in `:` or
        /// `/` selects that family (`status:`, `ws/`); any other value is an
        /// exact label name. A value matching nothing is an error.
        #[arg(long, value_name = "NAME_OR_FAMILY")]
        only: Vec<String>,
    },
    /// Move an issue to `<to-state>`, validating the edge against the model.
    Transition {
        /// Issue number (e.g. `1232`).
        issue: u64,
        /// Target state name (e.g. `approved`).
        to_state: String,
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Optional note appended to the transition audit comment.
        #[arg(long)]
        note: Option<String>,
    },
    /// Report an issue's current state, derived from its labels.
    Current {
        /// Issue number.
        issue: u64,
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// List the configured states and transitions (reads YAML only).
    States {
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// Print the ticketing standard in effect (#6918; reads config, no `gh`).
    Standard {
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// Write the default lifecycle model and the `agents.ticketing` block to the user config path.
    SeedConfig {
        /// Overwrite an existing user config file.
        #[arg(long)]
        force: bool,
    },
    /// Verify a filed issue carries a project, a milestone, and a component label (#7097).
    Audit {
        /// Issue number. Omit and pass `--recent`/`--since` to audit a window.
        #[arg(conflicts_with_all = ["recent", "since"])]
        issue: Option<u64>,
        /// Audit the N most recently created OPEN issues instead.
        #[arg(long, conflicts_with = "since")]
        recent: Option<usize>,
        /// Audit every OPEN issue created on or after this `YYYY-MM-DD` date.
        #[arg(long)]
        since: Option<String>,
    },
    /// File and maintain an epic tracker and its phase issues (#8447).
    #[command(subcommand)]
    Epic(EpicCmd),
    /// Resolve a mid-transition issue carrying multiple state labels.
    Repair {
        /// Issue number.
        issue: u64,
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
}

/// Verbs for the `tm issue epic` command group (#8447).
///
/// Why: filing an epic by hand is eleven ordered `gh` calls whose partway
/// failure is normal, and regenerating its `phases` block by hand is a session
/// retyping a markdown table — the drift `TICKETING.md`'s `epics.*` rules exist
/// to prevent. Two verbs make the deterministic half code.
/// What: `Create` (parse a committed plan document, file the tracker, rename it
/// once its number is known, then file each phase as a native sub-issue) and
/// `Sync` (regenerate the `phases` block wholesale from live child state).
/// Test: `cli_parses_issue_epic_create`,
/// `cli_parses_issue_epic_create_repeatable_components`,
/// `cli_parses_issue_epic_sync`.
#[derive(Debug, Subcommand)]
pub(crate) enum EpicCmd {
    /// File an epic tracker and its phase issues from a committed plan document.
    Create {
        /// Path to the plan document; it must already be on `origin/main`.
        #[arg(long, value_name = "PATH")]
        from: std::path::PathBuf,
        /// Milestone title applied to the tracker and to every phase.
        #[arg(long, value_name = "TITLE")]
        milestone: Option<String>,
        /// Component label. Repeatable; at least one is required.
        #[arg(long, value_name = "LABEL")]
        component: Vec<String>,
        /// Type label each phase issue carries (one of the existing six).
        #[arg(long, value_name = "TYPE", default_value = "enhancement")]
        phase_type: String,
        /// Owner-scoped GitHub Projects number to attach each issue to.
        #[arg(long, value_name = "NUMBER")]
        project: Option<u64>,
        /// Workstream name behind `ws/<session>`; defaults to the tmux session.
        #[arg(long, value_name = "NAME")]
        session: Option<String>,
        /// Resume into an existing tracker instead of searching for one.
        #[arg(long, value_name = "NUMBER")]
        tracker: Option<u64>,
        /// Report what would be filed without mutating anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Regenerate a tracker's `phases` block from its live child issues.
    Sync {
        /// The tracker's issue number.
        epic: u64,
    },
}
