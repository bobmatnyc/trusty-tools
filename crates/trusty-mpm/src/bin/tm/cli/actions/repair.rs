//! `tm repair` deploy-state recovery command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`RepairAction`] — `deploy` and `push-guard`.
//! Test: `cli_parses_repair_deploy`, `cli_parses_repair_push_guard` in `tests.rs`.

use clap::Subcommand;

/// Actions for the `repair` subcommand.
///
/// Why: scoped sub-actions keep `tm repair` extensible — future variants can
/// cover other deploy artefacts without changing the top-level command surface.
/// What: `Deploy` covers agent and skill manifests; `PushGuard` retrofits the
/// #2867 cross-branch push guard onto an already-provisioned clone.
/// Test: `cli_parses_repair_deploy`, `cli_parses_repair_push_guard`.
#[derive(Debug, Subcommand)]
pub(crate) enum RepairAction {
    /// Repair the agent/skill deploy state in `~/.claude/`.
    ///
    /// Why: a crash during `tm install` or `tm sessions start` may leave stale
    /// `.tmp` staging files in `~/.claude/agents/` or `~/.claude/skills/`, or
    /// leave either manifest corrupt. This command removes the orphans and
    /// validates both manifests.
    /// What: for each target directory (`~/.claude/agents/`, skill subdirs under
    /// `~/.claude/skills/`), removes `*.tmp` orphans and validates the manifest.
    /// With `--force`, resets a corrupt agent manifest to empty so the next
    /// `tm install` performs a fresh full deploy.
    /// Test: `cli_parses_repair_deploy`.
    Deploy {
        /// Reset a corrupt manifest to empty (triggers full re-deploy on next
        /// `tm install`). Without this flag, a corrupt manifest is reported
        /// but not modified.
        #[arg(long)]
        force: bool,
    },

    /// Retrofit the #2867 cross-branch `pre-push` guard onto an existing clone.
    ///
    /// Why: the guard is installed automatically only when trusty-mpm CLONES a
    /// repository, so every base clone that already existed when the guard
    /// shipped is unprotected — including long-lived ones shared by dozens of
    /// worktrees, which is exactly the population #2867 happened in. Without
    /// this verb there is no supported way to close that gap.
    /// What: installs the guard into the clone's SHARED hooks directory
    /// (`$GIT_COMMON_DIR/hooks`), which every linked and ad-hoc worktree of
    /// that clone consults — so one invocation from any worktree covers them
    /// all, with no git config write. Idempotent: re-running an up-to-date
    /// guard writes nothing. It never overwrites a `pre-push` it does not own
    /// (a foreign hook, a symlink, an unreadable file, or a `core.hooksPath`
    /// redirect all produce a refusal that names the reason). `--dry-run`
    /// reports what would happen and writes nothing at all.
    /// Test: `cli_parses_repair_push_guard`, `cli_parses_repair_push_guard_flags`.
    PushGuard {
        /// Repository (or any worktree of it) to protect. Defaults to the
        /// current directory.
        #[arg(long)]
        path: Option<String>,
        /// Report what would happen without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Recover a corrupt managed-session store (`sessions.json`).
    ///
    /// Why (#5007): the store is read before every write, so a file the daemon
    /// cannot parse is a file it can never rewrite — the corruption is
    /// permanent until something outside the daemon repairs it. On 2026-08-06
    /// that something was a human hand-truncating JSON at 09:00 while every
    /// session mutation failed. This is that repair, automated.
    /// What: backs the file up to a timestamped sibling
    /// (`sessions.json.corrupt-backup-<YYYYMMDD-HHMMSS>`) and truncates it to
    /// the longest complete JSON document at its head. It REFUSES when the
    /// bytes it would discard name a session id the recovered document does not
    /// contain, unless `--force`. `--dry-run` reports the cut and writes
    /// nothing. A store that parses is left alone.
    /// Test: `cli_parses_repair_session_store`,
    /// `cli_parses_repair_session_store_flags`.
    SessionStore {
        /// Store file to repair. Defaults to
        /// `~/.trusty-mpm/session-manager/sessions.json`.
        #[arg(long)]
        path: Option<String>,
        /// Report the cut without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Discard the trailing bytes even when they name a session id the
        /// recovered document does not contain.
        #[arg(long)]
        force: bool,
    },
}
