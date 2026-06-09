//! trusty-search CLI binary.
//!
//! Why: Single entry point that exposes both project-scoped commands
//! (`search`, `watch`, `status`, `init`, `add`, `remove`, `reindex`) which
//! auto-detect the index from the current working directory, and global
//! commands (`list`, `query`, `health`, `start`, `stop`, `serve`, `completions`)
//! that operate across the registry or manage the daemon.
//!
//! What: Parses CLI args via clap and dispatches to the per-subcommand
//! handlers in [`crate::commands`]. All implementation lives in those
//! modules; this file is purely a clap-to-handler shim plus a top-level
//! error printer.
//!
//! Test: `cargo run -- --help` → renders grouped command list with aliases.
//! `cargo run -- status` from inside this repo → prints `[trusty-search]`
//! detected via `.git`. `cargo test --workspace` → all tests pass.

mod commands;
mod detect;

// Re-export the library's modules into the binary's `crate::` namespace so
// existing `crate::core::*` / `crate::service::*` / `crate::mcp::*` imports
// in `commands/*.rs` resolve without churn after the workspace consolidation.
pub(crate) use trusty_search::{allowlist, config, core, mcp, service};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use colored::Colorize;
use commands::convert::ConvertTarget;
use commands::service::ServiceAction;
use std::io;

/// Machine-wide hybrid code search — BM25 + vector + knowledge graph.
///
/// Run from inside any project and trusty-search auto-detects the index.
/// Use `trusty-search start` to start the background service first.
#[derive(Parser)]
#[command(
    name = "trusty-search",
    version,
    author,
    propagate_version = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    /// Override the auto-detected project index
    #[arg(short = 'i', long, global = true, env = "TRUSTY_INDEX")]
    index: Option<String>,

    /// Output results as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Enable debug logging
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // ── Project commands (auto-detect index from CWD) ──────────────────────
    /// Hybrid search in the current project  [alias: s]
    ///
    /// Runs against a single named index — the one detected from CWD (`.git`,
    /// `.trusty-search` marker) or overridden via the global `--index` flag.
    /// To search across every registered project at once, use `query` instead.
    ///
    /// AGENT USAGE: prefer `search` when the working directory is inside the
    /// project you want to query — it's faster and the intent classifier has
    /// the best signal-to-noise ratio on a single corpus. Use `query` for
    /// cross-repo or unknown-project lookups.
    ///
    /// Examples:
    ///   trusty-search search "fn authenticate"
    ///   trusty-search search "error handling" --intent conceptual
    ///   trusty-search search "TODO FIXME" --intent bugdebt --top-k 20
    ///   trusty-search --index other-proj search "Database pool"
    #[command(alias = "s", display_order = 1)]
    Search {
        /// Search query (natural language or code)
        query: String,

        /// Number of results to return
        #[arg(short = 'k', long, default_value = "10")]
        top_k: usize,

        /// Show full chunk content instead of compact snippet
        #[arg(short, long)]
        full: bool,

        /// Force query intent classification
        #[arg(long, value_enum)]
        intent: Option<IntentArg>,

        /// Skip knowledge graph expansion
        #[arg(long)]
        no_kg: bool,

        /// Pagination offset
        #[arg(long, default_value = "0")]
        offset: usize,

        /// Max token budget for results
        #[arg(long, default_value = "8000")]
        budget: u32,
    },

    /// Watch for changes and keep index updated  [alias: w]
    ///
    /// Examples:
    ///   trusty-search watch
    ///   trusty-search watch ~/Projects/myapp
    #[command(alias = "w", display_order = 2)]
    Watch {
        /// Directory to watch (default: auto-detected project root)
        path: Option<std::path::PathBuf>,
    },

    /// Show daemon status and all index stats  [alias: st]
    ///
    /// Shows daemon liveness, version, and per-index chunk counts.
    /// `health` produces the same output (kept for backward compatibility).
    ///
    /// Examples:
    ///   trusty-search status
    ///   trusty-search status --json
    #[command(alias = "st", display_order = 3)]
    Status,

    /// Show per-stage status for an index, with optional live embed tracking
    ///
    /// Displays per-stage status (lexical / semantic / graph) for the given index.
    /// When the deferred-embed background job is running, shows embed progress as
    /// `embedded / total chunks (N%)`.  When INDEX is omitted, defaults to the
    /// index(es) whose root_path covers the current working directory (same
    /// defaulting convention as `trusty-search index .`).  Multiple matches are
    /// shown in turn.  `--watch` requires an explicit INDEX when >1 match.
    ///
    /// Examples:
    ///   trusty-search index-status              # cwd default
    ///   trusty-search index-status my-index --watch
    ///   trusty-search index-status my-index --json
    #[command(display_order = 4)]
    #[allow(clippy::doc_markdown)]
    IndexStatus {
        /// Index ID to inspect; omit to use the index(es) covering the cwd
        index_id: Option<String>,

        /// Poll every ~1 s until semantic embedding finishes, then exit
        #[arg(long)]
        watch: bool,
    },

    /// Register and index a project, or remove an existing registration  [alias: idx]
    ///
    /// With no subcommand: registers the index with the daemon if needed, then
    /// runs a reindex with a live progress bar. Skips the reindex if the index
    /// already has chunks indexed (use --force to override).
    ///
    /// With `remove`: deletes the daemon-side registration matching the given
    /// (or auto-detected) path AND drops the matching entry from
    /// `~/.config/trusty-search/config.yaml`. The on-disk redb data is left
    /// intact — re-registering reuses it.
    ///
    /// When run with no PATH argument, trusty-search looks for a
    /// `.trusty-search.yaml` file in the current directory and uses its
    /// `name`, `path`, and `exclude` values as defaults. CLI flags always
    /// override the config file. (For multi-index polyrepos, use the separate
    /// `trusty-search.yaml` manifest — no leading dot.)
    ///
    /// AGENT USAGE: prefer `trusty-search index` (no subcommand) to onboard a
    /// repo before any search calls. Use `trusty-search index remove` to
    /// clean up after a project is deleted or relocated; this avoids stale
    /// entries appearing in `trusty-search list`.
    ///
    /// Examples:
    ///   trusty-search index                   # CWD, name from basename or .trusty-search.yaml
    ///   trusty-search index ~/Projects/myapp
    ///   trusty-search index --force           # full reindex even if up-to-date
    ///   trusty-search index --exclude data/ --exclude "*.db"
    ///   trusty-search index remove            # remove registration for CWD's project
    ///   trusty-search index remove ~/Projects/old-app
    #[command(
        alias = "idx",
        display_order = 4,
        args_conflicts_with_subcommands = true
    )]
    Index {
        /// Directory to register and index (default: CWD, or `.trusty-search.yaml` `path`)
        path: Option<std::path::PathBuf>,

        /// Index name (default: directory basename, or `.trusty-search.yaml` `name`)
        #[arg(short, long)]
        name: Option<String>,

        /// Force a full reindex even if the index already has chunks
        #[arg(short, long)]
        force: bool,

        /// Additional glob exclusion patterns (override `.trusty-search.yaml` `exclude`)
        #[arg(long)]
        exclude: Vec<String>,

        /// Hard wall-clock cap on the foreground wait (seconds).
        ///
        /// When omitted (the default) the CLI uses progress-aware stall
        /// detection: it keeps displaying progress as long as files are being
        /// indexed and only detaches after no progress for 120 s.  Pass an
        /// explicit value (e.g. `--timeout 1200`) to impose a hard cap instead.
        /// Use `--timeout 0` to wait forever.
        #[arg(long)]
        timeout: Option<u64>,

        /// Create a Stage-1-only ("daemonized ripgrep") index (issue #109, Phase 1).
        ///
        /// When set, the reindex pipeline returns after Stage 1 (walker +
        /// chunker + BM25 + redb persist) and never embeds. The index stays
        /// at `status: indexed_lexical` permanently. Useful for callers who
        /// want exact-match / BM25 search without paying for the embedder's
        /// disk + CPU cost. The choice is persisted to `indexes.toml` so it
        /// survives daemon restarts; to opt back in, delete and re-create
        /// the index without this flag.
        #[arg(long)]
        lexical_only: bool,

        /// Skip the Knowledge Graph (Phase 3 / KG rebuild) during reindex (issue #313).
        ///
        /// When set, the reindex pipeline skips tree-sitter symbol extraction
        /// and the petgraph KG build. Stages 1 and 2 (BM25 + vector embed)
        /// still run normally. The `get_call_chain` endpoint returns a
        /// structured 503 `kg_unavailable` response for this index.
        ///
        /// The choice is persisted to `indexes.toml` so it survives daemon
        /// restarts; to opt back in, delete and re-create the index without
        /// this flag. Also expressible as `TRUSTY_NO_KG=1` (machine-wide
        /// default) or `skip_kg: true` in `trusty-search.yaml`.
        #[arg(long)]
        no_kg: bool,

        /// Optional subcommand. When absent, the default register+reindex flow runs.
        #[command(subcommand)]
        action: Option<IndexAction>,
    },

    /// Register current directory as a named index (see `index`)
    ///
    /// Kept for backward compatibility. Prefer `trusty-search index`, which
    /// registers AND indexes in one step.
    ///
    /// Examples:
    ///   trusty-search init
    ///   trusty-search init ~/Projects/myapp --name myapp-prod
    #[command(alias = "i", display_order = 4)]
    Init {
        /// Directory to register (default: CWD)
        path: Option<std::path::PathBuf>,

        /// Index name (default: directory basename)
        #[arg(short, long)]
        name: Option<String>,

        /// Additional glob exclusion patterns
        #[arg(long)]
        exclude: Vec<String>,
    },

    /// Add or update a single file in the index
    ///
    /// Examples:
    ///   trusty-search add src/main.rs
    #[command(display_order = 5)]
    Add {
        /// File to index
        file: std::path::PathBuf,
    },

    /// Remove a file from the index  [alias: rm]
    ///
    /// Examples:
    ///   trusty-search remove src/old.rs
    #[command(alias = "rm", display_order = 6)]
    Remove {
        /// File to remove
        file: std::path::PathBuf,
    },

    /// Remove stale empty index registrations (0 chunks)
    ///
    /// Lists every index registered with the daemon, finds those whose
    /// `chunk_count` is 0 (never successfully indexed), and removes them via
    /// `DELETE /indexes/:id`. Interactive by default — pass `--yes` to skip
    /// the confirmation prompt, or `--dry-run` to preview without deleting.
    ///
    /// Examples:
    ///   trusty-search cleanup
    ///   trusty-search cleanup --yes
    ///   trusty-search cleanup --dry-run
    #[command(display_order = 13)]
    Cleanup {
        /// Skip the confirmation prompt and remove empty indexes immediately
        #[arg(short = 'y', long)]
        yes: bool,

        /// Show what would be removed without deleting anything (overrides --yes)
        #[arg(long)]
        dry_run: bool,
    },

    /// Full reindex of current project (see `index --force`)
    ///
    /// Streams progress via SSE and renders a live progress bar. Prefer
    /// `trusty-search index --force` which also handles registration.
    ///
    /// Examples:
    ///   trusty-search reindex
    ///   trusty-search reindex ~/Projects/myapp
    #[command(display_order = 7)]
    Reindex {
        /// Directory to reindex (default: auto-detected project root)
        path: Option<std::path::PathBuf>,

        /// Hard wall-clock cap on the foreground wait (seconds).
        ///
        /// When omitted (the default) the CLI uses progress-aware stall
        /// detection: it keeps displaying progress as long as files are being
        /// indexed and only detaches after no progress for 120 s.  Pass an
        /// explicit value (e.g. `--timeout 1200`) to impose a hard cap instead.
        /// Use `--timeout 0` to wait forever.
        #[arg(long)]
        timeout: Option<u64>,
    },

    // ── Global / multi-index commands ─────────────────────────────────────
    /// List all registered indexes with stats  [alias: ls]
    ///
    /// Shows every project currently known to the daemon — including ones
    /// added manually via `trusty-search index` and ones auto-discovered at
    /// startup from `~/.config/trusty-search/config.yaml` `scan_paths`.
    /// Use `--json` to pipe the result into scripts.
    ///
    /// AGENT USAGE: call this before `search` / `query` to confirm an index
    /// is registered. If a project you expected is missing, run
    /// `trusty-search index` from inside its directory.
    ///
    /// Examples:
    ///   trusty-search list
    ///   trusty-search list --json
    #[command(alias = "ls", display_order = 10)]
    List,

    /// Search across all or named indexes  [alias: q]
    ///
    /// Examples:
    ///   trusty-search query "fn authenticate" --indexes "*"
    ///   trusty-search query "database pool" --indexes proj-a,proj-b
    #[command(alias = "q", display_order = 11)]
    Query {
        /// Search query
        query: String,

        /// Indexes to search: "*" for all, or comma-separated names
        #[arg(long, default_value = "*")]
        indexes: String,

        /// Number of results
        #[arg(short = 'k', long, default_value = "10")]
        top_k: usize,

        /// Show full chunk content
        #[arg(short, long)]
        full: bool,
    },

    /// Check daemon liveness (alias for `status`)
    ///
    /// Kept for backward compatibility. Both `health` and `status` produce
    /// the same rich output: daemon URL, version, and per-index chunk counts.
    ///
    /// Examples:
    ///   trusty-search health
    #[command(display_order = 12)]
    Health,

    // ── Service commands ──────────────────────────────────────────────────
    /// Start the HTTP daemon
    ///
    /// By default, self-spawns a detached background copy of itself (with
    /// `--foreground`) and returns immediately, so the daemon survives the
    /// caller's terminal closing (e.g. tmux pane SIGHUP, `make patch`).
    /// Use `--foreground` when the process is supervised by launchd, systemd,
    /// or Docker — those supervisors require the managed binary to stay in
    /// the foreground rather than forking.
    ///
    /// AUTO-DISCOVERY (issue #40): after the daemon hydrates its registry
    /// from `indexes.toml`, it walks the `scan_paths` declared in
    /// `~/.config/trusty-search/config.yaml` (or, when absent, the default
    /// list `~/Projects`, `~/code`, `~/src`) one level deep and indexes any
    /// directory containing `.claude/`, `CLAUDE.md`, or `.git/` that is not
    /// already registered. The behaviour is best-effort and never blocks
    /// daemon startup — failures are logged at warn level.
    ///
    /// AGENT USAGE: call this once at workstation boot (or rely on the macOS
    /// launchd agent installed via `trusty-search service install`). Every
    /// other CLI subcommand auto-detects the running daemon, so the agent
    /// rarely needs to invoke `start` directly.
    ///
    /// Examples:
    ///   trusty-search start
    ///   trusty-search start --port 7878
    ///   trusty-search start --foreground --port 7878   # launchd / systemd
    ///   trusty-search start --data-dir /tmp/test-daemon  # isolated data dir
    #[command(display_order = 20)]
    Start {
        /// Port to listen on (default: 7878, auto-selects next if busy)
        #[arg(long, default_value_t = trusty_search::service::DEFAULT_PORT)]
        port: u16,

        /// Run in the foreground instead of forking a background daemon.
        ///
        /// Default (`trusty-search start`): self-spawns a detached child with
        /// `--foreground` and returns immediately, so the daemon survives the
        /// caller's terminal closing (e.g. tmux pane SIGHUP). Use this flag
        /// when the process is managed by launchd, systemd, or Docker — those
        /// supervisors require the managed binary to stay in the foreground.
        #[arg(long, default_value_t = false)]
        foreground: bool,

        /// Embedding execution device: `auto` (default), `cpu`, or `gpu`.
        ///
        /// - `auto`: prefer CUDA on Linux/Windows (binary must be built with
        ///   `--features cuda`), then CoreML on Apple Silicon, otherwise CPU.
        /// - `cpu`: force CPU even when a GPU is available — useful for A/B
        ///   benchmarking or freeing the GPU for another workload.
        /// - `gpu`: require GPU acceleration; exit 1 if no GPU EP can be
        ///   initialised. Useful on a dedicated GPU indexing node where
        ///   silent CPU fallback would mean a 10× slower reindex.
        ///
        /// Implemented as the `TRUSTY_DEVICE` env var, which the embedder
        /// reads at session-init time. Set explicitly to override the daemon
        /// default.
        #[arg(long, value_parser = ["auto", "cpu", "gpu"], default_value = "auto")]
        device: String,

        /// Override the data directory used by the daemon (lockfile, port file,
        /// indexes.toml, per-index data).
        ///
        /// Equivalent to setting `TRUSTY_DATA_DIR` in the environment.
        /// `TRUSTY_DATA_DIR` takes precedence over this flag when both are set.
        /// The directory is created automatically if it does not exist.
        ///
        /// Use this to run an isolated daemon (e.g. for cert/benchmark work)
        /// alongside the production daemon without lockfile conflicts:
        ///   trusty-search start --data-dir /tmp/ts-cert --port 7879
        #[arg(long, env = "TRUSTY_DATA_DIR")]
        data_dir: Option<std::path::PathBuf>,

        /// Suppress the auto-discovery scan at startup.
        ///
        /// By default the daemon walks `scan_paths` (from
        /// `~/.config/trusty-search/config.yaml`) after hydrating its registry
        /// from `indexes.toml` and indexes any project not yet registered.
        /// Pass this flag (or set `TRUSTY_NO_AUTO_DISCOVER=1`) to skip that
        /// scan entirely — the daemon will only serve indexes that are already
        /// present in `indexes.toml` or registered manually at runtime.
        ///
        /// Useful when the scan-paths tree is very large, when the daemon is
        /// started in a CI/CD environment that should not discover arbitrary
        /// repositories, or when reproducible startup behaviour is required.
        ///
        /// Precedence: CLI flag > `TRUSTY_NO_AUTO_DISCOVER` env var > default
        /// (auto-discover enabled).
        #[arg(long, env = "TRUSTY_NO_AUTO_DISCOVER")]
        no_auto_discover: bool,
    },

    /// Stop the running background daemon
    ///
    /// Sends SIGTERM to the daemon process and waits for clean shutdown.
    ///
    /// Examples:
    ///   trusty-search stop
    #[command(display_order = 21)]
    Stop,

    /// Start MCP server (stdio by default; add --with-http for an HTTP listener)
    ///
    /// Stdio MCP is always served on the process's stdin/stdout for Claude
    /// Code, which pipes JSON-RPC directly and needs nothing more — so the
    /// HTTP listener is OFF by default (issue #123).
    ///
    /// Pass `--with-http` to additionally bind an HTTP/SSE transport on
    /// 127.0.0.1:<port> (port 0 = OS-chosen free port). The bound address is
    /// written to `~/.trusty-search/mcp_http_addr` so admin clients can
    /// discover it.
    ///
    /// Examples:
    ///   trusty-search serve                       # MCP stdio only (Claude hook)
    ///   trusty-search serve --with-http           # MCP stdio + HTTP on :0
    ///   trusty-search serve --with-http --port 7878  # MCP stdio + HTTP on :7878
    ///   trusty-search serve --http 0.0.0.0:8080   # legacy: explicit bind addr
    ///
    /// `--no-http` is accepted but ignored — HTTP is already off by default.
    #[command(display_order = 22)]
    Serve {
        /// Enable the HTTP/SSE listener in addition to MCP stdio.
        ///
        /// Off by default: Claude Code MCP hooks pipe JSON-RPC over
        /// stdin/stdout and never need the HTTP admin panel, so binding it
        /// would just waste a port. Opt in with this flag when you want the
        /// HTTP transport (e.g. for the browser admin panel).
        #[arg(long, default_value_t = false)]
        with_http: bool,

        /// Deprecated no-op: HTTP is now OFF by default (issue #123), so
        /// `--no-http` does nothing. Kept as a hidden, accepted flag so
        /// existing `.mcp.json` configs that still pass it don't break.
        #[arg(long, hide = true, default_value_t = false)]
        no_http: bool,

        /// Port for the HTTP/SSE MCP transport (default: 0 = OS picks).
        ///
        /// Only used when `--with-http` is set.
        #[arg(long, default_value_t = 0)]
        port: u16,

        /// Legacy: explicit "host:port" bind address. When set, overrides
        /// `--port`. Kept for backward compatibility with older docs.
        #[arg(long)]
        http: Option<String>,
    },

    /// Manage the macOS launchd service (install/uninstall/status/logs)
    ///
    /// Installs a LaunchAgent plist at
    /// `~/Library/LaunchAgents/com.trusty.trusty-search.plist` that runs the
    /// daemon in the foreground under launchd supervision. Not supported on
    /// Linux / Windows — the subcommand exits 1 with a clear message.
    ///
    /// Examples:
    ///   trusty-search service install
    ///   trusty-search service status
    ///   trusty-search service logs
    ///   trusty-search service uninstall
    #[command(display_order = 24)]
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Open the admin panel of the running daemon in the default browser
    ///
    /// Reads `~/.trusty-search/http_addr` to discover the daemon, then opens
    /// `http://<addr>/ui` in the default browser. Falls back to printing the
    /// URL if the browser fails to launch. Errors clearly if no daemon is
    /// running (no discovery file).
    ///
    /// Examples:
    ///   trusty-search dashboard
    ///   trusty-search dash
    ///   trusty-search ui
    #[command(display_order = 23, aliases = ["dash", "ui"])]
    Dashboard,

    /// Migrate mcp-vector-search project(s) to trusty-search
    ///
    /// Reads `.mcp-vector-search/config.json` from each project, derives an
    /// index name from the project root's basename, and POSTs to the daemon
    /// to create + reindex the project.
    ///
    /// Examples:
    ///   trusty-search convert project           # convert current project
    ///   trusty-search convert all               # convert every project on this machine
    ///   trusty-search convert all --dry-run     # preview without changes
    #[command(display_order = 25)]
    Convert {
        /// What to convert: "project" (CWD) or "all" (machine-wide scan)
        #[arg(value_name = "TARGET")]
        target: ConvertTarget,

        /// Show what would be converted without contacting the daemon
        #[arg(long)]
        dry_run: bool,

        /// Maximum concurrent conversions for "all"
        #[arg(long, default_value = "4")]
        concurrency: usize,
    },

    /// Migrate from mcp-vector-search (or other tools) to trusty-search
    ///
    /// Updates Claude MCP configuration files and migrates project indexes.
    ///
    /// Examples:
    ///   trusty-search migrate mcp-vector-search           # migrate both MCP config + indexes
    ///   trusty-search migrate mcp-vector-search --dry-run # preview changes
    ///   trusty-search migrate mcp-vector-search --mcp-only
    ///   trusty-search migrate mcp-vector-search --indexes-only
    #[command(display_order = 26)]
    Migrate {
        /// Migration source: "mcp-vector-search" or "storage"
        #[arg(value_name = "FROM")]
        target: commands::migrate::MigrateTarget,

        /// Preview changes without modifying any files or contacting the daemon
        #[arg(long)]
        dry_run: bool,

        /// Only update Claude MCP config files; skip index migration
        #[arg(long, conflicts_with = "indexes_only")]
        mcp_only: bool,

        /// Only migrate indexes; skip MCP config file updates
        #[arg(long, conflicts_with = "mcp_only")]
        indexes_only: bool,
    },

    /// Move legacy index storage into per-project .trusty-search/ directories (issue #403)
    ///
    /// Relocates each registered index's data files (redb corpus, HNSW snapshot,
    /// schema stamp) from the global `<data-dir>/indexes/<id>/` directory into
    /// `<root_path>/.trusty-search/` inside the project tree. No re-index is needed
    /// — the data is just moved. After running, restart the daemon.
    ///
    /// Indexes whose `root_path` no longer exists on disk are skipped (reported
    /// as "root missing"). Already-colocated indexes are silently skipped.
    ///
    /// Examples:
    ///   trusty-search migrate-storage             # move all legacy indexes
    ///   trusty-search migrate-storage --dry-run   # preview without moving
    #[command(display_order = 27, name = "migrate-storage")]
    MigrateStorage {
        /// Preview changes without moving any files or updating the registry
        #[arg(long)]
        dry_run: bool,
    },

    /// Migrate a redb 2.x index corpus to redb 4.x, preserving data (issue #702/#707)
    ///
    /// Copies an old redb-2.x `index.redb` into a fresh 4.x corpus row-for-row
    /// (chunks, entities, KG, file hashes, schema version) so an upgrade keeps
    /// the index WITHOUT re-embedding. Backs up the original first; re-running
    /// against an already-4.x corpus is a no-op. PATH may be the live
    /// `index.redb` or its `*.v2-incompatible` recovery backup.
    #[command(display_order = 28, name = "migrate-redb")]
    MigrateRedb {
        /// Path to the index.redb (or its .v2-incompatible backup) to migrate
        #[arg(value_name = "PATH")]
        path: std::path::PathBuf,
    },

    /// Remove orphaned index registrations whose root_path no longer exists (issue #489)
    ///
    /// Works OFFLINE — no daemon required. Reads indexes.toml, identifies
    /// entries whose `root_path` does not exist on disk (deleted projects,
    /// wiped volumes, /tmp test indexes), and removes them from the registry
    /// after displaying the list and asking for confirmation.
    ///
    /// Only the registry entry is removed — no index data directories are
    /// deleted. If the project comes back (e.g. volume remounted), you can
    /// re-register with `trusty-search index <path>`.
    ///
    /// Examples:
    ///   trusty-search prune-orphans --dry-run   # preview without any changes
    ///   trusty-search prune-orphans             # interactive confirmation
    ///   trusty-search prune-orphans --yes       # non-interactive (scripted)
    #[command(display_order = 27, name = "prune-orphans")]
    PruneOrphans {
        /// Preview the list of orphaned registrations without removing anything
        #[arg(long)]
        dry_run: bool,

        /// Skip the confirmation prompt and remove immediately
        #[arg(long)]
        yes: bool,
    },

    /// Wire trusty-search into Claude Code's settings.json files
    ///
    /// Scans `$HOME` for every `.claude/settings*.json` and idempotently
    /// registers the trusty-search MCP server in each. Falls back to creating
    /// `~/.claude/settings.json` when no settings files exist. No daemon
    /// required — this command only edits local config files. Safe to run
    /// any number of times.
    ///
    /// Examples:
    ///   trusty-search setup
    #[command(display_order = 28)]
    Setup,

    /// Wire trusty-search into an IDE (Cursor, etc.)
    ///
    /// Writes MCP server config and AI rules files for the target IDE.
    /// No daemon required — this command only writes config files.
    ///
    /// Examples:
    ///   trusty-search integrate cursor                 # global + project MCP config + rules
    ///   trusty-search integrate cursor --dry-run       # preview without writing
    ///   trusty-search integrate cursor --global-only   # only ~/.cursor/mcp.json
    ///   trusty-search integrate cursor --no-rules      # MCP config only, skip rules file
    #[command(display_order = 28)]
    Integrate {
        /// IDE to integrate with: "cursor"
        #[arg(value_name = "IDE")]
        target: commands::integrate::IntegrateTarget,

        /// Preview changes without writing any files
        #[arg(long)]
        dry_run: bool,

        /// Only update the global IDE config (~/.cursor/mcp.json); skip project files
        #[arg(long, conflicts_with = "project_only")]
        global_only: bool,

        /// Only update project-level files (.cursor/mcp.json + rules); skip global config
        #[arg(long, conflicts_with = "global_only")]
        project_only: bool,

        /// Skip writing the .cursor/rules/trusty-search.mdc rules file
        #[arg(long)]
        no_rules: bool,
    },

    /// Diagnose configuration, model cache, and index health
    ///
    /// Checks each component and reports ✓ / ✗ / ⚠ for each. Exit code 0
    /// when all checks pass or only warnings; exit code 1 when any error is
    /// found. Pass --fix to attempt automatic repair of fixable problems.
    ///
    /// Examples:
    ///   trusty-search doctor
    ///   trusty-search doctor --fix
    #[command(display_order = 29)]
    Doctor {
        /// Attempt to fix detected problems automatically
        #[arg(long)]
        fix: bool,
    },

    /// Get or set runtime daemon configuration (memory limits)
    ///
    /// Updates take effect immediately on the running daemon — no restart
    /// required. Use `0`, `off`, `none`, `disable`, or `unlimited` as the
    /// value to remove a limit.
    ///
    /// Examples:
    ///   trusty-search config get
    ///   trusty-search config get memory-limit
    ///   trusty-search config set memory-limit 16384
    ///   trusty-search config set index-memory-limit 65536
    ///   trusty-search config set memory-limit off
    #[command(display_order = 30)]
    Config {
        #[command(subcommand)]
        action: commands::config::ConfigAction,
    },

    /// Monitor the trusty-search daemon via web UI or terminal dashboard
    ///
    /// `monitor web` prints the admin panel URL of the running daemon (and
    /// attempts to open it in the default browser). `monitor tui` launches the
    /// trusty-search-specific ratatui dashboard: an index list, a live
    /// reindex/search activity log, and a query bar.
    ///
    /// Examples:
    ///   trusty-search monitor web
    ///   trusty-search monitor tui
    #[command(display_order = 31, subcommand_required = true)]
    Monitor {
        #[command(subcommand)]
        target: MonitorTarget,
    },

    /// Print the daemon's listening port (or address) to stdout.
    ///
    /// Reads the address the running daemon persisted to its `http_addr`
    /// discovery file. Useful for shell substitution:
    ///   curl http://127.0.0.1:$(trusty-search port)/health
    ///
    /// Exits non-zero (with a message on stderr) when no daemon is running
    /// or the address file is missing, so substitution fails cleanly.
    ///
    /// Examples:
    ///   trusty-search port               # bare port: 7879
    ///   trusty-search port --addr        # host:port: 127.0.0.1:7879
    ///   trusty-search port --json        # {"addr":"127.0.0.1","port":7879}
    #[command(display_order = 33)]
    Port {
        /// Emit full `host:port` instead of the bare port number.
        #[arg(long, conflicts_with = "json")]
        addr: bool,

        /// Emit a JSON object: `{"addr":"…","port":…}`.
        #[arg(long, conflicts_with = "addr")]
        json: bool,
    },

    /// Generate shell completion script
    ///
    /// Examples:
    ///   trusty-search completions zsh > ~/.zsh/completions/_trusty-search
    ///   trusty-search completions bash >> ~/.bashrc
    #[command(display_order = 34)]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Check for or install a new version of trusty-search.
    ///
    /// Why: Gives operators a single command to go from "I wonder if I'm up
    /// to date" through `cargo install` and daemon restart — without having
    /// to remember the exact `cargo install` invocation.
    ///
    /// Without flags: checks crates.io, shows current → available, prompts
    /// for confirmation, then installs + restarts (if newer exists).
    /// With `--check`: report versions only, no install.
    /// With `--yes`: skip the confirmation prompt.
    ///
    /// After a successful install the daemon restarts automatically when
    /// running under launchd (`KeepAlive::OnSuccess`). When not supervised,
    /// a restart hint is printed instead.
    ///
    /// Examples:
    ///   trusty-search upgrade               # interactive
    ///   trusty-search upgrade --check       # report only
    ///   trusty-search upgrade --yes         # non-interactive
    #[command(display_order = 35)]
    Upgrade {
        /// Report current and available versions without installing anything.
        #[arg(long)]
        check: bool,

        /// Skip the confirmation prompt and install immediately.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

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
enum IndexAction {
    /// Remove an index registration (daemon + global config + allowlist)
    ///
    /// Deletes the daemon-side registration matching the given (or
    /// auto-detected) path via `DELETE /indexes/:id`, drops the matching
    /// entry from `~/.config/trusty-search/config.yaml`, and also removes it
    /// from the allowlist (`~/.config/trusty-search/indexes.toml`). The
    /// on-disk redb / HNSW snapshot is preserved — re-registering with the
    /// same path reuses it.
    ///
    /// AGENT USAGE: use this when a project has been moved or deleted so the
    /// daemon stops reporting an empty/stale entry. Auto-detect from CWD when
    /// possible; pass an explicit PATH when running from outside the project.
    ///
    /// Examples:
    ///   trusty-search index remove
    ///   trusty-search index remove ~/Projects/old-app
    Remove {
        /// Directory of the index to remove (default: auto-detected from CWD)
        path: Option<std::path::PathBuf>,
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
    Add {
        /// Directory to approve for indexing
        path: std::path::PathBuf,

        /// Optional human-readable name for the index
        #[arg(short, long)]
        name: Option<String>,
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
}

/// Target surface for the `monitor` subcommand.
///
/// Why: operators want a quick browser link to the daemon's admin panel, the
/// trusty-search-specific terminal dashboard, OR the same dashboard data as
/// plain text / JSON so scripts and CI can read it without a TUI (issues #33,
/// #34).
/// What: `Web` prints (and opens) the daemon's `/ui` URL; `Tui` launches the
/// trusty-search-specific `trusty_common::monitor::search_tui` ratatui
/// dashboard; `Status` and `Indexes` print scriptable health and per-index
/// stats.
/// Test: `cargo run -p trusty-search -- monitor --help` lists every variant.
#[derive(Subcommand)]
enum MonitorTarget {
    /// Open the web dashboard URL in the terminal (or browser)
    Web,
    /// Launch the trusty-search terminal UI: indexes, reindex, and search monitor
    Tui,
    /// Print daemon status: health, version, uptime, and corpus totals
    ///
    /// Examples:
    ///   trusty-search monitor status
    ///   trusty-search monitor status --json
    Status {
        /// Emit the status as a JSON object instead of plain text
        #[arg(long)]
        json: bool,
    },
    /// List every index, or show one index's detail when an ID is given
    ///
    /// Examples:
    ///   trusty-search monitor indexes
    ///   trusty-search monitor indexes my-project
    ///   trusty-search monitor indexes --json
    Indexes {
        /// Optional index ID to show detail for (omit to list all)
        id: Option<String>,
        /// Emit the result as JSON instead of a plain-text table
        #[arg(long)]
        json: bool,
    },
}

/// Why: Allow users to override `QueryClassifier`'s automatic intent detection
/// when they know the intent up-front (e.g. searching for TODO comments).
/// What: Mirrors `crate::core::QueryIntent` for the CLI surface.
/// Test: `cargo run -- search foo --intent conceptual --help` parses without error.
#[derive(Debug, Clone, ValueEnum)]
enum IntentArg {
    Definition,
    Usage,
    Conceptual,
    Bugdebt,
    Unknown,
}

/// Why (issue #1006): raise tokio worker floor to prevent accept-loop starvation.
/// Embed-pool workers block on 30 s sidecar calls; with only num_cpus workers the
/// axum accept loop starves under heavy CoreML/CUDA load → false "daemon down".
/// What: `max(available_parallelism, 16)` workers. Blocking pool unchanged (512).
/// Test: `worker_thread_count_at_least_16` in tests_state.rs.
fn main() {
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let worker_threads = trusty_search::worker_thread_count(cpu_count);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .thread_name("trusty-search-worker")
        .build()
        .expect("failed to build tokio runtime");

    // Central error-printer + exit-code chooser. Why: command handlers are now
    // testable units that return `Result<()>` instead of calling `process::exit`
    // directly (issue #104). Print the chain compactly with the red ✗ prefix
    // operators already recognize, then exit 1.
    if let Err(e) = rt.block_on(run()) {
        let msg = format!("{:#}", e);
        if !msg.is_empty() {
            eprintln!("{} {}", "✗".red(), msg);
        }
        std::process::exit(1);
    }
}

/// Bundled declarative help config (issue #216). Loaded once per process.
///
/// Why: every binary in the workspace embeds its `help.yaml` via
/// `include_str!` so the suggester + render layer stays self-contained — no
/// runtime file I/O, no version skew between a compiled binary and an
/// out-of-tree YAML file.
/// What: `LazyLock<HelpConfig>` parsed from `help.yaml` at first access.
/// `expect` is acceptable here because the YAML is shipped inside the
/// binary; a parse failure is a programmer error caught on first run.
/// Test: `cargo test -p trusty-common --features cli-help` covers the
/// shared loader; CLI wiring is exercised manually via `trusty-search qury`.
static HELP: std::sync::LazyLock<trusty_common::help::HelpConfig> =
    std::sync::LazyLock::new(|| {
        trusty_common::help::load_help(include_str!("../help.yaml"))
            .expect("trusty-search help.yaml is bundled and valid")
    });

async fn run() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();

    // Why: parse via `try_parse` so we can attach the workspace-shared
    // "did you mean?" suggestion to clap's standard error rendering before
    // exiting (issue #216). On success the parse is indistinguishable from
    // the original `Cli::parse()` call.
    let argv: Vec<String> = std::env::args().collect();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Let clap render its own helpful error first so the user sees
            // the unrecognised-token message in the format they already know.
            e.print().ok();
            // Then layer on the workspace-shared "did you mean?" suggestion
            // when the input looks like an unknown subcommand or argument.
            if matches!(
                e.kind(),
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ) {
                trusty_common::help::print_suggestion_hint(&argv, &HELP);
            }
            std::process::exit(e.exit_code());
        }
    };

    // Tracing init + NO_COLOR handling via shared trusty-common helpers.
    //
    // The `start` daemon command initialises tracing itself via
    // `init_tracing_with_buffer` so it can wire the in-memory `LogBuffer`
    // that backs `GET /logs/tail` (issue #35). Installing a plain subscriber
    // here would win the `try_init` race and leave that buffer empty, so we
    // skip the early init for `start` only.
    if !matches!(cli.command, Commands::Start { .. }) {
        trusty_common::init_tracing(if cli.verbose { 2 } else { 0 });
    }
    trusty_common::maybe_disable_color(false);

    // Update check: run for human-facing commands only. MCP `serve` mode must
    // never print to stderr (or stdout) — JSON-RPC framing owns both streams.
    // The `start` daemon path also skips the check because it self-spawns a
    // detached child; the human-facing side returns immediately before the
    // daemon prints its banner, so there is no useful output window.
    // `upgrade` does its own fresh check, so we skip the throttled notice to
    // avoid a redundant second check on the same run.
    // The check is throttled to once per 24 h via an on-disk cache, so on typical
    // runs this is a sub-millisecond cache read with zero network I/O.
    let is_mcp_serve = matches!(cli.command, Commands::Serve { .. });
    let is_daemon_start = matches!(cli.command, Commands::Start { .. });
    let is_upgrade = matches!(cli.command, Commands::Upgrade { .. });
    if !is_mcp_serve && !is_daemon_start && !is_upgrade {
        if let Some(info) = trusty_common::update::check_throttled(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        )
        .await
        {
            eprintln!("{}", trusty_common::update::notice(&info));
        }
    }

    match cli.command {
        Commands::Search {
            query,
            top_k,
            full,
            intent: _,
            no_kg: _,
            offset: _,
            budget: _,
        } => {
            commands::search::handle_search(&cli.index, cli.json, query, top_k, full).await?;
        }

        Commands::Watch { path } => {
            commands::watch::handle_watch(&cli.index, path).await?;
        }

        Commands::Status => {
            commands::status::handle_status(cli.json).await?;
        }

        Commands::IndexStatus { index_id, watch } => {
            commands::index_status::handle_index_status(index_id.as_deref(), watch, cli.json)
                .await?;
        }

        Commands::Init {
            path,
            name,
            exclude,
        } => {
            commands::init::handle_init(path, name, exclude).await?;
        }

        Commands::Index {
            path,
            name,
            force,
            exclude,
            timeout,
            lexical_only,
            no_kg,
            action,
        } => match action {
            Some(IndexAction::Remove { path: rm_path }) => {
                commands::index_remove::handle_index_remove(rm_path).await?;
            }
            Some(IndexAction::Add {
                path: add_path,
                name: add_name,
            }) => {
                commands::index_allowlist::handle_allowlist_add(add_path, add_name).await?;
            }
            Some(IndexAction::List { json }) => {
                commands::index_allowlist::handle_allowlist_list(json).await?;
            }
            None => {
                commands::index::handle_index(
                    path,
                    name,
                    force,
                    exclude,
                    timeout,
                    lexical_only,
                    no_kg,
                )
                .await?;
            }
        },

        Commands::Add { file } => {
            commands::add::handle_add(&cli.index, file).await?;
        }

        Commands::Remove { file } => {
            commands::remove::handle_remove(&cli.index, file).await?;
        }

        Commands::Cleanup { yes, dry_run } => {
            commands::cleanup::handle_cleanup(yes, dry_run).await?;
        }

        Commands::Reindex { path, timeout } => {
            commands::reindex::handle_reindex(&cli.index, path, timeout).await?;
        }

        Commands::List => {
            commands::list::handle_list(cli.json).await?;
        }

        Commands::Query {
            query,
            indexes,
            top_k,
            full,
        } => {
            commands::query::handle_query(&cli.index, cli.json, query, indexes, top_k, full)
                .await?;
        }

        // `health` is an alias registered on the `status` subcommand, so
        // this arm catches the bare `Commands::Health` variant which is kept
        // for backward-compat with any scripts that invoke it directly.
        Commands::Health => {
            commands::status::handle_status(cli.json).await?;
        }

        Commands::Start {
            port,
            foreground,
            device,
            data_dir,
            no_auto_discover,
        } => {
            commands::start::handle_start(
                port,
                foreground,
                &device,
                data_dir.as_deref(),
                cli.verbose,
                no_auto_discover,
            )
            .await?;
        }

        Commands::Stop => {
            commands::stop::handle_stop().await?;
        }

        Commands::Serve {
            with_http,
            no_http: _, // deprecated no-op (issue #123): HTTP is opt-in now
            port,
            http,
        } => {
            commands::serve::handle_serve(with_http, port, http).await?;
        }

        Commands::Service { action } => {
            commands::service::handle_service(&action)?;
        }

        Commands::Dashboard => {
            commands::dashboard::handle_dashboard().await?;
        }

        Commands::Convert {
            target,
            dry_run,
            concurrency,
        } => {
            commands::convert::handle_convert(target, dry_run, concurrency).await?;
        }

        Commands::Migrate {
            target,
            dry_run,
            mcp_only,
            indexes_only,
        } => {
            commands::migrate::handle_migrate(target, dry_run, mcp_only, indexes_only).await?;
        }

        Commands::MigrateStorage { dry_run } => {
            commands::migrate_storage::handle_migrate_storage(dry_run)?;
        }

        Commands::MigrateRedb { path } => {
            commands::migrate_redb::handle_migrate_redb(path)?;
        }

        Commands::PruneOrphans { dry_run, yes } => {
            commands::prune_orphans::handle_prune_orphans(dry_run, yes)?;
        }

        Commands::Setup => {
            commands::setup::handle_setup()?;
        }

        Commands::Integrate {
            target,
            dry_run,
            global_only,
            project_only,
            no_rules,
        } => {
            commands::integrate::handle_integrate(
                target,
                dry_run,
                global_only,
                project_only,
                no_rules,
            )
            .await?;
        }

        Commands::Doctor { fix } => {
            commands::doctor::handle_doctor(fix).await?;
        }

        Commands::Config { action } => {
            commands::config::handle_config(action).await?;
        }

        Commands::Monitor { target } => match target {
            MonitorTarget::Web => {
                // Prefer the live address written by a running daemon; fall
                // back to the default loopback port so the command still
                // prints something useful when the daemon has not started.
                let url = match trusty_common::read_daemon_addr("trusty-search") {
                    Ok(Some(addr)) => format!("{addr}/ui"),
                    _ => format!(
                        "http://127.0.0.1:{}/ui",
                        trusty_search::service::DEFAULT_PORT
                    ),
                };
                println!("{url}");
                open::that(&url).ok();
            }
            MonitorTarget::Tui => {
                trusty_common::monitor::search_tui::run().await?;
            }
            MonitorTarget::Status { json } => {
                commands::monitor::handle_status(json).await?;
            }
            MonitorTarget::Indexes { id, json } => {
                commands::monitor::handle_indexes(id, json).await?;
            }
        },

        Commands::Port { addr, json } => {
            let format = if json {
                commands::port::PortFormat::Json
            } else if addr {
                commands::port::PortFormat::Addr
            } else {
                commands::port::PortFormat::Port
            };
            commands::port::handle_port(format)?;
        }

        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut io::stdout());
        }

        Commands::Upgrade { check, yes } => {
            commands::upgrade::handle_upgrade(check, yes).await?;
        }
    }

    Ok(())
}
