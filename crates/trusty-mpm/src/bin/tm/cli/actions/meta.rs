//! `tm meta` standalone-metaharness command group (#1045).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`MetaAction`] — the `run` verb.
//! Test: `cli_parses_meta_run*` in `tests.rs`.

use clap::Subcommand;

/// Verbs for the `tm meta` standalone-metaharness command group (#1045).
///
/// Why: `meta` is a group rather than a bare command because future work items
/// will add sibling verbs (e.g. inspecting transcripts under
/// `.trusty-mpm/meta-runs/`); modelling it as a sub-subcommand enum from the
/// start keeps that surface extensible without a breaking CLI change.
/// What: a single `Run` variant carrying the `--demo` flag, the optional
/// `--project <PATH>` working-directory argument, the `--no-provision` flag, and
/// the `--timeout-secs <N>` session-exit poll budget.
/// Test: `cli_parses_meta_run`, `cli_parses_meta_run_demo`,
/// `cli_parses_meta_run_project`, `cli_parses_meta_run_no_provision`,
/// `cli_parses_meta_run_timeout`, `cli_meta_requires_action` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum MetaAction {
    /// Boot the metaharness for a single run.
    ///
    /// Why: this is the harness's primary entry point — it deploys the custom
    /// instructions and launches a REAL `claude` tmux session rooted at the
    /// project dir (#1049). With `--demo` (#1051) it additionally attaches a
    /// bundled task instructing the session to write `hello_metaharness.txt`,
    /// polls for the session to exit, and verifies the artifact, exiting 0 on
    /// success and non-zero on failure/timeout.
    /// What: `--demo` attaches + verifies the bundled demo task; `--project
    /// <PATH>` sets the working directory (defaults to the cwd); `--no-provision`
    /// is a CURRENT NO-OP (the POC always uses the `--project` dir in place, so
    /// there is no provisioning/clone step to skip — the flag reserves that future
    /// seam); `--timeout-secs <N>` bounds the session-exit poll (default
    /// [`super::commands::meta::DEFAULT_TIMEOUT_SECS`]).
    /// Test: `cli_parses_meta_run*` in `tests.rs`; handler behaviour in the
    /// `commands::meta` unit tests.
    Run {
        /// Attach + verify the bundled demo task (writes hello_metaharness.txt).
        #[arg(long)]
        demo: bool,

        /// Working directory the metaharness operates in (defaults to the cwd).
        #[arg(long)]
        project: Option<std::path::PathBuf>,

        /// Use the local `--project` dir in place (CURRENTLY A NO-OP).
        ///
        /// The POC always operates on the local `--project` dir directly, so
        /// there is no provisioning / git-clone step to skip — passing this flag
        /// (or not) changes nothing today. It is kept to make the in-place intent
        /// explicit and to reserve the seam for a future provisioned/clone path.
        #[arg(long)]
        no_provision: bool,

        /// Seconds to wait for the launched session to exit before timing out.
        #[arg(long)]
        timeout_secs: Option<u64>,
    },
}
