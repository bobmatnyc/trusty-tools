//! `tm project` (singular, local-registry) command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap. Distinct from the plural
//! `tm projects` registry-B surface in `actions::projects`.
//! What: [`ProjectAction`] — `init`/`list`/`info`.
//! Test: exercised via `cli::Command::Project` parse coverage in `tests.rs`.

use clap::Subcommand;

/// Actions for the `project` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum ProjectAction {
    /// Register a working directory as a trusty-mpm project.
    Init {
        /// Directory to register (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// List all registered projects with their status.
    List,
    /// Show the current project's registered info and config.
    Info {
        /// Project directory (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
}
