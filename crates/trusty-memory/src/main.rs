//! CLI entry point for the `trusty-memory` binary.
//!
//! Why: ship a thin clap-to-handler shim so users can `cargo install
//! trusty-memory` and invoke `trusty-memory serve` (the HTTP/SSE daemon
//! consumed by Claude Code via the `trusty-memory-mcp-bridge` companion
//! binary) or `trusty-memory migrate kuzu-memory` (which rewrites Claude
//! settings files that still reference the legacy kuzu-memory MCP server).
//! All real logic lives in the library and the `commands::migrate` module —
//! this file does CLI parsing and dispatch only.
//! What: defines a `clap::Parser` with `serve` and `migrate` subcommands.
//! `serve` defers to `trusty_memory::run_http` / `run_http_dynamic`;
//! Claude Code talks to the daemon through the `trusty-memory-mcp-bridge`
//! stdio-to-UDS pipe (PR #149). `migrate` defers to
//! `commands::migrate::handle_migrate`.
//! Test: `cargo run -p trusty-memory -- --help` lists both subcommands.
//! `cargo run -p trusty-memory -- migrate kuzu-memory --dry-run` exercises
//! the migrate path end-to-end without modifying any files.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use trusty_memory::commands::inbox_check::handle_inbox_check;
use trusty_memory::commands::migrate::{handle_migrate, MigrateTarget};
use trusty_memory::commands::prompt_context::handle_prompt_context;
use trusty_memory::commands::send_message::handle_send_message;
use trusty_memory::commands::service::{handle_service, ServiceAction};
use trusty_memory::commands::setup::handle_setup;
use trusty_memory::commands::start::handle_start;
use trusty_memory::commands::stop::handle_stop;
use trusty_memory::{resolve_palace_registry_dir, run_http, run_http_dynamic, AppState};

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

    /// Run the daemon.
    ///
    /// Default mode is HTTP/SSE with dynamic port selection (7070..=7079, OS
    /// fallback). Without `--foreground`, `serve` self-spawns a detached
    /// background daemon (alias for `start`) and returns immediately so the
    /// parent shell gets its prompt back. Pass `--foreground` to keep the
    /// daemon in the foreground (used internally by `start` to host the
    /// actual HTTP server, and by launchd / systemd). Pass `--http <ADDR>`
    /// to bind a specific address.
    ///
    /// Claude Code integration: install the `trusty-memory-mcp-bridge`
    /// binary into your `.mcp.json` — it pipes stdio between Claude Code
    /// and the daemon over a Unix domain socket (PR #149). The legacy
    /// `serve --stdio` flag was removed in PR for #150 because it
    /// deadlocked on the redb exclusive write lock whenever a daemon was
    /// already running.
    Serve {
        /// Bind the HTTP/SSE server to a specific address. When omitted,
        /// the daemon binds dynamically.
        #[arg(long, value_name = "ADDR")]
        http: Option<SocketAddr>,

        /// Run the HTTP daemon in the foreground (do not self-spawn).
        ///
        /// Why: `serve` defaults to background mode so the trusty-* daemons
        /// share a `start` / `serve` UX. Long-running supervisors (launchd,
        /// systemd, Docker) need a foreground process to manage, so they
        /// pass `--foreground` to opt out of the spawn.
        #[arg(long)]
        foreground: bool,

        /// Bind every MCP tool call to this palace when the caller omits the
        /// `palace` argument.
        #[arg(long, value_name = "NAME")]
        palace: Option<String>,
    },

    /// Migrate from another memory MCP server to trusty-memory.
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
    },

    /// First-time setup: data dir + launchd (macOS) + Claude settings patch.
    Setup,

    /// Print the daemon's prompt-context block to stdout (Claude Code hook).
    ///
    /// Why: installed as a Claude Code `UserPromptSubmit` hook by
    /// `trusty-memory setup`. Claude Code injects whatever the hook writes to
    /// stdout as additional context for the next prompt, so this command
    /// fetches the daemon's pre-formatted prompt-context block and prints it
    /// verbatim. Every failure path exits 0 silently so the hook can never
    /// block a Claude Code prompt; the `CLAUDE_MPM_SUB_AGENT` env var also
    /// short-circuits this command to keep nested MPM agents from piling on
    /// duplicate prompt-context blocks.
    /// What: see `commands::prompt_context::handle_prompt_context`.
    /// Test: covered by the unit test in that module plus the integration
    /// path `cargo run -p trusty-memory -- prompt-context` against a live
    /// daemon.
    #[command(name = "prompt-context")]
    PromptContext,

    /// Diagnose daemon health: fastembed cache, launchd plist, HTTP /health,
    /// and stale palace locks.
    ///
    /// Why: GH #62 — silent failures (missing `FASTEMBED_CACHE_PATH` in the
    /// plist, missing model cache, daemon not bound) currently force users
    /// to grep through several directories by hand. `doctor` runs the
    /// equivalent checks in one shot.
    /// What: a one-shot CLI command that prints a ✅/❌ line per check and
    /// exits non-zero on any failure. See `commands::doctor`.
    /// Test: `cargo run -p trusty-memory -- doctor` after `setup`.
    Doctor,

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
    #[command(name = "kg-rebuild")]
    KgRebuild {
        /// Restrict the rebuild to a single palace id. When omitted, every
        /// palace under the data root is processed.
        #[arg(long, value_name = "ID")]
        palace: Option<String>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Issue #35: initialise tracing with an in-memory `LogBuffer` so the HTTP
    // daemon's `GET /api/v1/logs/tail` endpoint can serve recent logs. The
    // buffer-backed subscriber still writes the standard `fmt` layer to
    // stderr, so non-HTTP subcommands (and the MCP stdio path, which must
    // keep stdout clean) are unaffected. The buffer is only wired into the
    // `AppState` on the HTTP serve path.
    let log_buffer = trusty_common::init_tracing_with_buffer(
        cli.verbose,
        trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
    );

    match cli.command {
        Command::Start => handle_start().await,
        Command::Stop => handle_stop().await,
        Command::Serve {
            http,
            foreground,
            palace,
        } => run_serve(http, foreground, palace, log_buffer).await,
        Command::Migrate {
            target,
            dry_run,
            config_only,
        } => handle_migrate(target, dry_run, config_only),
        Command::Setup => handle_setup(),
        Command::PromptContext => handle_prompt_context().await,
        Command::Service { action } => handle_service(&action),
        Command::Doctor => trusty_memory::commands::doctor::handle_doctor().await,
        Command::Monitor { target } => run_monitor(target).await,
        Command::SendMessage {
            to,
            purpose,
            content,
            from,
        } => handle_send_message(to, purpose, content, from).await,
        Command::InboxCheck { palace } => handle_inbox_check(palace).await,
        Command::KgRebuild { palace } => {
            trusty_memory::commands::kg_rebuild::handle_kg_rebuild(palace).await
        }
    }
}

/// Dispatch the `monitor` subcommand.
///
/// Why: keeps `main` focused on parsing while putting the daemon-address
/// discovery and dashboard launch in one place.
/// What: `Web` resolves the live daemon address from the lock file and prints
/// its `/ui` URL (exiting non-zero when no daemon is running); `Tui` launches
/// the trusty-memory-specific `trusty_common::monitor::memory_tui` ratatui
/// dashboard; `Status` and `Palaces` print scriptable health and per-palace
/// stats via the `commands::monitor` handlers.
/// Test: not unit-tested (process-level entry point); `cargo run -p
/// trusty-memory -- monitor --help` lists every target.
async fn run_monitor(target: MonitorTarget) -> Result<()> {
    use trusty_memory::commands::monitor;
    match target {
        MonitorTarget::Web => match trusty_common::read_daemon_addr("trusty-memory")? {
            Some(addr) => {
                println!("{addr}/ui");
                Ok(())
            }
            None => {
                eprintln!("trusty-memory daemon not running (no address found)");
                std::process::exit(1);
            }
        },
        MonitorTarget::Tui => trusty_common::monitor::memory_tui::run().await,
        MonitorTarget::Status { json } => monitor::handle_status(json).await,
        MonitorTarget::Palaces { id, json } => monitor::handle_palaces(id, json).await,
    }
}

/// Dispatch `serve` to the HTTP server (background spawn or inline foreground).
///
/// Why: keeps `main` focused on parsing while putting the `AppState`
/// construction in one place. Issue #150 removed the legacy `--stdio` flag
/// — Claude Code now talks to the daemon through the
/// `trusty-memory-mcp-bridge` binary (PR #149) over a Unix domain socket,
/// which sidesteps the redb exclusive-lock deadlock that made the in-process
/// stdio path unusable whenever a long-lived daemon was already running.
/// What: resolves the palace registry directory (descending into the legacy
/// `palaces/` subdirectory when present — see `resolve_palace_registry_dir`),
/// builds an `AppState` rooted there, applies the `--palace` default if any,
/// re-hydrates every persisted palace, and wires the issue-#35 `LogBuffer`
/// so `GET /api/v1/logs/tail` serves captured logs.
/// Test: not unit-tested (process-level entry point); exercised manually via
/// `cargo run -p trusty-memory -- serve` and the parent integration tests.
async fn run_serve(
    http: Option<SocketAddr>,
    foreground: bool,
    palace: Option<String>,
    log_buffer: trusty_common::log_buffer::LogBuffer,
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

    // Resolve the standard data dir, then descend into `palaces/` if that
    // legacy-layout subdirectory exists. Using the resolved directory as
    // `data_root` keeps every call site (status, palace_list, open_palace,
    // palace_create, load_palaces_from_disk) pointed at the same place.
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")?;
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

    if let Some(addr) = http {
        let state = AppState::new(data_root)
            .with_default_palace(palace)
            .with_log_buffer(log_buffer);
        // Why: previously, `load_palaces_from_disk` was awaited synchronously
        // before binding the HTTP listener. A single broken `kg.db` (stale
        // WAL sidecar, corrupt file, permissions) could stall hydration for
        // seconds per palace, deferring `/health` becoming reachable until
        // every palace had been visited. The dashboard, MCP clients, and
        // `launchctl` health-probes all interpret that as "the daemon is
        // dead", so the launchd job thrashes and operators see no useful
        // output. Spawning hydration as a background task lets the HTTP
        // server bind immediately; palaces appear in `palace_list` and the
        // dashboard as each one finishes opening. Per-palace failures are
        // already logged and skipped inside `load_palaces_from_disk` so a
        // single bad `kg.db` can never abort the daemon.
        // What: `AppState` derives `Clone` (its internals are `Arc`-wrapped),
        // so the background task gets a cheap clone that shares the same
        // registry the serving state writes into. We log start, summary, and
        // total elapsed time so operators can see the warmup completing in
        // the daemon log.
        let bg_state = state.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            tracing::info!("starting background palace hydration");
            match bg_state.load_palaces_from_disk().await {
                Ok(count) => tracing::info!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "background palace hydration complete: {count} palaces loaded"
                ),
                Err(e) => tracing::error!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "background palace hydration failed: {e:#}"
                ),
            }
            // Issue #42: once palaces are live, kick off auto-discovery
            // against cwd targeting the default palace (if configured).
            // Without a default palace there's no obvious destination, so
            // skip — explicit MCP `discover_aliases` calls still work.
            if let Some(palace) = bg_state.default_palace.clone() {
                if let Ok(cwd) = std::env::current_dir() {
                    bg_state.spawn_alias_discovery(palace, cwd);
                }
            }
        });
        run_http(state, addr).await
    } else {
        // Default: dynamic-port HTTP daemon. Mirrors the explicit `--http`
        // branch above (log buffer, background hydration) but lets the
        // library pick a port from 7070..=7079 (OS-fallback) and write
        // `~/.trusty-memory/http_addr` for clients to discover.
        let state = AppState::new(data_root)
            .with_default_palace(palace)
            .with_log_buffer(log_buffer);
        let bg_state = state.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            tracing::info!("starting background palace hydration");
            match bg_state.load_palaces_from_disk().await {
                Ok(count) => tracing::info!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "background palace hydration complete: {count} palaces loaded"
                ),
                Err(e) => tracing::error!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "background palace hydration failed: {e:#}"
                ),
            }
            // Issue #42: once palaces are live, kick off auto-discovery
            // against cwd targeting the default palace (if configured).
            // Without a default palace there's no obvious destination, so
            // skip — explicit MCP `discover_aliases` calls still work.
            if let Some(palace) = bg_state.default_palace.clone() {
                if let Ok(cwd) = std::env::current_dir() {
                    bg_state.spawn_alias_discovery(palace, cwd);
                }
            }
        });
        run_http_dynamic(state).await
    }
}
