//! CLI entry point for the `trusty-memory` binary.
//!
//! Why: ship a thin clap-to-handler shim so users can `cargo install
//! trusty-memory` and invoke `trusty-memory serve` (MCP stdio — bare `serve`
//! and `serve --stdio` are the same thing since #5267), `trusty-memory start`
//! (the HTTP/SSE daemon), or `trusty-memory migrate kuzu-memory`
//! (which rewrites Claude settings files that still reference the legacy
//! kuzu-memory MCP server). All real logic lives in the library and the
//! `commands::` modules — this file does CLI parsing and dispatch only.
//! The former `trusty-memory-mcp-bridge` binary and UDS transport were
//! removed in PR3 of the #914 epic; `serve --stdio` is the canonical
//! stdio integration.
//! What: defines a `clap::Parser` with `serve`, `migrate`, and other
//! subcommands. `serve` and `serve --stdio` defer to
//! `commands::serve_stdio_bridge`; `serve --http` / `--foreground` defer to
//! `trusty_memory::run_http` / `run_http_dynamic`.
//! Test: `cargo run -p trusty-memory -- --help` lists all subcommands.
//! `cargo run -p trusty-memory -- migrate kuzu-memory --dry-run` exercises
//! the migrate path end-to-end without modifying any files.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use anyhow::Result;
use clap::{Parser, Subcommand};
// #6652: `PalaceAction` lives beside its handlers in the library so this file
// stays under the 500-SLOC production cap.
use std::net::SocketAddr;
use trusty_memory::commands::inbox_check::handle_inbox_check;
use trusty_memory::commands::link::handle_link;
use trusty_memory::commands::migrate::{handle_migrate, MigrateTarget};
use trusty_memory::commands::note::handle_note;
use trusty_memory::commands::palace::PalaceAction;
use trusty_memory::commands::prompt_context::run_prompt_context_and_exit;
use trusty_memory::commands::send_message::handle_send_message;
use trusty_memory::commands::service::{handle_service, ServiceAction};
use trusty_memory::commands::upgrade::handle_upgrade;
use trusty_memory::commands::{
    setup::{handle_setup, handle_setup_gui_client},
    start::handle_start,
    stop::handle_stop,
};
use trusty_memory::{resolve_palace_registry_dir, serve, AppState};

/// Top-level CLI for `trusty-memory`.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-memory",
    version,
    about = "Memory palace MCP server + migration utility",
    long_about = "MCP server (stdio + HTTP/SSE) for trusty-memory, plus a \
                  `migrate kuzu-memory` subcommand that rewrites Claude \
                  settings files referencing the legacy kuzu-memory server."
)]
struct Cli {
    /// Increase tracing verbosity (`-v` = debug, `-vv` = trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

/// Top-level subcommands.
///
/// Why: keep the surface small and mirror the `trusty-search` pattern so
/// users moving between the two tools have a consistent experience.
/// What: `serve` runs the MCP server; `migrate` rewrites Claude settings.
/// Test: clap's `--help` output enumerates both.
#[derive(Debug, Subcommand)]
enum Command {
    /// Start the HTTP daemon in the background and return control to the shell.
    ///
    /// Why: matches `trusty-search start` so the trusty-* daemons share a
    /// `start` / `serve` / `stop` surface. The detached child runs
    /// `serve --foreground` so it does not respawn recursively.
    Start,

    /// Stop every running trusty-memory daemon process.
    ///
    /// Why: with `start` now self-spawning a detached daemon, operators need a
    /// way to take it down that does not depend on launchd / systemd.
    Stop,

    /// Run the server.  Mode matrix (#5267; supersedes #914 PR4):
    ///   serve                  → MCP stdio (same as `--stdio`)
    ///   serve --stdio          → MCP stdio JSON-RPC server (Claude Code)
    ///   serve --http[=ADDR]    → HTTP daemon; optional bind address
    ///   serve --foreground     → HTTP in foreground (launchd / systemd)
    ///
    /// Bare `serve` speaks MCP over stdio, matching `trusty-search serve`. Use
    /// `trusty-memory start` for the background HTTP daemon — that is the
    /// daemon-launching verb. Before #5267 bare `serve` detached an HTTP daemon,
    /// which made the same word mean opposite things in two sibling crates.
    ///
    /// The stdio path is a pure proxy to the HTTP daemon and will START that
    /// daemon if it is not already running, under an exclusive lock so N
    /// concurrent bridges produce exactly one daemon (#5267, #1152).
    ///
    /// `--http` selects the HTTP/SSE daemon with dynamic port selection
    /// (7070..=7079, OS fallback); without `--foreground` it self-spawns a
    /// detached background daemon and returns immediately. Pass `--foreground`
    /// to keep it in the foreground (used by `start`, launchd, and systemd).
    Serve {
        /// Accepted and IGNORED since #6286 — the daemon binds a Unix socket.
        ///
        /// Why it is still parsed: a launchd plist installed before ADR-0032
        /// passes `--http`, and under `KeepAlive` a clap usage error would
        /// crash-loop the daemon instead of starting it. The flag now means
        /// only "run the daemon rather than MCP stdio", and any address given
        /// is discarded with a warning.
        #[arg(
            long,
            value_name = "ADDR",
            num_args = 0..=1,
            require_equals = false,
            conflicts_with = "stdio"
        )]
        http: Option<Option<SocketAddr>>,

        /// Run the daemon in the foreground (do not self-spawn).
        ///
        /// Why: `serve` defaults to background mode so the trusty-* daemons
        /// share a `start` / `serve` UX. Long-running supervisors (launchd,
        /// systemd, Docker) need a foreground process to manage, so they
        /// pass `--foreground` to opt out of the spawn.
        #[arg(long, conflicts_with = "stdio")]
        foreground: bool,

        /// Run a direct stdio JSON-RPC MCP server (issue #914).
        ///
        /// Why: reinstates `serve --stdio` as a safe, deadlock-free code
        /// path. When set this process binds nothing and forwards to the
        /// daemon's socket — stdout is the exclusive JSON-RPC channel. All
        /// non-protocol output (update checks, banners) is suppressed.
        /// Every request resolves within a deadline so the MCP client
        /// never hangs.
        #[arg(long)]
        stdio: bool,

        /// Bind every MCP tool call to this palace when the caller omits the
        /// `palace` argument.
        #[arg(long, value_name = "NAME")]
        palace: Option<String>,
    },

    /// Migrate from another memory MCP server to trusty-memory.
    ///
    /// For `kuzu-memory`: rewrites Claude `mcpServers` config entries.
    /// For `kuzu-data`: imports entity/relation data from a kuzu-memory
    /// `store.redb` file into a trusty-memory palace (requires `--from`
    /// and `--palace`).
    Migrate {
        /// What to migrate from.
        #[arg(value_enum)]
        target: MigrateTarget,

        /// Print what would change without writing any files.
        #[arg(long)]
        dry_run: bool,

        /// Accepted for parity with `trusty-search migrate`. Today the
        /// migration only has a config phase, so this flag is a no-op.
        #[arg(long)]
        config_only: bool,

        /// Path to the kuzu-memory `store.redb` file (required for
        /// `kuzu-data`).
        #[arg(long, value_name = "PATH")]
        from: Option<std::path::PathBuf>,

        /// Target palace name to import into (required for `kuzu-data`).
        /// The palace is created if it does not already exist.
        #[arg(long, value_name = "NAME")]
        palace: Option<String>,

        /// Maximum number of entities to import (default: import all).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },

    /// First-time setup: data dir + launchd (macOS) + Claude settings patch.
    Setup {
        /// Emit the MCP registration for a GUI client instead (e.g. `chatgpt`).
        ///
        /// #6307: a GUI client launched by launchd sees only
        /// `PATH=/usr/bin:/bin:/usr/sbin:/sbin` and cannot resolve a bare
        /// `trusty-memory`, so its entry needs this binary's absolute path and
        /// a working directory that exists.
        #[arg(long, value_name = "NAME")]
        client: Option<String>,
    },

    /// Print the daemon's prompt-context block to stdout (Claude Code hook).
    ///
    /// Why: installed as a Claude Code `UserPromptSubmit` hook by
    /// `trusty-memory setup`. Claude Code injects whatever the hook writes to
    /// stdout as additional context for the next prompt, so this command
    /// fetches the daemon's pre-formatted prompt-context block and prints it
    /// verbatim. Every failure path exits 0 silently so the hook can never
    /// block a Claude Code prompt, and every blocking/network step it runs
    /// is deadline-bounded (issue #2043) so it can never hang either.
    /// Note: unlike `trusty-mpm hook`, this command is **not** gated on
    /// `CLAUDE_MPM_SUB_AGENT` — sub-agents benefit from the parent palace's
    /// prompt-fact block just as much as the PM does. See the module-level
    /// doc on `commands::prompt_context` for the full rationale; this
    /// comment previously claimed a sub-agent short-circuit that was never
    /// implemented here (issue #2043 cleanup).
    /// What: see `commands::prompt_context::handle_prompt_context`.
    /// Test: covered by the unit tests in that module plus the integration
    /// path `cargo run -p trusty-memory -- prompt-context` against a live
    /// daemon.
    #[command(name = "prompt-context")]
    PromptContext,

    /// Diagnose daemon health: fastembed cache, launchd plist, HTTP /health,
    /// and stale palace locks. With `--fix-palaces`, audit existing palaces
    /// for project-mapping compliance (issue #88).
    ///
    /// Why: GH #62 / #88 — silent failures (missing `FASTEMBED_CACHE_PATH` in
    /// the plist, missing model cache, daemon not bound) currently force users
    /// to grep through several directories by hand. `doctor` runs the
    /// equivalent checks in one shot. `--fix-palaces` layers in the palace =
    /// project audit so users can see which palaces are orphaned (no matching
    /// project directory on disk).
    /// What: a one-shot CLI command that prints a ✅/❌ line per check and
    /// exits non-zero on any failure. See `commands::doctor`.
    /// Test: `cargo run -p trusty-memory -- doctor` after `setup`.
    ///       `cargo run -p trusty-memory -- doctor --fix-palaces` for the
    ///       palace audit (read-only by default; add `--fix` to suggest renames).
    Doctor {
        /// Audit existing palaces and report orphaned ones (palaces whose name
        /// does not match any detectable project directory on disk).
        ///
        /// Why: issue #88 — users accumulate palaces across many projects;
        /// `--fix-palaces` surfaces which names are orphaned so they can be
        /// cleaned up manually or via `--fix`.
        #[arg(long)]
        fix_palaces: bool,

        /// Print rename suggestions for orphaned palaces (dry-run by default).
        ///
        /// Why: issue #88 conservative default — users may have data in
        /// orphaned palaces; we never auto-rename without confirmation. `--fix`
        /// prints the "would rename X → Y" suggestions that can then be
        /// executed manually.
        #[arg(long, requires = "fix_palaces")]
        fix: bool,
    },

    /// Manage the macOS launchd LaunchAgent for the daemon.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Monitor the trusty-memory daemon via web UI or terminal dashboard.
    ///
    /// `monitor web` prints the daemon's admin-panel URL; `monitor tui`
    /// launches the trusty-memory-specific ratatui dashboard: a palace list,
    /// a live dream/recall activity log, and a recall query bar.
    #[command(subcommand_required = true)]
    Monitor {
        #[command(subcommand)]
        target: MonitorTarget,
    },

    /// Send an inter-project message to another palace (issue #99).
    ///
    /// Why: replaces the Python `/mpm-message` skill with a trusty-memory
    /// native primitive. Writes a tagged drawer into the recipient palace;
    /// the recipient's SessionStart hook picks it up via `inbox-check`.
    ///
    /// Example: `trusty-memory send-message --to claude-mpm --purpose task \
    ///           --content "Please refresh the messaging.db schema"`.
    #[command(name = "send-message")]
    SendMessage {
        /// Recipient palace id (repo slug). Required.
        #[arg(long, value_name = "PALACE")]
        to: String,

        /// Free-text purpose / category (e.g. `task`, `notify`, `reply`).
        #[arg(long, value_name = "PURPOSE")]
        purpose: String,

        /// Message body. Plain text; rendered into the recipient session as
        /// a Markdown block.
        #[arg(long, value_name = "TEXT")]
        content: String,

        /// Sender palace id (defaults to the cwd-derived slug).
        #[arg(long, value_name = "PALACE")]
        from: Option<String>,
    },

    /// Pick up unread inter-project messages for the calling project
    /// (issue #99).
    ///
    /// Why: installed as a Claude Code `SessionStart` hook by
    /// `trusty-memory setup`. Reads the receiver palace's unread messages,
    /// prints them as Markdown to stdout (Claude Code injects stdout as
    /// session context), and marks them read via the daemon's HTTP API.
    /// Every failure path degrades to silence so a slow daemon never blocks
    /// session start.
    ///
    /// `--palace` overrides the cwd-derived slug; useful for test rigs and
    /// for projects whose repo basename does not match their preferred
    /// palace name.
    #[command(name = "inbox-check")]
    InboxCheck {
        /// Receiver palace id (defaults to cwd-derived repo slug).
        #[arg(long, value_name = "PALACE")]
        palace: Option<String>,
    },

    /// Fire-and-forget save of a memory note to the running daemon.
    ///
    /// Why: sub-agents spawned via Claude Code's Agent tool do not inherit
    /// any MCP connections, so the `mcp__trusty-memory__memory_remember`
    /// tool is unreachable to them. They can still execute shell commands,
    /// so this subcommand POSTs to `POST /api/v1/remember` and returns
    /// immediately — the daemon dispatches `memory_remember` on a detached
    /// task. Errors degrade to stderr warnings + zero exit because the
    /// agent has already left the room by the time the write completes.
    ///
    /// Example: `trusty-memory note "User prefers tabs" --palace my-project \
    ///           --tag style --tag preferences`.
    Note {
        /// Drawer body. Required.
        #[arg(value_name = "CONTENT")]
        content: String,

        /// Target palace (defaults to the daemon's `--palace` default when
        /// omitted; required when the daemon was started without one).
        #[arg(long, value_name = "NAME")]
        palace: Option<String>,

        /// Tag to attach to the drawer. Repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },

    /// Re-run auto-KG extraction across every drawer in a palace.
    ///
    /// Why: Issue #97 — `memory_remember` now extracts triples on write,
    /// but existing palaces sit at zero auto-extracted triples until
    /// back-filled. `kg-rebuild` walks every drawer and re-asserts the
    /// heuristic triples so the visual graph view is immediately useful.
    /// What: Loads palaces from disk, processes each palace (or just one
    /// when `--palace` is supplied), and prints a per-palace summary plus
    /// an aggregate total. Failures on individual asserts are logged but
    /// never abort the run.
    /// Test: `commands::kg_rebuild::tests::kg_rebuild_processes_all_drawers`.
    // #5401: --dry-run previews whichever maintenance pass was asked for, so it
    // requires the GROUP rather than one named flag.
    #[command(
        name = "kg-rebuild",
        group = clap::ArgGroup::new("kg_maintenance")
            .args(["purge_stale_subjects", "merge_punctuated_twins"])
            .multiple(true)
    )]
    KgRebuild {
        /// Restrict the rebuild to a single palace id. When omitted, every
        /// palace under the data root is processed.
        #[arg(long, value_name = "ID")]
        palace: Option<String>,

        /// Also DELETE auto-extracted subjects the #4678 token filter now
        /// rejects (pronouns, prepositions, one- and two-character tokens).
        ///
        /// Destructive and off by default. The forward filter only stops new
        /// garbage; a plain rebuild re-asserts and never retracts, so triples
        /// already in the graph need this pass to leave. Every subject is
        /// printed as it goes. Pair with --dry-run to see the list first.
        #[arg(long = "purge-stale-subjects")]
        purge_stale_subjects: bool,

        /// Also MERGE each auto-extracted triple off a punctuated entity node
        /// (`` `redb` ``) onto its cleaned twin (`redb`), in both the subject
        /// and the object position.
        ///
        /// Destructive and off by default. #4678's trim fixed extraction going
        /// forward and left every pre-fix entity split across two nodes; a
        /// plain rebuild only widens the split. Each move is printed. Pair with
        /// --dry-run to see the list first.
        #[arg(long = "merge-punctuated-twins")]
        merge_punctuated_twins: bool,

        /// Report what the selected maintenance pass would do and write nothing
        /// at all — the re-assert pass is skipped too.
        #[arg(long = "dry-run", requires = "kg_maintenance")]
        dry_run: bool,
    },

    /// Inspect or apply the ADR-0027 room registry back-fill.
    ///
    /// The back-fill runs automatically the first time a palace is opened, so
    /// this command exists to audit it against a live palace BEFORE it writes:
    /// `--dry-run` prints the label each room would be given, by which
    /// confidence step, and how many drawers sit behind it, then exits without
    /// writing. `--apply` is required to write. Nothing here ever moves,
    /// reassigns, or rewrites a drawer.
    ///
    ///   trusty-memory rooms backfill --dry-run
    ///   trusty-memory rooms backfill --palace trusty-tools --apply
    #[command(subcommand_required = true)]
    Rooms {
        #[command(subcommand)]
        action: RoomsAction,
    },

    /// Inspect and reclaim a palace's knowledge-graph store, `kg.redb` (#6652).
    ///
    /// redb never returns freed pages to the filesystem — a retracted fact, a
    /// forgotten drawer and a dropped table all release pages into redb's own
    /// free list — so a palace's `kg.redb` only ever grows. `stats` reports
    /// what is actually in the file; `compact` rewrites it into a fresh file
    /// and renames that into place, dropping stale history rows on the way.
    ///
    ///   trusty-memory palace stats trusty-tools
    ///   trusty-memory palace compact trusty-tools --dry-run
    #[command(subcommand_required = true)]
    Palace {
        #[command(subcommand)]
        action: PalaceAction,
    },

    /// Rank existing drawers by how often they are actually injected, so a
    /// human can decide which deserve an `expires_at` (ADR-0028, Migration).
    ///
    /// READ-ONLY. This never writes a drawer, never sets `expires_at`, and
    /// never retires anything — ADR-0028 makes backfill human-gated, and a
    /// tool that applied its own recommendations would violate that outright.
    ///
    /// ADR-0028 does NOT migrate the drawers already on disk. They keep
    /// competing in L1 exactly as they do today, so this report is the only
    /// path by which the estate's existing problem drawers get addressed —
    /// and only when a human acts on a row.
    ///
    /// Ranking is by injection frequency because that is the cost the ADR
    /// cares about: a stale drawer nobody retrieves is free, while the
    /// motivating case is a 19-day-old session checkpoint reaching 44.8% of
    /// turns. Counts are measured from the enriched-prompt hook logs; with no
    /// logs present every count is 0, which the report says out loud rather
    /// than reporting as "nothing is stale".
    ///
    /// No tier is suggested. ADR-0028 §C4 measured why: `resume-target` splits
    /// 71/26 across tiers, so a tag-derived verdict would be wrong for a
    /// quarter of rows while looking as confident as the rest. The evidence is
    /// listed; the call is yours.
    ///
    ///   trusty-memory backfill-report
    ///   trusty-memory backfill-report --palace trusty-tools --min-injections 50
    ///   trusty-memory backfill-report --json --limit 200
    BackfillReport {
        /// Restrict the report to one palace slug.
        #[arg(long, value_name = "ID")]
        palace: Option<String>,

        /// Maximum drawers to print (default 25).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,

        /// Hide drawers injected fewer than N times.
        #[arg(long, value_name = "N", default_value_t = 1)]
        min_injections: u64,

        /// Emit JSON instead of the human-readable stanzas.
        #[arg(long)]
        json: bool,

        /// Override the hook-log directory (default `<data_root>/logs`).
        #[arg(long, value_name = "DIR")]
        logs_dir: Option<std::path::PathBuf>,
    },

    /// Pin this project's palace slug in `.trusty-tools/trusty-memory.yaml`.
    ///
    /// Why: the lazy write in normal memory operations locks in the slug the
    /// first time a memory is saved. `link` lets you do this explicitly
    /// *before* a directory rename or drive reorg, so the slug is already
    /// committed and the palace linkage never breaks.
    ///
    /// The generated file should be committed to version control; it travels
    /// with the repository regardless of where the directory lives on disk.
    ///
    /// Examples:
    ///   trusty-memory link                        # pin CWD's project
    ///   trusty-memory link --path ~/projects/foo  # pin a specific project
    ///   trusty-memory link --slug custom-slug     # override the derived slug
    ///   trusty-memory link --force                # overwrite existing pin
    Link {
        /// Project directory to pin (default: current directory). The
        /// command walks upward from here to find the project root.
        #[arg(long, value_name = "DIR")]
        path: Option<std::path::PathBuf>,

        /// Override the derived palace slug. When omitted the slug is
        /// derived from the project root's directory basename.
        #[arg(long, value_name = "SLUG")]
        slug: Option<String>,

        /// Optional human note to embed in the pin file
        /// (e.g. "pinned before GDrive reorganisation 2026-06").
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,

        /// Overwrite an existing pin file even when the slug differs.
        /// Without this flag, `link` refuses to overwrite an existing pin
        /// with a different slug to prevent accidental data loss.
        #[arg(long)]
        force: bool,
    },

    /// Print the daemon's listening port (or address) to stdout.
    ///
    /// Reads the address the running daemon persisted to its `http_addr`
    /// discovery file. Useful for shell substitution:
    ///   curl http://127.0.0.1:$(trusty-memory port)/health
    ///
    /// Exits non-zero (with a message on stderr) when no daemon is running
    /// or the address file is missing, so substitution fails cleanly.
    ///
    /// Examples:
    ///   trusty-memory port               # bare port: 7070
    ///   trusty-memory port --addr        # host:port: 127.0.0.1:7070
    ///   trusty-memory port --json        # {"addr":"127.0.0.1","port":7070}
    Port {
        /// Emit full `host:port` instead of the bare port number.
        #[arg(long, conflicts_with = "json")]
        addr: bool,

        /// Emit a JSON object: `{"addr":"…","port":…}`.
        #[arg(long, conflicts_with = "addr")]
        json: bool,
    },

    /// Check for or install a new version of trusty-memory.
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
    ///   trusty-memory upgrade               # interactive
    ///   trusty-memory upgrade --check       # report only
    ///   trusty-memory upgrade --yes         # non-interactive
    Upgrade {
        /// Report current and available versions without installing anything.
        #[arg(long)]
        check: bool,

        /// Skip the confirmation prompt and install immediately.
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405).
    Config(trusty_common::inference::config::ConfigCommand),
}

/// Actions under `trusty-memory rooms`.
#[derive(Debug, Subcommand)]
enum RoomsAction {
    /// Register a row for every room the palace's drawers already sit in.
    Backfill {
        /// Restrict to a single palace id (default: every palace on disk).
        #[arg(long, value_name = "ID")]
        palace: Option<String>,

        /// Print the plan and exit without writing. This is the default.
        #[arg(long)]
        dry_run: bool,

        /// Write the plan. Required — the command never writes without it.
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
    },
}

/// Target surface for the `monitor` subcommand.
///
/// Why: operators want a quick link to the daemon's web UI, the
/// memory-specific terminal UI, OR the same dashboard data as plain text /
/// JSON so scripts and CI can read it without a TUI (issues #33, #34).
/// What: `Web` prints the daemon's `/ui` URL; `Tui` launches the
/// trusty-memory-specific `trusty_common::monitor::memory_tui` dashboard;
/// `Status` and `Palaces` print scriptable health and per-palace stats.
/// Test: `cargo run -p trusty-memory -- monitor --help` lists every variant.
#[derive(Debug, Subcommand)]
enum MonitorTarget {
    /// Open the web dashboard URL in the terminal (or browser).
    Web,
    /// Launch the trusty-memory terminal UI: palaces, recall, and dream monitor.
    Tui,
    /// Print daemon status: version and aggregate palace/drawer/vector counts.
    ///
    /// Examples:
    ///   trusty-memory monitor status
    ///   trusty-memory monitor status --json
    Status {
        /// Emit the status as a JSON object instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// List every palace, or show one palace's detail when an ID is given.
    ///
    /// Examples:
    ///   trusty-memory monitor palaces
    ///   trusty-memory monitor palaces default
    ///   trusty-memory monitor palaces --json
    Palaces {
        /// Optional palace ID to show detail for (omit to list all).
        id: Option<String>,
        /// Emit the result as JSON instead of a plain-text table.
        #[arg(long)]
        json: bool,
    },
}

/// Bundled declarative help config (issue #216). Loaded once per process.
///
/// Why: every binary in the workspace embeds its `help.yaml` via
/// `include_str!` so the workspace-shared `trusty_common::help::suggest`
/// helper has a config to consult when the user types an unknown subcommand.
/// What: `LazyLock<HelpConfig>` parsed from `help.yaml` at first access.
/// Test: parse coverage lives in `trusty-common`; this site is exercised
/// manually via `trusty-memory dotor`.
static HELP: std::sync::LazyLock<trusty_common::help::HelpConfig> =
    std::sync::LazyLock::new(|| {
        trusty_common::help::load_help(include_str!("../help.yaml"))
            .expect("trusty-memory help.yaml is bundled and valid") // Why: include_str! guarantees presence at compile time; parse is validated in tests
    });

#[tokio::main]
async fn main() -> Result<()> {
    // #4764: panic payloads reach the log stream via the hook that
    // `trusty_common::init_tracing*` installs — see `trusty_common::panic_hook`.
    // Why: parse via `try_parse` so we can attach the workspace-shared
    // "did you mean?" suggestion to clap's standard error rendering before
    // exiting (issue #216).
    let argv: Vec<String> = std::env::args().collect();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            e.print().ok();
            if matches!(
                e.kind(),
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ) {
                trusty_common::help::print_suggestion_hint(&argv, &HELP);
            }
            std::process::exit(e.exit_code());
        }
    };
    // Issue #35: initialise tracing with an in-memory `LogBuffer` so the HTTP
    // daemon's `GET /api/v1/logs/tail` endpoint can serve recent logs. The
    // buffer-backed subscriber still writes the standard `fmt` layer to
    // stderr, so non-HTTP subcommands (and the MCP stdio path, which must
    // keep stdout clean) are unaffected. The buffer is only wired into the
    // `AppState` on the HTTP serve path.
    //
    // Bug-reporting #478 (Phase 1 wire-up): compose the bug-capture layer in
    // the same registry so all three layers are installed in one `try_init`.
    // The `ErrorStore` is forwarded to `run_serve` which stashes it in
    // `AppState` so Phase 2 can expose it via HTTP / MCP tools.
    let (log_buffer, error_store) = trusty_common::init_tracing_with_buffer_and_capture(
        cli.verbose,
        trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
        "trusty-memory",
        env!("CARGO_PKG_VERSION"),
    );

    // Update check: emitted only for human-facing subcommands. `serve`
    // (foreground or stdio) is the long-running HTTP/MCP daemon — stdout/stderr
    // are owned by the supervisor or the JSON-RPC framing, so we must not print
    // anything there. `start` self-spawns a detached `serve --foreground` child
    // and exits immediately; the very brief window makes the notice useless.
    // `upgrade` does its own fresh check, so we skip the throttled notice to
    // avoid a redundant second check on the same run. `config` (#2405, LOW fix
    // from PR #2528 review) is also excluded — the universal credential CLI
    // must be genuinely offline, so `config keys list` never triggers a
    // network update-check call.
    // The check is throttled to once per 24 h (on-disk cache), so on a
    // typical run this is a sub-millisecond cache-hit with no network I/O.
    let is_daemon_path = matches!(cli.command, Command::Serve { .. } | Command::Start);
    let is_upgrade = matches!(cli.command, Command::Upgrade { .. });
    let is_config = matches!(cli.command, Command::Config(_));
    if !is_daemon_path && !is_upgrade && !is_config {
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
        Command::Start => handle_start().await,
        Command::Stop => handle_stop().await,
        Command::Serve {
            http,
            foreground,
            stdio,
            palace,
        } => match serve_mode(&http, foreground, stdio) {
            ServeMode::Stdio { notify } => {
                if notify {
                    warn_bare_serve_is_stdio();
                }
                run_serve_stdio(palace).await
            }
            // Flatten Option<Option<SocketAddr>> → Option<SocketAddr>.
            // --http (bare) → Some(None) → flatten → None → dynamic port.
            // --http ADDR   → Some(Some(addr)) → flatten → Some(addr).
            ServeMode::Daemon => {
                run_serve(http.flatten(), foreground, palace, log_buffer, error_store).await
            }
        },
        Command::Migrate {
            target,
            dry_run,
            config_only,
            from,
            palace,
            limit,
        } => handle_migrate(target, dry_run, config_only, from, palace, limit),
        // #6307: `--client <name>` emits a GUI client's registration instead of
        // running the local install phases, which a GUI client does not need.
        Command::Setup { client } => match client {
            Some(name) => handle_setup_gui_client(&name),
            None => handle_setup(),
        },
        Command::PromptContext => run_prompt_context_and_exit().await,
        Command::Service { action } => handle_service(&action),
        Command::Doctor { fix_palaces, fix } => {
            if fix_palaces {
                trusty_memory::commands::doctor::handle_doctor_fix_palaces(fix).await?;
            }
            trusty_memory::commands::doctor::handle_doctor().await
        }
        Command::Monitor { target } => run_monitor(target).await,
        Command::SendMessage {
            to,
            purpose,
            content,
            from,
        } => handle_send_message(to, purpose, content, from).await,
        Command::InboxCheck { palace } => handle_inbox_check(palace).await,
        Command::Note {
            content,
            palace,
            tags,
        } => handle_note(content, palace, tags).await,
        Command::KgRebuild {
            palace,
            purge_stale_subjects,
            merge_punctuated_twins,
            dry_run,
        } => {
            trusty_memory::commands::kg_rebuild::handle_kg_rebuild_with(
                trusty_memory::commands::kg_rebuild::KgRebuildOptions {
                    palace,
                    purge_stale_subjects,
                    merge_punctuated_twins,
                    dry_run,
                },
            )
            .await
        }
        Command::Rooms {
            action: RoomsAction::Backfill { palace, apply, .. },
        } => trusty_memory::commands::rooms::handle_rooms_backfill(palace, apply).await,
        Command::Palace { action } => trusty_memory::commands::palace::dispatch(action).await,
        Command::BackfillReport {
            palace,
            limit,
            min_injections,
            json,
            logs_dir,
        } => {
            trusty_memory::commands::backfill_report::handle_backfill_report(
                trusty_memory::commands::backfill_report::ReportOptions {
                    palace,
                    limit,
                    min_injections,
                    json,
                    logs_dir,
                },
            )
            .await
        }
        Command::Link {
            path,
            slug,
            note,
            force,
        } => handle_link(path, slug, note, force),
        Command::Port { addr, json } => {
            let format = if json {
                trusty_memory::commands::port::PortFormat::Json
            } else if addr {
                trusty_memory::commands::port::PortFormat::Addr
            } else {
                trusty_memory::commands::port::PortFormat::Port
            };
            trusty_memory::commands::port::handle_port(format).await
        }
        Command::Upgrade { check, yes } => handle_upgrade(check, yes).await,
        Command::Config(cmd) => cmd.run().await,
    }
}

/// Dispatch the `monitor` subcommand.
///
/// Why: keeps `main` focused on parsing while putting the daemon-address
/// discovery and dashboard launch in one place.
/// What: `Tui` launches the trusty-memory-specific
/// `trusty_common::monitor::memory_tui` ratatui dashboard; `Status` and
/// `Palaces` print scriptable health and per-palace stats via the
/// `commands::monitor` handlers.
///
/// `Web` no longer opens anything (#6286). It used to start the daemon and
/// point a browser at `http://<addr>/ui`, which was this crate serving its own
/// SPA — the surface ADR-0032 retired. The dashboard mounts on
/// `trusty-console`, and until it does, saying so beats opening a URL nothing
/// answers. The subcommand stays rather than being removed so a muscle-memory
/// invocation gets the redirect instead of a clap usage error.
/// Test: not unit-tested (process-level entry point).
async fn run_monitor(target: MonitorTarget) -> Result<()> {
    use trusty_memory::commands::monitor;
    match target {
        MonitorTarget::Web => {
            eprintln!(
                "trusty-memory no longer serves a browser dashboard: ADR-0032 leaves \
                 trusty-console as the only HTTP surface, and the memory dashboard \
                 mounts there.\n\
                 Run `trusty-console` and open its memory page, or use \
                 `trusty-memory monitor tui` for the terminal dashboard."
            );
            Ok(())
        }
        MonitorTarget::Tui => trusty_common::monitor::memory_tui::run().await,
        MonitorTarget::Status { json } => monitor::handle_status(json).await,
        MonitorTarget::Palaces { id, json } => monitor::handle_palaces(id, json).await,
    }
}

/// Dispatch `serve --stdio` to the pure daemon-bridge MCP server (issue #1078).
///
/// Why: the prior direct-store path opened redb in the stdio process, which
/// collided with the HTTP daemon's exclusive write lock.  Reads fell back to
/// a stale snapshot; writes failed with "palace is read-only".  The fix is to
/// make the stdio process a pure proxy: it never touches redb.  Every JSON-RPC
/// request is forwarded over the daemon's Unix socket; if the daemon is not
/// running it is auto-started (detached, survives CLI exit).
/// Stdout hygiene: no update-check banner, no socket-bind announcement, no
/// eprintln! — stdout is the JSON-RPC channel and must carry only protocol
/// bytes.
/// What: delegates to `commands::serve_stdio_bridge::run_stdio_bridge` which
/// (1) ensures the daemon is running (auto-start + readiness poll), (2)
/// resolves this process' own caller identity, and (3) runs the shared
/// `trusty_mcp::DaemonBridgeJsonRpc` stdio loop against that socket (#6316).
/// Test: `tests/serve_stdio_e2e.rs` spawns a real child, asserts bounded
/// responses.  The bridge-specific unit tests live in
/// `commands/serve_stdio_bridge.rs`.
async fn run_serve_stdio(palace: Option<String>) -> Result<()> {
    trusty_memory::commands::serve_stdio_bridge::run_stdio_bridge(palace).await
}

/// Which server `serve` runs, decided from its transport flags.
///
/// Why: the bare-vs-flagged decision is the whole of #5267's behavior change, so
/// it is a value a test can assert on rather than a branch buried in `main`'s
/// dispatch. A test that only proved `serve` PARSES would have passed just as
/// well before the change.
/// Test: `serve_mode_*` in `cli_tests.rs`.
#[derive(Debug, PartialEq, Eq)]
enum ServeMode {
    /// MCP stdio JSON-RPC. `notify` is set only for the bare form, whose
    /// meaning changed and whose user may therefore be surprised.
    Stdio { notify: bool },
    /// The resident daemon, serving its Unix socket.
    Daemon,
}

/// Decide the `serve` transport from its flags (#5267).
///
/// Why: bare `serve` used to mean "detach an HTTP daemon" and now means "speak
/// MCP stdio", aligning with `trusty-search serve`. Only the no-flag case moved;
/// every flagged form resolves exactly as it did before, which is what keeps the
/// launchd plist, `handle_start`, and the existing integration tests working
/// untouched.
/// What: `--stdio` → stdio. `--http` (with or without an address) or
/// `--foreground` → the daemon. Nothing → stdio, with the notice flag set.
/// `--palace` is not a transport flag and does not affect the choice. `--http`
/// no longer names a transport — it survives only so a pre-#6286 launchd plist
/// still selects the daemon rather than failing clap.
/// Test: `serve_mode_bare_is_stdio` (fails before #5267),
/// `serve_mode_explicit_stdio`, `serve_mode_daemon_bare`, `serve_mode_daemon_addr`,
/// `serve_mode_foreground_is_daemon`, `serve_mode_palace_only_is_stdio`.
fn serve_mode(http: &Option<Option<SocketAddr>>, foreground: bool, stdio: bool) -> ServeMode {
    if stdio {
        return ServeMode::Stdio { notify: false };
    }
    if http.is_some() || foreground {
        return ServeMode::Daemon;
    }
    ServeMode::Stdio { notify: true }
}

/// Tell an interactive human that bare `serve` now speaks MCP stdio (#5267).
///
/// Why: bare `serve` used to detach an HTTP daemon and return the prompt. It now
/// blocks reading JSON-RPC from stdin, which to someone who typed it at a shell
/// looks exactly like a hang. The notice names the new verb so they are not left
/// guessing.
///
/// What: writes one line to **stderr** — never stdout, which is the JSON-RPC
/// channel — and only when stdin is a TTY, so an MCP client (whose stdin is a
/// pipe) sees nothing. The TTY check gates ONLY the notice: bare `serve` speaks
/// stdio identically either way. Behavior that varied by TTY would make the
/// tests lie about what a real client gets.
/// Test: `bare_serve_notice_absent_when_stdin_is_piped`,
/// `bare_serve_notice_present_when_stdin_is_a_tty`.
fn warn_bare_serve_is_stdio() {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        eprintln!(
            "trusty-memory: `serve` speaks MCP over stdio and is waiting on stdin. \
             To run the daemon, use `trusty-memory start`."
        );
    }
}

/// Dispatch `serve` to the resident daemon.
///
/// Why: keeps `main` focused on parsing while `AppState` construction lives in
/// one place. The `--stdio` path (PR1 #919) is `run_serve_stdio` above; this is
/// the process that owns redb and answers the socket.
/// What: resolves the palace registry directory (descending into the legacy
/// `palaces/` subdirectory when present — see `resolve_palace_registry_dir`),
/// builds an `AppState` rooted there, applies the `--palace` default if any,
/// re-hydrates every persisted palace, wires the #35 `LogBuffer` so
/// `memory.logs_tail` serves captured logs, installs the Phase 1 bug-capture
/// `ErrorStore` (#478), and hands the state to `transport::uds::serve`.
/// Test: not unit-tested (process-level entry point); exercised by
/// `transport::uds::tests` against the same `serve_with_shutdown` body, and
/// manually via `cargo run -p trusty-memory -- serve --foreground`.
async fn run_serve(
    http: Option<SocketAddr>,
    foreground: bool,
    palace: Option<String>,
    log_buffer: trusty_common::log_buffer::LogBuffer,
    error_store: trusty_common::error_capture::ErrorStore,
) -> Result<()> {
    // Background self-spawn path: when invoked without `--http` or
    // `--foreground`, fork a detached copy of ourselves with `serve
    // --foreground` and return immediately. Mirrors `trusty-search start` so
    // the parent shell keeps its prompt and tmux pane closures do not
    // SIGHUP the daemon.
    //
    // Supervisors (launchd, systemd, Docker) always pass `--foreground` and
    // stay on the inline path so they can manage the process lifecycle.
    if !foreground && http.is_none() {
        return trusty_memory::commands::start::handle_start().await;
    }

    // #6286: an address passed to `--http` is discarded. Warning rather than
    // failing is what keeps a pre-ADR-0032 launchd plist starting the daemon
    // instead of crash-looping on a flag that no longer means anything.
    if let Some(addr) = http {
        tracing::warn!("--http {addr} is ignored since #6286; the daemon serves a Unix socket");
        eprintln!(
            "trusty-memory: --http {addr} is ignored — the daemon serves \
             {} (ADR-0032)",
            trusty_memory::socket_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "its Unix socket".to_string())
        );
    }

    let socket = trusty_memory::socket_path()?;

    // Single-instance guard: if another healthy daemon is already serving the
    // socket, exit 0. launchd's `KeepAlive { SuccessfulExit: false }` respawns
    // only on NON-zero exits, so exit 0 stops the respawn storm cleanly. This
    // runs on every `serve --foreground` — launchd-spawned and manual alike —
    // so the guard is always active regardless of how the daemon was launched.
    {
        use trusty_memory::commands::single_instance as si;
        // #1152, Tier 3: 3 probes × 200 ms catches a mid-boot daemon.
        let action = si::single_instance_check_retried(&socket, 2, 200).await;
        match action {
            si::StartupAction::Proceed => {}
            si::StartupAction::ExitAlreadyRunning => {
                tracing::info!(
                    "single-instance guard: another trusty-memory instance is \
                     already running; exiting 0 to stop launchd respawn storm"
                );
                eprintln!(
                    "trusty-memory: another instance is already running; \
                     exiting cleanly (exit 0 stops launchd KeepAlive respawn)"
                );
                std::process::exit(0);
            }
            si::StartupAction::Fail(msg) => {
                anyhow::bail!("single-instance check failed unexpectedly: {msg}");
            }
        }

        // #6619: the guard above answers "is someone serving?", which is blind
        // to the bootout/bootstrap window — nothing serves the socket then, and
        // an unsupervised process bound it and kept it. Refuse the production
        // socket outright when a launchd unit owns it and launchd positively
        // reports it does not run us.
        let production = si::is_production_socket(&socket);
        let owner = trusty_common::launchd_claim::launchd_socket_owner(
            trusty_common::launchd_labels::MEMORY,
            production,
        );
        if let Some(refusal) = si::production_bind_refusal(
            trusty_common::launchd_labels::MEMORY,
            production,
            owner.is_launchd(),
            &trusty_common::supervision::launchd_supervision(),
        ) {
            anyhow::bail!(refusal);
        }
    }

    // Resolve the standard data dir, then descend into `palaces/` if that
    // legacy-layout subdirectory exists. Using the resolved directory as
    // `data_root` keeps every call site (status, palace_list, open_palace,
    // palace_create, load_palaces_from_disk) pointed at the same place.
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")?;
    // Defense-in-depth (belt-and-suspenders, #503): assert the resolved data
    // root is absolute and not the filesystem root before binding. This guards
    // against any future resolver path that could produce a bad dir — even if
    // trusty_common::resolve_data_dir's own guards fire first, a second check
    // here means a misconfigured deployment fails loudly at startup rather than
    // silently scattering palaces across `/`.
    if !data_dir.is_absolute() {
        anyhow::bail!(
            "resolved trusty-memory data directory {:?} is not absolute; \
             refusing to start to prevent palace directories from being created \
             under the daemon working directory",
            data_dir
        );
    }
    if data_dir == std::path::Path::new("/") {
        anyhow::bail!(
            "resolved trusty-memory data directory is the filesystem root (/); \
             refusing to start to prevent palace directories from being created \
             directly under /",
        );
    }
    let data_root = resolve_palace_registry_dir(data_dir);

    // Apply one-shot, idempotent on-disk migrations before any in-memory
    // registry hydration so subsequent `load_palaces_from_disk` calls see the
    // updated metadata. Currently this rewrites the default `localLLM`
    // palace's display name to "User Memories" when the legacy literal is
    // still present (issue #98). Failures here are logged but do not abort
    // startup — a single bad migration must not take the daemon down.
    if let Err(e) = trusty_memory::commands::migrations::migrate_default_palace_name(&data_root) {
        tracing::warn!("default-palace name migration skipped: {e:#}");
    }

    // Build the daemon AppState once — the builder chain is identical for the
    // fixed-port and dynamic/foreground HTTP paths. Issue #1487:
    // `with_writer_intent()` MUST come first (it replaces the registry) and
    // run before `spawn_startup_tasks` hydration so palace redb files open as
    // `Writer` — a second daemon instance then fails loud instead of silently
    // degrading to read-only snapshot mode. Bug-reporting #478 wires the
    // ErrorStore; #156/#193 opt into the BM25 lexical lane when enabled.
    // Issue #2223: `with_multi_tenant_mode_from_env()` reads
    // `TRUSTY_MEMORY_MULTI_TENANT=1` and was defined + unit-tested (issue
    // #1714 / PR #2221) but never called here, so the opt-in authz seam was
    // dead in the shipped binary. Wiring it here is additive-only: default
    // (unset) preserves today's single-tenant behaviour exactly.
    let state = AppState::new(data_root)
        .with_writer_intent()
        .with_default_palace(palace)
        .with_log_buffer(log_buffer)
        .with_error_store(error_store)
        .with_bm25_lane_from_env()
        .with_multi_tenant_mode_from_env();
    spawn_startup_tasks(&state);
    // `foreground` is already spent: the background branch returned via
    // `handle_start` above, so anything reaching here runs inline.
    let _ = foreground;
    serve(state, &socket).await
}

// #4678: moved out of this file to keep it under the 500 SLOC cap.
#[path = "startup_tasks.rs"]
mod startup_tasks;
use startup_tasks::spawn_startup_tasks;

// CLI parse tests: `serve --http` / `--stdio` semantics (#914 PR4)
#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
// ---------------------------------------------------------------------------
// Tests for spawn_startup_tasks (#474)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "startup_task_tests.rs"]
mod startup_task_tests;
