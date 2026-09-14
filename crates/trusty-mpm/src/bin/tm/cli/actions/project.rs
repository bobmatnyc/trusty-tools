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
    /// Grant (or revoke) consent for a project's `[session] plugins` opt-ins.
    ///
    /// A `[session] plugins` list ships with the cloned repo itself, and a
    /// Claude Code plugin brings its own skills, commands and hooks into every
    /// session in the project, so tm refuses to honor the list until the
    /// operator explicitly runs this command. Trust is recorded in USER-scope
    /// state under `~/.trusty-tools/trusty-mpm/project-trust.json` — never
    /// inside the repo — so a cloned repo can never self-trust.
    ///
    /// #7892: this no longer affects MCP servers. A user-scope server loads in
    /// every session with no grant, and a project's `.mcp.json` follows Claude
    /// Code's own approval.
    ///
    /// IMPORTANT: trust is PER-DIRECTORY (a canonicalized path), not
    /// per-repo-content. Replacing what's checked out at an already-trusted
    /// path (re-cloning a different repo into the same path, or checking out
    /// an attacker-controlled branch/remote in place) silently inherits the
    /// existing grant. If you replace a trusted directory's contents,
    /// `--revoke` first and re-trust afterward — or clone to a new path,
    /// which starts untrusted by default.
    Trust {
        /// Project directory to trust or revoke (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
        /// Revoke trust instead of granting it.
        #[arg(long)]
        revoke: bool,
    },
}
