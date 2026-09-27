//! `tm memory` command group — deterministic palace maintenance.
//!
//! Why (issue #4837): bulk-loading a directory of memory files into a
//! trusty-memory palace is ETL, not reasoning. Doing it through an agent cost
//! 622k tokens for 120 files; this command group is the zero-inference path.
//! What: [`MemoryAction`] — `import`, plus `import-auto-memory` (#7685), the
//! one-way migration of Claude Code's own auto-memory store into the palace,
//! plus `recall` / `remember` / `note` (#8352), the no-MCP palace verbs.
//! Test: `cli_parses_memory_import*`, `cli_parses_memory_recall*` in `tests.rs`.

use std::path::PathBuf;

use clap::Subcommand;

/// `tm memory` actions.
///
/// #8352: `recall`, `remember` and `note` are the FALLBACK path for a session
/// whose `mcp__trusty-memory__*` tools are unavailable. They call the same
/// daemon methods the MCP tools do, over its Unix socket, so nothing in this
/// process ever opens a palace store the daemon is serving (#1078).
#[derive(Debug, Subcommand)]
pub(crate) enum MemoryAction {
    /// Recall memories from the palace — the no-MCP `memory_recall` (#8352).
    ///
    /// Use this when the `mcp__trusty-memory__*` tools are unavailable. The
    /// palace is the session's own — `TRUSTY_MEMORY_PALACE`, else the
    /// committed pin, else the repo slug — unless `--palace` names another.
    /// With no palace resolvable at all, trusty-memory answers with an index of
    /// the palaces on this host, naming the one to pass next.
    Recall {
        /// What to recall.
        query: String,
        /// Palace to search. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Hits to return. Defaults to trusty-memory's own default.
        #[arg(long)]
        top_k: Option<u64>,
        /// Restrict the semantic layer to one room.
        #[arg(long)]
        room: Option<String>,
        /// Restrict the search to the rooms one wing owns.
        #[arg(long, conflicts_with = "room")]
        wing: Option<String>,
        /// Drop query-scored hits below this relevance floor (0.4 is the
        /// recommended value for PM-context recall).
        #[arg(long)]
        min_score: Option<f64>,
        /// Print the machine-readable envelope instead of the human summary.
        #[arg(long)]
        json: bool,
        /// trusty-memory socket path. Defaults to `TRUSTY_MEMORY_SOCKET`, else
        /// the derived one.
        #[arg(long)]
        memory_socket: Option<PathBuf>,
    },

    /// Store a memory in the palace — the no-MCP `memory_remember` (#8352).
    ///
    /// The daemon's content gates still apply: very short text with no context,
    /// auto-capture noise, and secret-shaped content are refused there, and the
    /// report says so.
    Remember {
        /// The memory text.
        text: String,
        /// Palace to write to. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Room to file it in.
        #[arg(long)]
        room: Option<String>,
        /// Tag to store alongside it; repeat for several.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Print the machine-readable envelope instead of the human summary.
        #[arg(long)]
        json: bool,
        /// trusty-memory socket path. Defaults to the derived one.
        #[arg(long)]
        memory_socket: Option<PathBuf>,
    },

    /// Store a short curated fact — the no-MCP `memory_note` (#8352).
    ///
    /// The shortcut for high-signal one-liners ("deploy target is prod-east"):
    /// stored at importance 1.0 so it surfaces in the palace's essentials.
    Note {
        /// The fact.
        content: String,
        /// Palace to write to. Defaults to the session's own.
        #[arg(long)]
        palace: Option<String>,
        /// Room to file it in.
        #[arg(long)]
        room: Option<String>,
        /// Tag to store alongside it; repeat for several.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Print the machine-readable envelope instead of the human summary.
        #[arg(long)]
        json: bool,
        /// trusty-memory socket path. Defaults to the derived one.
        #[arg(long)]
        memory_socket: Option<PathBuf>,
    },

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
