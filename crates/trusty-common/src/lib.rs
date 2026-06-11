//! Shared utility surface for trusty-* projects.
//!
//! Why: Port auto-detect, data-directory resolution, tracing init, NO_COLOR
//! handling, and the OpenRouter chat-completions client appeared in both
//! trusty-memory and trusty-search with subtle divergence. Centralising keeps
//! them aligned and gives future trusty-* binaries a one-import surface.
//!
//! What: pure utility functions — no global state. Each subsystem is a free
//! function or a small helper struct.
//!
//! Test: `cargo test -p trusty-common` covers port walking, data-dir creation,
//! and the OpenRouter request shape (without hitting the network).
//!
//! # Test isolation: `TRUSTY_DATA_DIR_OVERRIDE`
//!
//! macOS's [`dirs::data_dir()`] resolves the application-support directory via
//! `NSFileManager`, a native Cocoa API that completely ignores the `HOME` and
//! `XDG_DATA_HOME` environment variables. This makes it impossible to redirect
//! data-directory access in tests using ordinary env-var tricks, because the
//! kernel query bypasses the environment entirely.
//!
//! To work around this, [`resolve_data_dir`] checks the
//! [`DATA_DIR_OVERRIDE_ENV`] (`TRUSTY_DATA_DIR_OVERRIDE`) environment variable
//! before consulting `dirs::data_dir()`. When set, the variable's value is used
//! as the base directory verbatim, and `dirs::data_dir()` is never called.
//!
//! **This escape hatch is intended for testing only.** Do not set it in
//! production deployments; rely on the OS-standard data directory instead.

pub mod chat;
pub mod claude_config;
pub mod project_discovery;

/// Shared graceful-shutdown signal helper for trusty-* daemons (issue #534).
///
/// Why: trusty-search, trusty-memory, and trusty-analyze all need the same
/// SIGTERM + SIGINT shutdown future to pass to axum's `with_graceful_shutdown`.
/// Centralising it here eliminates three-way duplication and guarantees every
/// daemon responds identically to `launchctl bootout`.
/// What: exposes [`shutdown_signal`] — an async fn that resolves on SIGTERM
/// (unix) or SIGINT/Ctrl-C (all platforms), whichever fires first.
/// Test: `cargo test -p trusty-common -- shutdown`.
pub mod shutdown;
pub use shutdown::shutdown_signal;

/// Bounded in-memory ring buffer of recent tracing log lines.
///
/// Why: trusty-* daemons expose a `/logs/tail` endpoint so operators can read
/// recent logs over HTTP without file I/O or a daemon restart. The buffer and
/// its `tracing_subscriber::Layer` live here so every daemon shares one impl.
/// What: `LogBuffer` (thread-safe capped `VecDeque<String>`) plus
/// `LogBufferLayer` (the tracing layer that feeds it).
/// Test: `cargo test -p trusty-common log_buffer` covers capacity eviction,
/// tail semantics, and layer capture.
pub mod log_buffer;

/// Process RSS / CPU sampling and data-directory sizing for daemon health.
///
/// Why: every trusty-* daemon's `/health` endpoint reports its own resident
/// memory, CPU usage, and on-disk footprint; the sampling logic is identical
/// across them so it lives here once.
/// What: `SysMetrics` (per-process RSS + CPU sampler) and `dir_size_bytes`
/// (recursive directory byte count).
/// Test: `cargo test -p trusty-common sys_metrics`.
pub mod sys_metrics;

/// macOS LaunchAgent generation and lifecycle management. macOS-only —
/// the module compiles to nothing on every other platform.
#[cfg(target_os = "macos")]
pub mod launchd;

#[cfg(feature = "axum-server")]
pub mod server;

/// Shared JSON-RPC 2.0 / MCP primitives (formerly the `trusty-mcp-core` crate).
///
/// Why: Centralises `Request`/`Response`/`JsonRpcError` envelopes, the
/// `initialize` response builder, an async stdio dispatch loop, and the
/// OpenRPC `rpc.discover` helpers so every MCP server in the workspace
/// imports the same types.
/// What: Gated behind the `mcp` feature; pulls in no extra dependencies
/// beyond `serde` / `tokio`, both of which are already required.
/// Test: `cargo test -p trusty-common --features mcp` runs the module's
/// own unit tests (envelope round-trips, stdio loop dispatch, OpenRPC
/// builder shape).
#[cfg(feature = "mcp")]
pub mod mcp;

/// General-purpose JSON-RPC client + transports (formerly the library half
/// of the `trusty-rpc` crate).
///
/// Why: Both `trpc` (the CLI) and any future library consumer want one
/// place that owns the JSON-RPC envelope construction, stdio-subprocess
/// transport, HTTP transport, and pretty-printers.
/// What: Gated behind the `rpc` feature; requires `uuid` for request id
/// generation. The HTTP transport reuses the workspace `reqwest`.
/// Test: `cargo test -p trusty-common --features rpc` runs the module's
/// own unit tests (envelope extraction, pretty-print smoke tests).
#[cfg(feature = "rpc")]
pub mod rpc;

/// Shared text-embedding abstraction (formerly the `trusty-embedder` crate).
///
/// Why: trusty-memory and trusty-search both ship near-identical `Embedder`
/// traits and `FastEmbedder` implementations; centralising the surface here
/// keeps them aligned and lets future consumers pick up embedding for free
/// without a separate published crate.
/// What: Gated behind the `embedder` feature. Exposes the `Embedder` trait,
/// `FastEmbedder` (fastembed-rs, all-MiniLM-L6-v2, 384-d) with LRU caching
/// and ORT warmup, and (under `embedder-test-support`) the `MockEmbedder`
/// test double.
/// Test: `cargo test -p trusty-common --features embedder,embedder-test-support`
/// covers the mock embedder and ONNX-backed `#[ignore]`d integration tests.
#[cfg(feature = "embedder")]
pub mod embedder;

/// Unified RPC client surface for the `trusty-embedderd` standalone process.
///
/// Why: absorbs both the former `trusty-embedder-client` HTTP crate (PR #163)
/// and the former `embed_client` UDS module (PR #157) into a single unified
/// module. Reduces workspace crate count and provides one trait (`EmbedderClient`)
/// with three concrete implementations (InProcess, HTTP remote, UDS remote) so
/// call sites are identical regardless of transport. The `embed-client` feature
/// and `embed_client` module are retired by issue #164; use `embedder-client`
/// and `trusty_common::embedder_client::UdsEmbedderClient` instead.
/// What: Gated behind the `embedder-client` feature. Exposes the
/// `EmbedderClient` trait, `InProcessEmbedderClient`, `RemoteEmbedderClient`
/// (HTTP), `UdsEmbedderClient` (UDS), `EmbedRequest` / `EmbedResponse` wire
/// types, and `EmbedderError`. The UDS impl uses `tokio::net::UnixStream`
/// with newline-framed JSON-RPC 2.0 — no additional dependencies.
/// Test: `cargo test -p trusty-common --features embedder-client` covers
/// error-display, JSON round-trip, URL assembly, UDS wire types, and empty-
/// batch short-circuits. ONNX-backed tests are in
/// `trusty-embedderd/tests/bit_identical.rs` (`#[ignore]`).
#[cfg(feature = "embedder-client")]
pub mod embedder_client;

/// Zero-dependency BM25 lexical index + code-aware tokenizer (issue #156).
///
/// Why: trusty-memory, trusty-search, and the per-palace
/// `trusty-bm25-daemon` subprocess all want one shared BM25 implementation
/// so the tokenizer's camelCase / PascalCase / alpha↔digit splits stay
/// consistent across the workspace. Originally ported from open-mpm; now
/// the single source of truth lives here.
/// What: Gated behind the `bm25` feature. Adds no new dependencies — pure
/// `std` + `tracing` (already required).
/// Test: `cargo test -p trusty-common --features bm25`.
#[cfg(feature = "bm25")]
pub mod bm25;

/// Reusable schema-migration kernel (issue #179).
///
/// Why: trusty-search, trusty-memory, and other long-lived stores have grown
/// ad-hoc schema-migration loops that drift apart. Centralising the
/// `SchemaVersion` newtype, the `Migration<S>` trait, and a `MigrationRunner`
/// that applies pending steps in order (writing a stamp after each) collapses
/// those into one shared kernel. The `file_stamp` helper covers the common
/// "JSON sidecar in the store's data dir" stamp format; redb-stamp users get
/// a documented recipe instead of a heavyweight dep.
/// What: gated behind the `migrations` feature flag. Adds no new
/// dependencies — pure `serde` + `serde_json` + `anyhow` + `tracing` which
/// the crate already requires.
/// Test: `cargo test -p trusty-common --features migrations` covers the
/// runner ordering, crash resumption, write-stamp failure propagation, and
/// the file-stamp round-trip / atomic-write behaviour.
#[cfg(feature = "migrations")]
pub mod migrations;

/// UDS JSON-RPC client for the per-palace `trusty-bm25-daemon` subprocess
/// (issue #156).
///
/// Why: trusty-memory needs a lexical-search lane without holding an
/// in-process BM25 index. `Bm25Client` delegates to the per-palace daemon
/// over `$TMPDIR/trusty-bm25-<palace>.sock`, matching the design of
/// `EmbedClient` and `trusty-embed-daemon` (PR #157).
/// What: Gated behind the `bm25-client` feature. Pure user of existing
/// `tokio` / `serde_json` / `anyhow` workspace deps — adds no new
/// dependencies.
/// Test: `cargo test -p trusty-common --features bm25-client` covers
/// request shape and path defaults; end-to-end coverage lives in
/// `trusty-bm25-daemon/tests/`.
#[cfg(feature = "bm25-client")]
pub mod bm25_client;

/// Symbol-graph engine (formerly the `trusty-symgraph` crate).
///
/// Why: All trusty-* tools that touch source code (open-mpm, trusty-search,
/// trusty-analyze) want the same `EntityType` / `RawEntity` / `EdgeKind`
/// data shapes and (for orchestrators) the same tree-sitter pipeline. Living
/// here lets the workspace ship one tree-sitter `links =` slot instead of
/// juggling two crates that both claim it.
/// What: Gated behind two features. `symgraph` exposes only the contracts
/// surface (`EntityType`, `RawEntity`, `EdgeKind`, `fact_hash_str`, tables)
/// — no tree-sitter, no `links` conflict. `symgraph-parser` additionally
/// pulls in tree-sitter and the full parse → registry → emit stack.
/// `symgraph-server` enables the HTTP server frontend.
/// Test: `cargo test -p trusty-common --features symgraph` exercises the
/// contracts surface; `cargo test -p trusty-symgraph` covers the parser
/// path through the thin re-export shim.
#[cfg(feature = "symgraph")]
pub mod symgraph;

/// Memory Palace storage engine (formerly the `trusty-memory-core` crate).
///
/// Why: Centralises the Memory Palace data model (`Palace` / `Wing` /
/// `Room` / `Drawer`), storage backends (usearch vector index + SQLite
/// knowledge graph + chat-session log + payload store), retrieval handle,
/// and the dream / decay / analytics / git-history surfaces so every
/// trusty-* binary that talks to a palace reuses the same types. Absorbed
/// into `trusty-common` (issue #5 phase 2d) so we ship one fewer published
/// crate.
/// What: Gated behind the `memory-core` feature because it pulls in heavy
/// storage deps (`usearch`, `rusqlite`, `r2d2`, `git2`, `kuzu`). Enables
/// the embedder surface automatically (memory-core → embedder).
/// Test: `cargo test -p trusty-common --features memory-core` exercises
/// the full surface.
#[cfg(feature = "memory-core")]
pub mod memory_core;

/// Unified ticketing MCP server (formerly the `trusty-tickets` crate).
///
/// Why: Claude Code and the rest of the trusty-* suite need a single MCP
/// surface that can talk to GitHub Issues, JIRA, and Linear without the
/// caller needing to know which backend is configured. Absorbing into
/// `trusty-common` reduces the workspace crate count and co-locates the
/// HTTP client surface with the other protocol helpers.
/// What: Gated behind the `tickets` feature. Exposes `tickets::api::*`
/// (config, models, Backend trait, three concrete backends), `tickets::server`
/// (MCP dispatch loop + `run_stdio`), and `tickets::tools` (the tool-list
/// schema). Requires the `mcp` feature for the stdio loop.
/// Test: `cargo test -p trusty-common --features tickets` runs the module's
/// own unit tests (dispatch, tool-list counts, config parsing, serde
/// round-trips). Live backend tests require env-var credentials.
#[cfg(feature = "tickets")]
pub mod tickets;

/// Declarative CLI help system with "did you mean?" suggestions (issue #216).
///
/// Why: every standalone trusty-* binary used to render its `--help` and
/// unknown-subcommand error output independently, so the formats drifted
/// apart over time. Centralising the help model into one YAML schema, one
/// canonical renderer, and one Jaro-Winkler suggester keeps the six binaries
/// (search, memory, analyze, mpm-cli, tga, open-mpm) speaking with a single
/// user-facing voice.
/// What: gated behind the `cli-help` feature. Pulls in `serde_yaml`, `strsim`,
/// and `indexmap`. Exposes `HelpConfig` / `CommandDef` / `FlagDef` / `Example`
/// + `load_help` / `render_help` / `suggest`.
/// Test: `cargo test -p trusty-common --features cli-help`.
#[cfg(feature = "cli-help")]
pub mod help;

/// Unified monitor TUI for the trusty-search and trusty-memory daemons
/// (formerly the `trusty-monitor-tui` crate).
///
/// Why: operators run both daemons and want one terminal surface that shows
/// the health of both at a glance. Living here behind the `monitor-tui`
/// feature flag matches the workspace's "one fewer published crate" direction
/// (issue #31 companion) and keeps the dashboard logic unit-testable.
/// What: gated behind the `monitor-tui` feature, which pulls in `ratatui` and
/// `crossterm`. Exposes `monitor::run` (the entry point the `trusty-monitor`
/// binary calls) plus the pure `dashboard` / `search_client` / `memory_client`
/// submodules.
/// Test: `cargo test -p trusty-common --features monitor-tui` covers the
/// rendering, layout, and HTTP-client pieces.
#[cfg(feature = "monitor-tui")]
pub mod monitor;

// epic #1104: stdio MCP client + console metrics contract (feature-gated).
#[cfg(feature = "console-metrics")]
pub mod console_metrics;
#[cfg(feature = "stdio-mcp-client")]
pub mod stdio_mcp_client;

/// Throttled crates.io update-notification helper.
///
/// Why: User-facing CLIs should nudge operators when a newer release is
/// available without adding perceptible latency. A shared implementation
/// keeps the throttle, cache, opt-out, and User-Agent logic consistent across
/// every consumer in the workspace.
/// What: Gated behind the `update-check` feature. Exposes
/// [`update::check_throttled`] (the main entry — reads a per-crate JSON cache
/// under the OS cache dir, queries crates.io at most once per 24 h),
/// [`update::check_crates_io`] (the raw network call), [`update::notice`]
/// (formatted upgrade message), and [`update::UpdateInfo`] (the result type).
/// All failures degrade to `None` — the check is best-effort and will not
/// panic or stall a CLI.
/// Opt-out: set `TRUSTY_NO_UPDATE_CHECK` or `CI` to any non-empty value.
/// Test: `cargo test -p trusty-common --features update-check`.
#[cfg(feature = "update-check")]
pub mod update;

/// Error-capture layer for the trusty-* consent-gated bug-reporting system
/// (bug-reporting Phase 1, issue #479).
///
/// Why: Every trusty-* daemon encounters runtime errors that developers need
///      to see but that must be captured locally and only filed to GitHub after
///      explicit user consent. A shared capture layer in `trusty-common` means
///      all daemons gain error capture without per-binary changes.
/// What: Gated behind the `bug-capture` feature. Exposes:
///      - [`error_capture::CapturedError`] — structured error record.
///      - [`error_capture::ErrorStore`] — ring buffer + JSONL store.
///      - [`error_capture::BugCaptureLayer`] — the tracing Layer.
///      - [`error_capture::bug_capture_layer`] — convenience constructor.
///      - [`error_capture::TRUSTY_NO_BUG_CAPTURE_ENV`] — opt-out env name.
///      Additive: does not alter stderr logging. Opt-out via
///      `TRUSTY_NO_BUG_CAPTURE=1`. New dep: `sha2` (already workspace-optional).
/// Test: `cargo test -p trusty-common --features bug-capture`.
#[cfg(feature = "bug-capture")]
pub mod error_capture;

// ─── Focused submodules (split from lib.rs in issue #1108) ────────────────

/// TCP port auto-walking helper.
///
/// Why: Running multiple daemon instances shouldn't produce noisy failures
/// when a port is already occupied.
/// What: Exposes [`bind_with_auto_port`] which walks forward to the next free
/// port within `max_attempts`.
/// Test: `cargo test -p trusty-common -- port::tests`.
pub mod port;

/// Data-directory resolution and filesystem utilities.
///
/// Why: All trusty-* tools share the same per-app data-directory resolution
/// logic including the macOS `NSFileManager` bypass needed for test isolation.
/// What: Exposes [`data_dir::resolve_data_dir`], [`data_dir::sanitize_data_root`],
/// [`data_dir::DATA_DIR_OVERRIDE_ENV`], and [`data_dir::is_dir`].
/// Test: `cargo test -p trusty-common -- data_dir::tests`.
pub mod data_dir;

/// Daemon HTTP-address file helpers.
///
/// Why: Both trusty-search and trusty-memory persist their bound `host:port`
/// to disk for discovery by CLI and MCP clients. Centralising keeps them in sync.
/// What: Exposes [`daemon_addr::write_daemon_addr`], [`daemon_addr::read_daemon_addr`],
/// and [`daemon_addr::check_already_running`].
/// Test: `cargo test -p trusty-common -- daemon_addr::tests`.
pub mod daemon_addr;

/// HTTP health-probe helper.
///
/// Why: Every daemon uses the same tight-timeout `/health` probe to detect
/// whether a prior instance is still running.
/// What: Exposes [`health_probe::probe_health`].
/// Test: covered via daemon_addr integration tests.
pub mod health_probe;

/// Global tracing subscriber initialisation helpers.
///
/// Why: Every trusty-* binary needs the same verbosity ladder, `RUST_LOG`
/// override, and (for daemons) the log-buffer + bug-capture layer composition.
/// What: Exposes [`tracing_init::init_tracing`],
/// [`tracing_init::init_tracing_with_buffer`],
/// [`tracing_init::init_tracing_with_buffer_and_capture`] (feature-gated),
/// and [`tracing_init::maybe_disable_color`].
/// Test: side-effecting global — covered by downstream integration tests.
pub mod tracing_init;

/// Deprecated single-shot OpenRouter helpers.
///
/// Why: Backward-compatible wrapper for the pre-streaming OpenRouter API.
/// New code should use `chat::OpenRouterProvider::chat_stream` instead.
/// What: Exposes [`openrouter_legacy::ChatMessage`],
/// [`openrouter_legacy::openrouter_chat`] (deprecated), and
/// [`openrouter_legacy::openrouter_chat_stream`] (deprecated).
/// Test: `chat_message_round_trips`, `openrouter_chat_rejects_empty_key`.
pub mod openrouter_legacy;

// ─── Re-exports preserving the pre-split public API ───────────────────────

pub use chat::{
    BedrockProvider, ChatEvent, ChatProvider, DEFAULT_BEDROCK_MODEL, LocalModelConfig,
    OllamaProvider, OpenRouterProvider, ToolCall, ToolDef, auto_detect_local_provider,
};

// Port
pub use port::bind_with_auto_port;

// Data directory
pub use data_dir::{DATA_DIR_OVERRIDE_ENV, is_dir, resolve_data_dir, sanitize_data_root};

// Daemon address
pub use daemon_addr::{check_already_running, read_daemon_addr, write_daemon_addr};

// Health probe
pub use health_probe::probe_health;

// Tracing init
#[cfg(feature = "bug-capture")]
pub use tracing_init::init_tracing_with_buffer_and_capture;
pub use tracing_init::{init_tracing, init_tracing_with_buffer, maybe_disable_color};

// OpenRouter legacy (deprecated but must remain reachable)
#[allow(deprecated)]
pub use openrouter_legacy::{ChatMessage, openrouter_chat, openrouter_chat_stream};
