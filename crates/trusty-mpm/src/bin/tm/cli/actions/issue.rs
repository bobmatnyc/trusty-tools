//! `tm issue` state-management command group (#1246).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`IssueCmd`] — `seed-labels`/`transition`/`current`/`states`/
//! `seed-config`/`repair`.
//! Test: `cli_parses_issue_*` in `tests.rs`.

use clap::Subcommand;

/// Verbs for the `tm issue` state-management command group (#1246).
///
/// Why: each operation (seed labels, transition state, inspect, repair) is a
/// distinct, scriptable verb; a sub-subcommand enum keeps them discoverable and
/// individually parseable.
/// What: `SeedLabels` (idempotent create-missing), `Transition` (validated
/// atomic state change), `Current` (read state from labels), `States` (list the
/// model), `SeedConfig` (write the default YAML to disk), `Repair` (resolve a
/// multi-state issue).
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
    /// Write the embedded default model to the user config path.
    SeedConfig {
        /// Overwrite an existing user config file.
        #[arg(long)]
        force: bool,
    },
    /// Resolve a mid-transition issue carrying multiple state labels.
    Repair {
        /// Issue number.
        issue: u64,
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
}
