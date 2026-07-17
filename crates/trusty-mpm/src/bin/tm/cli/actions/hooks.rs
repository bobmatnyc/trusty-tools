//! `tm hooks` project-settings hygiene command group (issue #2940).
//!
//! Why: `tm install` used to wire tm hooks into every discovered project's
//! `.claude/settings.json` on the machine (see
//! `commands::install::install_claude_hooks`'s pre-fix history). This group
//! is the cleanup path for settings files a pre-fix `tm install` already
//! contaminated.
//! What: [`HooksAction`] — currently just `Clean`.
//! Test: `cli_parses_hooks_clean`, `cli_parses_hooks_clean_with_path_and_force`.

use clap::Subcommand;

/// Actions for the `hooks` subcommand (issue #2940).
///
/// Why: scoped so future hook-hygiene verbs (e.g. an `inspect`/`list`) have an
/// obvious home without cluttering the top-level CLI surface.
/// What: `Clean` — see [`Command::Hooks`](crate::cli::Command::Hooks)'s doc
/// comment for the full rationale.
/// Test: `cli_parses_hooks_clean`, `cli_parses_hooks_clean_with_path_and_force`.
#[derive(Debug, Subcommand)]
pub(crate) enum HooksAction {
    /// Remove tm-owned hook entries from project-level `.claude/settings.json`
    /// / `settings.local.json` files.
    ///
    /// Why: a machine that ran a pre-#2940 `tm install` has tm hook entries
    /// scattered across every project under `$HOME` that had a `.claude/`
    /// directory at the time. Those entries no longer serve any purpose (tm
    /// hooks now live solely in the tm-owned `CLAUDE_CONFIG_DIR`) and are a
    /// one-way street away from other harnesses — this command reverses the
    /// damage.
    /// What: dry-run by default — prints every file that WOULD be modified
    /// and which `hooks` event keys carry a tm-owned group, without touching
    /// disk. `--force` applies the change: each modified file is backed up to
    /// `<file>.bak-<unix-epoch-seconds>` before the tm-owned groups are
    /// stripped (every other key is preserved). `--path <dir>` scopes the
    /// scan to exactly `<dir>/.claude/settings.json` and
    /// `<dir>/.claude/settings.local.json`; without it, every
    /// `.claude/settings*.json` discoverable under `$HOME` is scanned (the
    /// same walk a pre-fix `tm install` used to contaminate).
    /// Test: `cli_parses_hooks_clean`, `cli_parses_hooks_clean_with_path_and_force`.
    Clean {
        /// Scope the scan to this single project directory instead of
        /// walking all of `$HOME`.
        #[arg(long)]
        path: Option<String>,
        /// Apply the cleanup (write backups + strip tm-owned entries).
        /// Without this flag, `clean` only reports what it would do.
        #[arg(long)]
        force: bool,
    },
}
