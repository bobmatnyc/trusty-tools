//! `tm memory` command group — deterministic palace maintenance.
//!
//! Why (issue #4837): bulk-loading a directory of memory files into a
//! trusty-memory palace is ETL, not reasoning. Doing it through an agent cost
//! 622k tokens for 120 files; this command group is the zero-inference path.
//! What: [`MemoryAction`] — currently just `import`.
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
}
