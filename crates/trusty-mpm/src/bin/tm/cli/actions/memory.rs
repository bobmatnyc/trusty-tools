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
    /// Re-running is safe: a file whose drawer is already present is skipped.
    Import {
        /// Directory of memory `.md` files (scanned non-recursively).
        dir: PathBuf,
        /// Target palace slug (e.g. `trusty-tools`).
        #[arg(long)]
        palace: String,
        /// Parse, derive, and dedup-check without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Print the full JSON report instead of the human summary.
        #[arg(long)]
        json: bool,
        /// Store drawers whose prose trips trusty-memory's secret heuristic
        /// (a localhost URL, a token-shaped identifier) instead of failing them.
        #[arg(long)]
        allow_secret_like: bool,
        /// trusty-memory base URL. Defaults to daemon discovery.
        #[arg(long)]
        memory_url: Option<String>,
    },
}
