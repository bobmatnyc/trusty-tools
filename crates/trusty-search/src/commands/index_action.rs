//! The `trusty-search index <action>` subcommand enum.
//!
//! Why: extracted from `main.rs` (#767) — adding `index add
//! --allow-sensitive-path` pushed that file past its frozen SLOC budget, and
//! CLAUDE.md's rule is that the split ships in the PR the cap blocks. This enum
//! is the natural extraction target: it is a leaf clap type with no dependency
//! on anything else in `main.rs`.
//! What: `IndexAction` and nothing else; `main.rs` re-imports it.
//! Test: `cargo run -p trusty-search -- index --help` lists every variant;
//! `index_remove::tests::*` cover path resolution.

use clap::Subcommand;

/// Subcommands attached to the `index` command.
///
/// Why: `trusty-search index` historically only registered + reindexed. Issue
/// #40 adds a `remove` action that deletes the daemon-side registration AND
/// drops the matching entry from `~/.config/trusty-search/config.yaml`. Using
/// an enum here keeps the default register-and-reindex flow intact (clap's
/// `args_conflicts_with_subcommands` lets the top-level args coexist with an
/// optional subcommand) while opening the door to additional actions
/// (`rename`, `move`, …) without further breaking changes.
/// What: `Remove` drops a registration; `Add` writes to the allowlist so the
/// path can later be indexed; `List` displays the current allowlist.
/// Test: `cargo run -p trusty-search -- index --help` lists every variant;
/// `index_remove::tests::*` cover path resolution.
#[derive(Subcommand)]
pub(crate) enum IndexAction {
    /// Remove an index registration AND its on-disk data (daemon + config + allowlist)
    ///
    /// Deletes the daemon-side registration matching the given (or
    /// auto-detected) path via `DELETE /indexes/:id?delete_data=true`, drops
    /// the matching entry from `~/.config/trusty-search/config.yaml`, and also
    /// removes it from the allowlist
    /// (`~/.config/trusty-search/indexes.toml`).
    ///
    /// The on-disk redb / HNSW data goes with it. Pass `--keep-data` to
    /// deregister only and leave the corpus in place — re-registering the same
    /// path then reuses it. Because the default destroys data, the command
    /// asks for confirmation first; `--yes` answers it for a script.
    ///
    /// AGENT USAGE: use this when a project has been moved or deleted so the
    /// daemon stops reporting an empty/stale entry. Auto-detect from CWD when
    /// possible; pass an explicit PATH when running from outside the project.
    /// Non-interactive callers must pass `--yes` (or `--keep-data`).
    ///
    /// Examples:
    ///   trusty-search index remove
    ///   trusty-search index remove ~/Projects/old-app --yes
    ///   trusty-search index remove --keep-data
    Remove {
        /// Directory of the index to remove (default: auto-detected from CWD)
        path: Option<std::path::PathBuf>,

        /// Deregister only — leave the on-disk index data in place (issue #6422)
        ///
        /// The opt-out from the destructive default. Without it the index's
        /// redb corpus and HNSW snapshot are deleted along with the
        /// registration.
        #[arg(long)]
        keep_data: bool,

        /// Answer the "this cannot be undone" confirmation with yes (issue #6422)
        ///
        /// Only consulted when data is about to be deleted; `--keep-data`
        /// never prompts.
        #[arg(long)]
        yes: bool,
    },

    /// Add a path to the opt-in allowlist (issue #767)
    ///
    /// Writes the path to `~/.config/trusty-search/indexes.toml` so it can
    /// subsequently be registered and indexed. This is the ONLY way to
    /// approve a new path under the default-deny model — the daemon will
    /// refuse `POST /indexes` for any path not in the allowlist.
    ///
    /// Paths matching the hard sensitive-path denylist (e.g. ~/.ssh, /tmp,
    /// ~/.aws) are refused with a clear error even when this command is used.
    ///
    /// Examples:
    ///   trusty-search index add ~/Projects/my-repo
    ///   trusty-search index add .   # adds the current directory
    ///   trusty-search index add /var/folders/../scratch-repo --allow-sensitive-path
    Add {
        /// Directory to approve for indexing
        path: std::path::PathBuf,

        /// Optional human-readable name for the index
        #[arg(short, long)]
        name: Option<String>,

        /// Approve a path under an OS-temp or app-support prefix (issue #767)
        ///
        /// Relaxes ONLY the ephemeral-prefix denylist rows (`/tmp`,
        /// `/private/tmp`, `/var/folders`, `Library/Application Support`) — the
        /// rows for credential directories (`~/.ssh`, `~/.aws`), secret file
        /// names (`.env`), and top-level home directories (`~/Desktop`) are
        /// never relaxed and still refuse the path.
        ///
        /// Use this for a scratch or bake-off project that genuinely lives
        /// under a temp prefix. Without it there is no way to approve such a
        /// root, which is what made the daemon's own `allow_sensitive_path`
        /// opt-in unreachable.
        #[arg(long)]
        allow_sensitive_path: bool,
    },

    /// List all paths currently in the allowlist (issue #767)
    ///
    /// Displays the contents of `~/.config/trusty-search/indexes.toml` — the
    /// single source of truth for what may be indexed. An empty list means
    /// nothing can be indexed (default-deny).
    ///
    /// Examples:
    ///   trusty-search index list
    ///   trusty-search index list --json
    List {
        /// Emit the list as JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Relocate an index to a new root directory (issue #1073)
    ///
    /// Updates the daemon registration and on-disk `indexes.toml` to point at
    /// the new root. Because all chunk/hash keys are root-relative (issue #402),
    /// the existing embedded data is reused as-is — no re-embedding occurs.
    ///
    /// The index being relocated is resolved using the same precedence as other
    /// subcommands: the `-i` / `--index` flag first, then auto-detect from CWD.
    ///
    /// AGENT USAGE: run this when a project directory has been moved on disk.
    /// Follow with `trusty-search index` (without `--force`) to incrementally
    /// re-embed only files that have genuinely changed.
    ///
    /// Examples:
    ///   trusty-search index relocate --to ~/Projects/new-location
    ///   trusty-search index relocate --to /abs/path/to/repo
    Relocate {
        /// New root directory for the index
        #[arg(long = "to")]
        to: std::path::PathBuf,
    },
}
