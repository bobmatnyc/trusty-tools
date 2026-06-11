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

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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

pub use chat::{
    BedrockProvider, ChatEvent, ChatProvider, DEFAULT_BEDROCK_MODEL, LocalModelConfig,
    OllamaProvider, OpenRouterProvider, ToolCall, ToolDef, auto_detect_local_provider,
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

// ─── Port binding ─────────────────────────────────────────────────────────

/// Bind to `addr`; if the port is in use, walk forward up to `max_attempts`
/// ports and return the first listener that binds.
///
/// Why: Running multiple instances of a trusty-* daemon (or restarting before
/// the kernel releases the prior socket) shouldn't produce a noisy failure —
/// auto-incrementing gives a friendlier developer experience while still
/// honouring the user's preferred starting port.
/// What: returns the first successful `tokio::net::TcpListener`. Callers can
/// inspect `local_addr()` to discover where it landed and report it however
/// they prefer — this function does not perform any I/O on stdout/stderr.
/// `max_attempts == 0` means "try `addr` exactly once".
/// Test: `auto_port_walks_forward` binds a port, then calls this with the
/// occupied port and confirms a different free port is returned.
pub async fn bind_with_auto_port(addr: SocketAddr, max_attempts: u16) -> Result<TcpListener> {
    use std::io::ErrorKind;
    let mut current = addr;
    for attempt in 0..=max_attempts {
        match TcpListener::bind(current).await {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == ErrorKind::AddrInUse && attempt < max_attempts => {
                let next_port = current.port().saturating_add(1);
                if next_port == 0 {
                    anyhow::bail!("ran out of ports while searching for free slot");
                }
                tracing::warn!("port {} in use, trying {}", current.port(), next_port);
                current.set_port(next_port);
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("could not find free port after {max_attempts} attempts")
}

// ─── Data directory ───────────────────────────────────────────────────────

/// Environment variable name for the data-directory test escape hatch.
///
/// Why: macOS's `dirs::data_dir()` delegates to `NSFileManager`, a native Cocoa
/// API that ignores `HOME` and `XDG_DATA_HOME`. Setting `HOME` in a test process
/// does **not** redirect `dirs::data_dir()` on macOS, making path isolation
/// impossible without a separate bypass. This constant names that bypass.
///
/// What: When `TRUSTY_DATA_DIR_OVERRIDE` is set in the environment,
/// [`resolve_data_dir`] uses its value as the base directory and skips the
/// `dirs::data_dir()` call entirely. The final path is
/// `${TRUSTY_DATA_DIR_OVERRIDE}/<app_name>`, identical in structure to the
/// normal OS-standard path.
///
/// **Intended for tests only.** Do not set this variable in production; it
/// bypasses the OS-standard application-data directory.
///
/// Test: All `resolve_data_dir` tests in this module set this var to a
/// temporary directory so they run identically on macOS, Linux, and Windows.
pub const DATA_DIR_OVERRIDE_ENV: &str = "TRUSTY_DATA_DIR_OVERRIDE";

/// Validate and, if necessary, replace an unsafe data-root path.
///
/// Why: `dirs::data_dir()` and the HOME-relative fallback can return dangerous
/// paths when the daemon environment is degenerate — e.g. `HOME="/"` on Linux
/// yields `/.trusty-memory`, and `XDG_DATA_HOME="/"` yields `/trusty-memory`.
/// Neither of those are literal `/`, but both scatter application data directly
/// under the filesystem root. This pure helper applies post-resolution
/// validation to any candidate path regardless of which branch produced it, and
/// returns a known-safe fallback path if any guard fires. Being infallible
/// (always returns a usable path) avoids adding an error return to the many
/// existing `resolve_data_dir` call sites while still preventing root-scatter.
///
/// What: checks, in order:
/// 1. `candidate` must be absolute. If not, falls back to
///    `$TMPDIR/trusty-<app_name>` and emits `tracing::error!`.
/// 2. `candidate` must not be exactly `/`. If so, falls back and logs error.
/// 3. `candidate`'s parent must not be `/` unless `candidate` is a normal
///    user-data path (guards against e.g. `/.trusty-memory` from `HOME=/`).
///    Paths whose sole parent is `/` receive the safe-temp fallback.
///
/// The safe fallback is `std::env::temp_dir().join(format!("trusty-{app_name}"))`.
/// This lets the daemon still start (and log a clear error) rather than
/// crash-looping when the host environment is misconfigured.
///
/// Test: `sanitize_data_root_rejects_relative`, `sanitize_data_root_rejects_root`,
/// `sanitize_data_root_rejects_bare_root_child`, `sanitize_data_root_passes_valid_path`.
pub fn sanitize_data_root(candidate: PathBuf, app_name: &str) -> PathBuf {
    let safe_fallback = || std::env::temp_dir().join(format!("trusty-{app_name}"));

    if !candidate.is_absolute() {
        tracing::error!(
            path = %candidate.display(),
            app = app_name,
            "resolved data root is not absolute; \
             falling back to temp dir to prevent CWD-relative palace creation. \
             Check HOME and TRUSTY_DATA_DIR_OVERRIDE in the daemon environment."
        );
        return safe_fallback();
    }

    if candidate == Path::new("/") {
        tracing::error!(
            app = app_name,
            "resolved data root is the filesystem root (/); \
             falling back to temp dir. \
             Check HOME and TRUSTY_DATA_DIR_OVERRIDE in the daemon environment."
        );
        return safe_fallback();
    }

    // Reject paths whose immediate parent is "/" — these arise when HOME="/"
    // (Linux) produces `/.trusty-memory` or XDG_DATA_HOME="/" produces
    // `/trusty-memory`. A well-formed user-data path always has at least two
    // non-root ancestors (e.g. `/home/user/...` or `/Users/user/...`).
    if candidate.parent() == Some(Path::new("/")) {
        tracing::error!(
            path = %candidate.display(),
            app = app_name,
            "resolved data root is a direct child of the filesystem root; \
             this usually means HOME or XDG_DATA_HOME is set to '/'. \
             Falling back to temp dir to prevent data scatter under /."
        );
        return safe_fallback();
    }

    candidate
}

/// Resolve `<data_dir>/<app_name>`, creating it if it doesn't exist.
///
/// Why: All trusty-* tools want a per-machine, per-app directory under the
/// OS-standard data dir (`~/Library/Application Support/`, `~/.local/share/`,
/// `%APPDATA%/`). If `dirs::data_dir()` is unavailable (rare — locked-down
/// containers), falls back to `~/.<app_name>` so the tool still works.
///
/// The [`DATA_DIR_OVERRIDE_ENV`] (`TRUSTY_DATA_DIR_OVERRIDE`) environment
/// variable provides a test escape hatch: when set to a *non-empty absolute
/// path*, `dirs::data_dir()` is **never called** and the variable's value is
/// used as the base directory instead. This is necessary because macOS's
/// `dirs::data_dir()` calls `NSFileManager` — a native Cocoa API that
/// resolves the application-support directory through the system rather than
/// through the process environment — so setting `HOME` or `XDG_DATA_HOME` in
/// a test process does not redirect it. `TRUSTY_DATA_DIR_OVERRIDE` is the
/// only reliable cross-platform way to isolate test data paths. **It is
/// intended for tests only; do not set it in production.**
///
/// Safety guards (fixes latent defect tracked in #503 / #504):
/// - An **empty or whitespace-only** override is treated as unset: a
///   `tracing::warn!` is emitted and normal platform-dir resolution proceeds.
///   `std::fs::create_dir_all("")` silently returns `Ok(())` on macOS/Linux,
///   so without this guard the empty-override case would return a relative
///   `"<app_name>"` path that resolves under the daemon's CWD — `/` under
///   launchd — and create palace directories at the filesystem root.
/// - A **non-absolute** (relative) override is rejected with an error, because
///   relative paths are daemon-CWD-dependent and therefore non-deterministic.
/// - A resolved root equal to `/` from an explicit override is rejected with
///   an error.
/// - The FINAL resolved path (from any branch) is validated by
///   [`sanitize_data_root`]: non-absolute, `/`, or a bare `/`-child path
///   (e.g. from `HOME="/"`) falls back to a safe temp location rather than
///   allowing the daemon to create palace dirs at the filesystem root.
///
/// What: returns the absolute path `${base}/<app_name>` (created if absent).
/// Resolution order:
/// 1. `$TRUSTY_DATA_DIR_OVERRIDE/<app_name>` — when the env var is set to a
///    non-empty absolute path that is not `/`.
/// 2. `$(dirs::data_dir())/<app_name>` — normal OS-standard path.
/// 3. `~/.<app_name>` — fallback when `dirs::data_dir()` returns `None`.
///
/// In all cases, the result passes through [`sanitize_data_root`].
///
/// Test: `resolve_data_dir_creates_directory`, `resolve_data_dir_empty_override_uses_platform_dir`,
/// `resolve_data_dir_whitespace_override_uses_platform_dir`,
/// `resolve_data_dir_relative_override_errors`,
/// `resolve_data_dir_root_override_errors`,
/// `sanitize_data_root_rejects_relative`, `sanitize_data_root_rejects_root`,
/// `sanitize_data_root_rejects_bare_root_child`, `sanitize_data_root_passes_valid_path`.
pub fn resolve_data_dir(app_name: &str) -> Result<PathBuf> {
    let base = match std::env::var(DATA_DIR_OVERRIDE_ENV) {
        Ok(raw) if raw.trim().is_empty() => {
            // Empty or whitespace-only override: treat as unset, warn operator.
            tracing::warn!(
                env = DATA_DIR_OVERRIDE_ENV,
                "TRUSTY_DATA_DIR_OVERRIDE is set but empty; ignoring and using \
                 the platform data directory instead. An empty override would \
                 produce a relative path that resolves against the daemon's \
                 working directory (/ under launchd), which is never correct."
            );
            dirs::data_dir()
                .or_else(|| dirs::home_dir().map(|h| h.join(format!(".{app_name}"))))
                .context("could not resolve data directory or home directory")?
        }
        Ok(raw) => {
            let p = PathBuf::from(&raw);
            if !p.is_absolute() {
                anyhow::bail!(
                    "TRUSTY_DATA_DIR_OVERRIDE={raw:?} is a relative path; only \
                     absolute paths are accepted to prevent the data directory \
                     from depending on the daemon's working directory"
                );
            }
            if p == Path::new("/") {
                anyhow::bail!(
                    "TRUSTY_DATA_DIR_OVERRIDE={raw:?} resolves to the filesystem \
                     root (/); refusing to create palace directories directly \
                     under / as that would scatter data across the root filesystem"
                );
            }
            p
        }
        Err(_) => dirs::data_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(format!(".{app_name}"))))
            .context("could not resolve data directory or home directory")?,
    };
    let dir = if base.ends_with(format!(".{app_name}")) {
        base
    } else {
        base.join(app_name)
    };
    // Post-resolution validation: applies to every branch, including the
    // platform-default path. Dangerous paths (non-absolute, /, bare /-child)
    // are replaced with a safe temp fallback rather than returning an error,
    // so misconfigured daemon environments degrade gracefully instead of
    // crash-looping.
    let dir = sanitize_data_root(dir, app_name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create data directory {}", dir.display()))?;
    Ok(dir)
}

// ─── Daemon address file ──────────────────────────────────────────────────

/// Filename used inside each app's data directory to record the daemon's
/// bound HTTP address. Kept as a module-level constant so writers and readers
/// can't drift.
const DAEMON_ADDR_FILENAME: &str = "http_addr";

/// Write the daemon's bound HTTP address to the app's data directory.
///
/// Why: Both trusty-search and trusty-memory persist their bound `host:port`
/// to disk so MCP clients (and follow-up CLI invocations) can discover where
/// the daemon ended up after auto-port-walking. Centralising the path layout
/// keeps the two projects in sync and prevents a third trusty-* daemon from
/// inventing yet another location.
/// What: writes `addr` verbatim (no trailing newline) to
/// `{resolve_data_dir(app_name)}/http_addr`, creating the directory if it
/// doesn't yet exist. Atomic-overwrite semantics aren't required — the file
/// is rewritten on every daemon start.
/// Test: `daemon_addr_round_trips` writes then reads under a stubbed HOME and
/// confirms equality.
pub fn write_daemon_addr(app_name: &str, addr: &str) -> Result<()> {
    let dir = resolve_data_dir(app_name)?;
    let path = dir.join(DAEMON_ADDR_FILENAME);
    std::fs::write(&path, addr).with_context(|| format!("write daemon addr to {}", path.display()))
}

/// Read the daemon's HTTP address from the app's data directory.
///
/// Why: CLI commands and MCP clients need to discover the running daemon's
/// bound port. Returning `Option` lets callers distinguish "daemon never
/// started" (file absent) from "filesystem error" (permission denied, etc.)
/// without resorting to string matching on error messages.
/// What: reads `{resolve_data_dir(app_name)}/http_addr`, trims surrounding
/// whitespace, and returns `Some(addr)`. Returns `Ok(None)` iff the file
/// does not exist; any other I/O error propagates as `Err`.
/// Test: `daemon_addr_round_trips` and `read_daemon_addr_missing_returns_none`.
pub fn read_daemon_addr(app_name: &str) -> Result<Option<String>> {
    let dir = resolve_data_dir(app_name)?;
    let path = dir.join(DAEMON_ADDR_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("read daemon addr from {}", path.display())),
    }
}

// ─── Already-running guard ────────────────────────────────────────────────

/// Issue a short-timeout `GET {base_url}{health_path}` and report whether it
/// returns a 2xx response.
///
/// Why: every trusty-* daemon's "is one already running?" check follows the
/// same shape — probe the recorded address for `/health` with a tight timeout
/// so a dead daemon does not block the start command for the discovery
/// timeout. Lifting the probe into one helper keeps the request/timeout
/// configuration identical across `check_already_running` (file-based) and the
/// trusty-mpm lock-file path (where the URL is derived from a TOML file).
/// What: builds a `reqwest::Client` with a 1 s request timeout, issues the GET,
/// returns `true` only when the response is HTTP 2xx. Any client-builder error
/// or transport failure returns `false`.
/// Test: covered indirectly via `check_already_running_*` and the three daemon
/// integration paths.
pub async fn probe_health(base_url: &str, health_path: &str) -> bool {
    let probe = format!("{base_url}{health_path}");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.get(&probe).send().await, Ok(resp) if resp.status().is_success())
}

/// Probe whether an existing daemon recorded at `addr_file` is healthy and,
/// if so, return its base URL so the caller can refuse to start a duplicate.
///
/// Why: every trusty-* daemon (search, memory, mpm) historically port-walked on
/// boot. Invoking the `start` / `serve` command a second time silently spawned
/// a second instance on the next free port — splitting traffic between two
/// stores, doubling RSS, and confusing every client that resolves the address
/// from disk. The CLI must read the recorded address, ask the live process for
/// `/health`, and if both succeed report "already running" and exit 0 rather
/// than racing a duplicate process against the port walker. A shared helper
/// keeps the three daemons honest — drift here is the bug we are fixing.
/// What: returns `Some("http://<addr>")` only when (a) `addr_file` exists and
/// is readable, (b) its trimmed contents parse as a non-empty `host:port`, and
/// (c) an HTTP `GET http://<addr><health_path>` returns a 2xx within ~1.5 s
/// (1 s request timeout plus tokio scheduling slack). Returns `None` on every
/// other outcome — missing file, unreadable contents, dead address, non-2xx
/// response — so the caller treats that as "no live daemon, proceed".
/// Side-effect (stale-file cleanup): when the file exists but the health probe
/// fails (or the file is empty / malformed), the function best-effort deletes
/// it via `std::fs::remove_file` so the next caller does not chase the same
/// dead address. A delete failure is intentionally ignored.
/// Test: `check_already_running_returns_none_when_file_missing`,
/// `check_already_running_returns_none_when_file_empty`,
/// `check_already_running_returns_none_when_address_dead`,
/// `check_already_running_returns_url_when_health_ok`.
pub async fn check_already_running(addr_file: &Path, health_path: &str) -> Option<String> {
    let raw = match std::fs::read_to_string(addr_file) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let addr = raw.trim();
    if addr.is_empty() {
        // Empty / whitespace-only file is treated as stale — best-effort delete.
        let _ = std::fs::remove_file(addr_file);
        return None;
    }
    let url = format!("http://{addr}");
    if probe_health(&url, health_path).await {
        Some(url)
    } else {
        // Stale file pointing at a dead address. Clear it so the next start
        // attempt is not blocked by a probe against the dead URL.
        let _ = std::fs::remove_file(addr_file);
        None
    }
}

// ─── CLI initialisation ───────────────────────────────────────────────────

/// Initialise the global tracing subscriber.
///
/// Why: Every trusty-* binary wants the same verbosity ladder and the same
/// `RUST_LOG` override semantics. Defining it once removes the boilerplate
/// from every `main.rs`.
/// What: `verbose_count` maps `0 → warn`, `1 → info`, `2 → debug`, `3+ →
/// trace`. If `RUST_LOG` is set in the environment it wins. Logs go to
/// stderr so stdout stays clean for MCP JSON-RPC.
/// Test: side-effecting (global subscriber) — covered by integration with
/// `cargo run -- -v status` in downstream crates.
pub fn init_tracing(verbose_count: u8) {
    let default_filter = match verbose_count {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    // try_init so callers that pre-install a subscriber don't panic.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// Initialise the global tracing subscriber and capture events into a
/// [`log_buffer::LogBuffer`] so the daemon can serve recent logs over HTTP.
///
/// Why: daemons expose `GET /logs/tail`, which needs an in-memory ring of
/// recent log lines. Routing capture through the subscriber means every
/// existing `tracing::info!` / `warn!` call site is mirrored automatically —
/// no second logging API to keep in sync. The stderr `fmt` layer is retained
/// so operators still see live logs in the terminal / launchd log file.
/// What: builds a `tracing_subscriber::registry` with two layers — the
/// standard stderr `fmt` layer (same verbosity ladder + `RUST_LOG` override
/// as [`init_tracing`]) and a [`log_buffer::LogBufferLayer`] feeding the
/// returned [`log_buffer::LogBuffer`]. Uses `try_init`, so a process that has
/// already installed a subscriber keeps it; the returned buffer is still
/// valid (just empty) in that case.
/// Test: `cargo test -p trusty-common log_buffer` covers the layer; the
/// daemon `/logs/tail` integration tests cover the wired path end-to-end.
#[must_use]
pub fn init_tracing_with_buffer(verbose_count: u8, capacity: usize) -> log_buffer::LogBuffer {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let default_filter = match verbose_count {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    // Stderr filter follows the same verbosity ladder + `RUST_LOG` override as
    // `init_tracing` so terminal output stays compact at the operator's chosen
    // level.
    let stderr_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));

    // The log-buffer layer must capture activity even when the stderr filter
    // is set to `warn` (the default for `trusty-search start` without `-v`).
    // Operators reading `/logs/tail` expect to see info-level lifecycle events
    // (file-watcher reindexes, startup scans). Without a separate filter the
    // global stderr filter would suppress them before they reach the buffer.
    // `RUST_LOG_BUFFER` lets ops widen or narrow the buffer independently of
    // stderr; the default of `info` matches the activity feed's intent.
    let buffer_filter = tracing_subscriber::EnvFilter::try_from_env("RUST_LOG_BUFFER")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let buffer = log_buffer::LogBuffer::new(capacity);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_filter(stderr_filter);
    let buf_layer = log_buffer::LogBufferLayer::new(buffer.clone()).with_filter(buffer_filter);
    // try_init so callers that pre-install a subscriber don't panic — the
    // returned buffer simply stays empty in that (rare) case.
    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(buf_layer)
        .try_init();
    buffer
}

/// Initialise the global tracing subscriber with a [`log_buffer::LogBuffer`]
/// **and** a [`error_capture::BugCaptureLayer`] composed in one `try_init` call.
///
/// Why: `tracing_subscriber::registry().try_init()` can only succeed once per
///      process. Callers that need both the HTTP log-tail buffer (issue #35)
///      and Phase 1 bug capture must compose all three layers in a single call;
///      two separate `try_init` calls would leave the second one silently ignored.
///      This helper is the canonical entry-point for daemon binaries that want
///      both features wired together at startup.
/// What: builds an `EnvFilter`-gated stderr `fmt` layer, an info-level
///      `LogBufferLayer`, and a `BugCaptureLayer` for `app_name`/`crate_version`;
///      installs them together via `try_init`. Returns `(LogBuffer, ErrorStore)`
///      so the caller can stash both handles in the daemon's `AppState`.
///      All capture is to a JSONL file under `<dirs::data_dir()>/<app_name>/`
///      and an in-memory ring — nothing is written to stdout, so this is
///      MCP-safe. Honours `TRUSTY_NO_BUG_CAPTURE` for opt-out.
/// Test: `cargo test -p trusty-common --features bug-capture -- init_tracing_with_capture`.
#[cfg(feature = "bug-capture")]
#[must_use]
pub fn init_tracing_with_buffer_and_capture(
    verbose_count: u8,
    capacity: usize,
    app_name: &str,
    crate_version: impl Into<String>,
) -> (log_buffer::LogBuffer, error_capture::ErrorStore) {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let default_filter = match verbose_count {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let stderr_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let buffer_filter = tracing_subscriber::EnvFilter::try_from_env("RUST_LOG_BUFFER")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let buffer = log_buffer::LogBuffer::new(capacity);
    let (capture_layer, store) = error_capture::bug_capture_layer(
        app_name,
        error_capture::DEFAULT_CAPTURE_CAPACITY,
        crate_version,
    );

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_filter(stderr_filter);
    let buf_layer = log_buffer::LogBufferLayer::new(buffer.clone()).with_filter(buffer_filter);
    // All three layers are composed in one try_init so subsequent try_init
    // calls from other code paths become no-ops and do not race with ours.
    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(buf_layer)
        .with(capture_layer)
        .try_init();
    (buffer, store)
}

/// Disable coloured terminal output when requested or when stdout is not a TTY.
///
/// Why: Pipe-friendly output is mandatory for scripting (`trusty-search list
/// | jq …`). `NO_COLOR` / `TERM=dumb` are the canonical signals; passing
/// `--no-color` should override too.
/// What: calls `colored::control::set_override(false)` when the caller asks
/// for it or when the standard heuristics indicate no colour.
/// Test: side-effecting global; trivially covered by manual `NO_COLOR=1 cargo
/// run -- list`.
pub fn maybe_disable_color(no_color: bool) {
    let env_says_no =
        std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").as_deref() == Ok("dumb");
    if no_color || env_says_no {
        colored::control::set_override(false);
    }
}

// ─── OpenRouter ───────────────────────────────────────────────────────────

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const HTTP_REFERER: &str = "https://github.com/bobmatnyc/trusty-common";
const X_TITLE: &str = "trusty-common";
const OPENROUTER_CONNECT_TIMEOUT_SECS: u64 = 10;
const OPENROUTER_REQUEST_TIMEOUT_SECS: u64 = 120; // chat completions can take 60–90s

/// OpenAI-compatible chat message.
///
/// Why: Both trusty-memory's `chat` subcommand and trusty-search's `/chat`
/// endpoint speak the OpenRouter format. Sharing the struct keeps them in
/// step (and lets callers compose chat histories without re-defining types).
/// Tool-use additions (`tool_call_id`, `tool_calls`) follow the OpenAI
/// function-calling shape: assistant messages set `tool_calls` when the model
/// requests tool invocations; subsequent `role: "tool"` messages echo the
/// matching `tool_call_id` with the tool's result in `content`.
/// What: `role` is one of `"system" | "user" | "assistant" | "tool"`.
/// `content` is the message text. `tool_call_id` is the id of the tool call
/// this message is replying to (only set when `role == "tool"`). `tool_calls`
/// is the raw OpenAI `tool_calls` array on an assistant message that asked
/// to invoke tools — kept as `serde_json::Value` so we don't drop any fields
/// the upstream may add.
/// Test: serde round-trip in `chat_message_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: String,
}

/// Send a chat completion request to OpenRouter and return the assistant's
/// message content.
///
/// Why: A one-shot, non-streaming chat call is the common-case helper — used
/// by trusty-memory's `chat` CLI and trusty-search's `/chat` endpoint.
/// What: POSTs `{model, messages, stream: false}` to OpenRouter with bearer
/// auth, decodes the response, and returns `choices[0].message.content`.
/// Errors propagate as anyhow with HTTP status context.
/// Test: error paths covered by `openrouter_propagates_http_errors` (uses a
/// blackhole base URL — no real call).
#[deprecated(since = "0.3.1", note = "Use OpenRouterProvider::chat_stream instead")]
pub async fn openrouter_chat(
    api_key: &str,
    model: &str,
    messages: Vec<ChatMessage>,
) -> Result<String> {
    if api_key.is_empty() {
        return Err(anyhow!("openrouter api key is empty"));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(
            OPENROUTER_CONNECT_TIMEOUT_SECS,
        ))
        .timeout(std::time::Duration::from_secs(
            OPENROUTER_REQUEST_TIMEOUT_SECS,
        ))
        .build()
        .context("build reqwest client for openrouter_chat")?;
    let body = ChatRequest {
        model,
        messages: &messages,
        stream: false,
    };
    let resp = client
        .post(OPENROUTER_URL)
        .bearer_auth(api_key)
        .header("HTTP-Referer", HTTP_REFERER)
        .header("X-Title", X_TITLE)
        .json(&body)
        .send()
        .await
        .context("POST openrouter chat completions")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("openrouter HTTP {status}: {text}"));
    }
    let payload: ChatResponse = resp.json().await.context("decode openrouter response")?;
    payload
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| anyhow!("openrouter returned no choices"))
}

/// Stream chat-completion deltas from OpenRouter through a tokio mpsc channel.
///
/// Why: `chat` UIs want incremental tokens for a responsive feel; the
/// streaming endpoint emits SSE `data:` frames with delta content.
/// What: POSTs the request with `stream: true`, parses each SSE `data:` line
/// as a JSON object, extracts `choices[0].delta.content`, and sends each
/// non-empty chunk to `tx`. The function returns when the stream terminates
/// (either by `[DONE]` sentinel or by upstream EOF).
/// Test: integration-only (no offline mock); covered manually via the
/// trusty-search `/chat` endpoint that re-uses this helper.
#[deprecated(since = "0.3.1", note = "Use OpenRouterProvider::chat_stream instead")]
pub async fn openrouter_chat_stream(
    api_key: &str,
    model: &str,
    messages: Vec<ChatMessage>,
    tx: tokio::sync::mpsc::Sender<String>,
) -> Result<()> {
    use futures_util::StreamExt;

    if api_key.is_empty() {
        return Err(anyhow!("openrouter api key is empty"));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(
            OPENROUTER_CONNECT_TIMEOUT_SECS,
        ))
        .timeout(std::time::Duration::from_secs(
            OPENROUTER_REQUEST_TIMEOUT_SECS,
        ))
        .build()
        .context("build reqwest client for openrouter_chat_stream")?;
    let body = ChatRequest {
        model,
        messages: &messages,
        stream: true,
    };
    let resp = client
        .post(OPENROUTER_URL)
        .bearer_auth(api_key)
        .header("HTTP-Referer", HTTP_REFERER)
        .header("X-Title", X_TITLE)
        .json(&body)
        .send()
        .await
        .context("POST openrouter chat completions (stream)")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("openrouter HTTP {status}: {text}"));
    }

    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("read openrouter stream chunk")?;
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        buf.push_str(text);

        while let Some(idx) = buf.find('\n') {
            let line: String = buf.drain(..=idx).collect();
            let line = line.trim();
            let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(delta) = v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                && !delta.is_empty()
                && tx.send(delta.to_string()).await.is_err()
            {
                // Receiver dropped — caller has lost interest.
                return Ok(());
            }
        }
    }
    Ok(())
}

// ─── Misc helpers ─────────────────────────────────────────────────────────

/// Check whether a path exists and is a directory.
///
/// Why: tiny but commonly-needed shim — clearer at call sites than
/// `path.exists() && path.is_dir()`.
/// What: returns `true` iff the path exists and metadata reports a directory.
/// Test: `is_dir_recognises_directories`.
pub fn is_dir(path: &Path) -> bool {
    path.metadata().map(|m| m.is_dir()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises tests that mutate the `TRUSTY_DATA_DIR_OVERRIDE` env var so
    /// they don't race when `cargo test` runs them in parallel threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn auto_port_walks_forward() {
        // Bind to an OS-chosen port, then ask auto-port to start there.
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let next = bind_with_auto_port(addr, 8).await.unwrap();
        let got = next.local_addr().unwrap().port();
        assert_ne!(got, port, "expected walk-forward to a different port");
    }

    #[tokio::test]
    async fn auto_port_zero_attempts_still_binds_free() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let l = bind_with_auto_port(addr, 0).await.unwrap();
        assert!(l.local_addr().unwrap().port() > 0);
    }

    #[test]
    fn resolve_data_dir_creates_directory() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Use the override env var so we deterministically control the base
        // directory cross-platform (macOS's dirs::data_dir ignores HOME).
        let tmp = tempfile_like_dir();
        // SAFETY: env mutation; tests in this module run serially via
        // #[test] threading isolation only when MUTEX-guarded — we accept
        // the residual risk since the override var is unique to these tests.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let dir = resolve_data_dir("trusty-test-xyz").unwrap();
        assert!(
            dir.exists(),
            "data dir should be created at {}",
            dir.display()
        );
        assert!(dir.is_dir());
        assert!(
            dir.starts_with(&tmp),
            "data dir {} should live under override {}",
            dir.display(),
            tmp.display()
        );
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
    }

    /// Why: guard introduced in #503 — an empty override must not produce a
    /// relative path that resolves under the daemon CWD.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="" and asserts the result is an
    /// absolute path that does NOT start with "".
    /// Test: this function.
    #[test]
    fn resolve_data_dir_empty_override_uses_platform_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation; serialised by ENV_LOCK.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "");
        }
        let result = resolve_data_dir("trusty-test-empty-override");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let dir = result.expect("empty override should fall back to platform dir");
        assert!(
            dir.is_absolute(),
            "resolved dir should be absolute, got {}",
            dir.display()
        );
        assert_ne!(
            dir,
            std::path::PathBuf::from("/"),
            "resolved dir must not be filesystem root"
        );
    }

    /// Why: whitespace-only overrides are as dangerous as empty ones.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="   " and asserts an absolute fallback.
    /// Test: this function.
    #[test]
    fn resolve_data_dir_whitespace_override_uses_platform_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation; serialised by ENV_LOCK.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "   ");
        }
        let result = resolve_data_dir("trusty-test-ws-override");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let dir = result.expect("whitespace override should fall back to platform dir");
        assert!(dir.is_absolute(), "resolved dir should be absolute");
    }

    /// Why: a relative override is non-deterministic (depends on daemon CWD).
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="relative/path" and asserts an error.
    /// Test: this function.
    #[test]
    fn resolve_data_dir_relative_override_errors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation; serialised by ENV_LOCK.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "relative/path");
        }
        let result = resolve_data_dir("trusty-test-relative");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            result.is_err(),
            "relative override should be rejected, but got Ok({})",
            result.unwrap().display()
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("relative"),
            "error should mention 'relative', got: {msg}"
        );
    }

    /// Why: override set to "/" would create palace dirs directly under the
    /// filesystem root, scattering data.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="/" and asserts an error.
    /// Test: this function.
    #[test]
    fn resolve_data_dir_root_override_errors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation; serialised by ENV_LOCK.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "/");
        }
        let result = resolve_data_dir("trusty-test-root");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            result.is_err(),
            "root '/' override should be rejected, but got Ok({})",
            result.unwrap().display()
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains('/'),
            "error should mention the path, got: {msg}"
        );
    }

    /// Why: confirms that a valid absolute override is still honoured, so the
    /// guard changes do not break the test-isolation use-case.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE to a tempdir and asserts the resolved
    /// path lives under it.
    /// Test: this function (complements resolve_data_dir_creates_directory).
    #[test]
    fn resolve_data_dir_valid_absolute_override_is_honoured() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        // SAFETY: env mutation; serialised by ENV_LOCK.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let result = resolve_data_dir("trusty-test-abs-override");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let dir = result.expect("valid absolute override should succeed");
        assert!(
            dir.starts_with(&tmp),
            "resolved dir {} should be under override {}",
            dir.display(),
            tmp.display()
        );
        assert!(dir.is_absolute(), "resolved dir must be absolute");
    }

    /// Why: `sanitize_data_root` must catch a relative candidate (e.g. produced
    /// by a code path that forgot to prepend a base dir) and replace it with the
    /// safe temp fallback.
    /// What: passes `PathBuf::from("relative/path")` and asserts the returned
    /// path is absolute, lives under `temp_dir()`, and starts with "trusty-".
    /// Test: this function.
    #[test]
    fn sanitize_data_root_rejects_relative() {
        let result = sanitize_data_root(PathBuf::from("relative/path"), "myapp");
        assert!(result.is_absolute(), "fallback must be absolute");
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("trusty-"),
            "fallback dir name should start with trusty-, got {name}"
        );
    }

    /// Why: a candidate equal to "/" must be replaced — palace dirs would be
    /// created directly at the filesystem root.
    /// What: passes `PathBuf::from("/")` and asserts a safe fallback is returned.
    /// Test: this function.
    #[test]
    fn sanitize_data_root_rejects_root() {
        let result = sanitize_data_root(PathBuf::from("/"), "myapp");
        assert!(result.is_absolute(), "fallback must be absolute");
        assert_ne!(result, PathBuf::from("/"), "must not still be /");
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("trusty-"),
            "fallback should start with trusty-"
        );
    }

    /// Why: `HOME="/"` on Linux yields `/.trusty-memory`; `XDG_DATA_HOME="/"`
    /// yields `/trusty-memory`. These are direct children of `/` and are just
    /// as dangerous as `/` itself for data-scattering.
    /// What: passes `/bare-child` (parent == "/") and asserts a safe fallback.
    /// Test: this function.
    #[test]
    fn sanitize_data_root_rejects_bare_root_child() {
        let result = sanitize_data_root(PathBuf::from("/bare-child"), "myapp");
        assert!(result.is_absolute(), "fallback must be absolute");
        assert_ne!(
            result,
            PathBuf::from("/bare-child"),
            "bare root-child must be replaced"
        );
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("trusty-"),
            "fallback should start with trusty-"
        );
    }

    /// Why: valid paths (two or more non-root ancestors) must pass through
    /// unchanged — we must not accidentally redirect legitimate data dirs.
    /// What: passes a tempdir-based path and asserts it is returned unmodified.
    /// Test: this function.
    #[test]
    fn sanitize_data_root_passes_valid_path() {
        let tmp = tempfile_like_dir();
        let candidate = tmp.join("trusty-myapp");
        let result = sanitize_data_root(candidate.clone(), "myapp");
        assert_eq!(
            result, candidate,
            "valid absolute path should be returned unchanged"
        );
    }

    #[test]
    fn daemon_addr_round_trips() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        // SAFETY: env mutation; see note in resolve_data_dir_creates_directory.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        write_daemon_addr(&app, "127.0.0.1:12345").unwrap();
        let got = read_daemon_addr(&app).unwrap();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(got.as_deref(), Some("127.0.0.1:12345"));
    }

    #[test]
    fn read_daemon_addr_missing_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        // SAFETY: env mutation; see note in resolve_data_dir_creates_directory.
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let got = read_daemon_addr(&app).unwrap();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(got.is_none(), "expected None when file absent, got {got:?}");
    }

    #[test]
    fn is_dir_recognises_directories() {
        let tmp = tempfile_like_dir();
        assert!(is_dir(&tmp));
        assert!(!is_dir(&tmp.join("nope")));
    }

    #[test]
    fn chat_message_round_trips() {
        let m = ChatMessage {
            role: "user".into(),
            content: "hello".into(),
            tool_call_id: None,
            tool_calls: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: ChatMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(back.role, "user");
        assert_eq!(back.content, "hello");
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn openrouter_chat_rejects_empty_key() {
        let err = openrouter_chat("", "x", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("api key"));
    }

    #[tokio::test]
    async fn check_already_running_returns_none_when_file_missing() {
        // Why: a fresh machine (no prior daemon) must skip the probe entirely
        // and let the caller proceed with normal startup.
        let tmp = tempfile_like_dir();
        let missing = tmp.join("does-not-exist");
        let got = check_already_running(&missing, "/health").await;
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn check_already_running_returns_none_when_file_empty() {
        // Why: a half-written / truncated address file should be treated as
        // "no daemon" and the stale file cleared so the next start does not
        // see it again.
        let tmp = tempfile_like_dir();
        let path = tmp.join("http_addr");
        std::fs::write(&path, "   \n  ").unwrap();
        let got = check_already_running(&path, "/health").await;
        assert!(got.is_none());
        assert!(
            !path.exists(),
            "empty address file should be cleaned up by check_already_running"
        );
    }

    #[tokio::test]
    async fn check_already_running_returns_none_when_address_dead() {
        // Why: a stale address (daemon previously crashed) must NOT block a
        // fresh start; the helper must probe, see no listener, clear the file,
        // and report "no daemon".
        let tmp = tempfile_like_dir();
        let path = tmp.join("http_addr");
        // Reserved unbound port — TCP connect will fail fast.
        std::fs::write(&path, "127.0.0.1:1\n").unwrap();
        let got = check_already_running(&path, "/health").await;
        assert!(got.is_none(), "dead address should map to None");
        assert!(
            !path.exists(),
            "stale address file should be cleaned up by check_already_running"
        );
    }

    #[tokio::test]
    async fn check_already_running_returns_url_when_health_ok() {
        // Why: positive control — when a daemon really is listening and
        // returns 2xx on the health path, the helper must report its URL so
        // the caller can refuse to spawn a duplicate.
        // What: spin up a one-shot mini HTTP server on an ephemeral port that
        // answers `GET /health → 200`, write the address to the file, and
        // confirm the helper returns the expected URL.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
                let _ = sock.shutdown().await;
            }
        });

        let tmp = tempfile_like_dir();
        let path = tmp.join("http_addr");
        std::fs::write(&path, format!("{local}\n")).unwrap();

        let got = check_already_running(&path, "/health").await;
        assert_eq!(got.as_deref(), Some(format!("http://{local}").as_str()));
        assert!(
            path.exists(),
            "address file must be preserved when the daemon is healthy"
        );
        let _ = server.await;
    }

    // Test-only helper: makes a unique scratch dir without pulling in tempfile
    // as a dev-dep (keeps the dependency surface minimal).
    fn tempfile_like_dir() -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-common-test-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
