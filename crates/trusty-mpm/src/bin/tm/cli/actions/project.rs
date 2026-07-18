//! `tm project` (singular, local-registry) command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap. Distinct from the plural
//! `tm projects` registry-B surface in `actions::projects`.
//! What: [`ProjectAction`] — `init`/`list`/`info`/`trust`.
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
    /// Grant (or revoke) consent for a project's `[mcp.custom]` manifest
    /// entries to be bridged into fleet sessions (issue #3033 security fix).
    ///
    /// A project-scope `[mcp.custom]` entry ships with the cloned repo
    /// itself, so `session_launch::custom_mcp` refuses to honor it until the
    /// operator explicitly runs this command. Trust is recorded in USER-scope
    /// state under `~/.trusty-tools/trusty-mpm/project-trust.json` — never
    /// inside the repo — so a cloned repo can never self-trust.
    Trust {
        /// Project directory to trust or revoke (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
        /// Revoke trust instead of granting it.
        #[arg(long)]
        revoke: bool,
    },
}
