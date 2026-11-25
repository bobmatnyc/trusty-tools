//! `tm project` (singular, local-registry) command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap. Distinct from the plural
//! `tm projects` registry-B surface in `actions::projects`.
//! What: [`ProjectAction`] — `init`/`list`/`info`/`unadopt`/`trust`.
//! Test: exercised via `cli::Command::Project` parse coverage in `tests.rs`.

use clap::Subcommand;

/// Actions for the `project` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum ProjectAction {
    // #10586 (owner ruling 2026-10-31): deprecated; it writes the same
    // registry-B record `tm projects register` writes.
    /// Register a working directory as a trusty-mpm project (deprecated: use `tm projects register`).
    ///
    /// #10592: init also changes the checkout's git metadata and the adoption
    /// ledger. Each change and its reversal:
    ///
    /// - `git config --local maintenance.auto false` and `gc.auto 0` (#7171).
    ///   Reverse: `git config --local --unset maintenance.auto` and
    ///   `git config --local --unset gc.auto`.
    /// - `/.trusty-mpm-worktree` and `/.trusty-mpm/` appended to
    ///   `$(git rev-parse --git-common-dir)/info/exclude` (#8511). Reverse:
    ///   delete those two lines from that file.
    /// - each tree's in-tree `.trusty-mpm-worktree` marker moved to
    ///   `<common-dir>/worktrees/<name>/trusty-mpm-worktree` (main checkout:
    ///   `.git/trusty-mpm-worktree`) (#8511). Reverse: move each file back to
    ///   `<tree>/.trusty-mpm-worktree`.
    /// - every pre-existing worktree recorded in
    ///   `~/.trusty-mpm/project-registry/worktrees.json` (#7588). Reverse:
    ///   `tm project unadopt <path>...` (ledger only; `--dry-run` previews).
    Init {
        /// Directory to register (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// List all registered projects with their status (deprecated: use `tm projects list`).
    List,
    /// Show the current project's registered info and config.
    Info {
        /// Project directory (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Remove worktrees from the adoption ledger by path, ledger only (#10592).
    ///
    /// Edits `~/.trusty-mpm/project-registry/worktrees.json` and nothing else:
    /// it never edits or deletes a worktree folder and never touches git
    /// metadata. A later `tm project init` on the same checkout adopts the
    /// worktrees again.
    Unadopt {
        /// Worktree paths to remove from the ledger.
        #[arg(required = true)]
        paths: Vec<std::path::PathBuf>,
        /// Report what would be removed and write nothing.
        #[arg(long)]
        dry_run: bool,
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
