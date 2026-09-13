//! `tm memory` command group — deterministic palace maintenance.
//!
//! Why (issue #4837): bulk-loading a directory of memory files into a
//! trusty-memory palace is ETL, not reasoning. Doing it through an agent cost
//! 622k tokens for 120 files; this command group is the zero-inference path.
//! What: [`MemoryAction`] — `import`, plus `import-auto-memory` (#7685), the
//! one-way migration of Claude Code's own auto-memory store into the palace.
//! Test: `cli_parses_memory_import*` in `tests.rs`.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum MemoryAction {
    /// Bulk-import a directory of memory `.md` files into a palace.
    ///
    /// Reads every `*.md` file directly inside `<DIR>` (non-recursive), maps
    /// its YAML frontmatter onto drawer fields — the `description` leads the
    /// stored text, and `name` + `metadata.type` + every `[[wikilink]]` target
    /// become tags — and writes it via trusty-memory's JSON-RPC surface.
    ///
    /// Re-running never writes a file twice. A file's own drawer is found by
    /// its slug tag, with drawers that merely link to that slug excluded, so
    /// the match does not depend on the file's prose: a file whose text has
    /// changed since it was imported is still skipped, and the report says its
    /// drawer has drifted. `--refresh` (issue #5044) replaces such a drawer
    /// with the file's current text and requires every drawer the run names to
    /// be retrievable. When several drawers could be the file's own, or the
    /// slug tag is shared by more drawers than one lookup returns, the file is
    /// reported as failed rather than guessed at.
    Import {
        /// Directory of memory `.md` files (scanned non-recursively).
        dir: PathBuf,
        /// Target palace slug (e.g. `trusty-tools`).
        #[arg(long)]
        palace: String,
        /// Parse, derive, and dedup-check without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Replace a drifted drawer with the file's current text, and fail any
        /// file whose drawer is not retrievable — the mode to run immediately
        /// before deleting the source files (issue #5044).
        #[arg(long)]
        refresh: bool,
        /// Print the full JSON report instead of the human summary.
        #[arg(long)]
        json: bool,
        /// Store drawers whose prose trips trusty-memory's secret heuristic
        /// (a localhost URL, a token-shaped identifier) instead of failing them.
        #[arg(long)]
        allow_secret_like: bool,
        /// trusty-memory socket path. Defaults to the derived one (#6286 —
        /// the daemon has no port and publishes no address).
        #[arg(long)]
        memory_socket: Option<std::path::PathBuf>,
    },

    /// Migrate Claude Code's own auto-memory store into the project's palace.
    ///
    /// trusty-memory is the memory; Claude Code's auto memory is the fallback
    /// for when it is down (owner ruling 2026-09-12). This command is what makes
    /// emptying that fallback safe: it reads every fact file under
    /// `<claude-config>/projects/<slug>/memory/`, stores it as a drawer tagged
    /// `migrated-from-auto-memory` plus `name:<name>` and `type:<type>`, moves
    /// the file into `memory.archived-<YYYYMMDD>/`, and only then empties
    /// `MEMORY.md` — archiving a copy of it first.
    ///
    /// Nothing is deleted, and a fact that fails to store is left exactly where
    /// it was, with the index untouched, so a re-run picks up the remainder.
    /// Re-running after a clean migration finds nothing and writes nothing.
    ImportAutoMemory {
        /// Project whose auto-memory store is migrated. Defaults to the cwd.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Target palace slug. Defaults to the slug this project's sessions pin.
        #[arg(long)]
        palace: Option<String>,
        /// Print the full JSON report instead of the human summary.
        #[arg(long)]
        json: bool,
        /// trusty-memory socket path. Defaults to the derived one.
        #[arg(long)]
        memory_socket: Option<PathBuf>,
    },
}
