//! HTTP/SSE serving helpers for the trusty-memory daemon.
//!
//! Why: `lib.rs` was approaching the 500 SLOC production file cap (issue #607).
//! The axum-gated HTTP-serving functions are the most self-contained group —
//! they all depend on `AppState` from the crate root but have no upward
//! dependencies — so extracting them here keeps `lib.rs` under the cap while
//! preserving all public API.
//! What: exports `run_http`, `run_http_dynamic`, `run_http_on`,
//! `write_http_addr_file`, `dotfile_http_addr_path`, and the internal
//! `sse_handler`. Port constants and the `spawn_*` helpers live here too.
//! Test: `cargo test -p trusty-memory --features axum-server` exercises the
//! full axum router via `web::tests`; `lib_tests` covers the addr-file helpers.

use anyhow::Result;
use std::net::SocketAddr;
use std::path::Path;
use tracing::info;

use crate::AppState;

/// Write the bound `host:port` to the given path atomically.
///
/// Why: clients must read the file mid-write without observing a partial
/// value. Writing to a `.tmp` sibling and renaming over the target gives
/// POSIX atomicity, matching the trusty-search implementation.
/// What: creates the parent directory if missing; writes `addr` followed by a
/// trailing newline (avoids the "no newline at end of file" warnings from
/// `cat`); renames `.tmp` → `http_addr`. Best-effort: I/O errors are
/// returned to the caller so `run_http_on` can log without panicking.
/// Test: `http_addr_file_round_trip_via_helpers` in `lib_tests`.
pub(crate) fn write_http_addr_file(path: &Path, addr: &SocketAddr) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("addr.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "{addr}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Return `$HOME/.trusty-memory/http_addr`, or `None` when home is
/// unavailable or a data-dir override is active.
///
/// Why (issue #498): external tooling such as claude-mpm's
/// `migrate_trusty_autodetect` reads `~/.trusty-memory/http_addr` to find
/// the running daemon's port. On macOS, `resolve_data_dir("trusty-memory")`
/// returns `~/Library/Application Support/trusty-memory/`, not
/// `~/.trusty-memory/`, so the daemon was writing to the OS-standard location
/// while readers expected the dotfile location. Writing to both locations
/// keeps every reader happy regardless of which convention they follow.
///
/// Fix #880: returns `None` when `TRUSTY_DATA_DIR_OVERRIDE` is active so an
/// isolated instance (test rig, CI, parallel run) never overwrites the real
/// production daemon's discovery dotfile.
///
/// What: returns `$HOME/.trusty-memory/http_addr` in the default (production)
/// case, or `None` when `dirs::home_dir()` is unavailable OR when a data-dir
/// override is active (see `crate::is_data_dir_override_active`).
/// Test: `dotfile_http_addr_path_uses_home_dir`,
///       `dotfile_suppressed_when_override_active` in `lib_tests`.
pub(crate) fn dotfile_http_addr_path() -> Option<std::path::PathBuf> {
    if crate::is_data_dir_override_active() {
        return None;
    }
    dirs::home_dir().map(|h| h.join(".trusty-memory").join("http_addr"))
}

/// Interval between aggregate-status snapshot emits on the SSE bus.
///
/// Why (issue #228): mutations used to fire `StatusChanged` synchronously on
/// the write path, which forced an O(N palaces) sum of drawer / vector / KG
/// counts on every `memory_remember`. Coalescing into a fixed-cadence ticker
/// lets dashboards stay current (a 30 s lag is invisible at human scale)
/// while keeping the write path free of aggregate work.
/// What: 30 seconds — short enough that the operator UI doesn't feel stale
/// between manual writes, long enough that the recompute cost (in-memory
/// registry walk plus the redb `count_active_triples` per palace) is a
/// rounding error on the daemon's CPU budget.
/// Test: covered indirectly — the math has not changed, only the cadence.
#[allow(dead_code)]
const STATUS_EVENT_TICK_SECS: u64 = 30;

/// Spawn a background ticker that recomputes the `data_root` disk footprint
/// every 10 seconds and stores it in `state.disk_bytes` (issue #35).
///
/// Why: `GET /health` reports `disk_bytes`. Walking the data directory on
/// every health request would turn a frequent health poll into unbounded
/// recursive I/O. Computing it off the request path on a fixed cadence keeps
/// `/health` cheap and bounds the staleness to ~10 s — fine for an
/// at-a-glance footprint figure.
/// What: spawns a detached tokio task. `AppState` is cheap to `Clone` (all
/// `Arc` fields), so the task holds a full clone; the daemon process lives
/// for the lifetime of the server anyway, so no `Weak` downgrade is needed.
/// Each tick runs the blocking directory walk on `spawn_blocking` so it never
/// stalls the async runtime, then stores the byte total atomically.
/// Test: `health_endpoint_includes_resource_fields` asserts the field shape;
/// the ticker cadence is not unit-tested (timing-dependent).
pub(crate) fn spawn_disk_size_ticker(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let dir = state.data_root.clone();
            let bytes = tokio::task::spawn_blocking(move || {
                trusty_common::sys_metrics::dir_size_bytes(&dir)
            })
            .await
            .unwrap_or(0);
            state
                .disk_bytes
                .store(bytes, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// Spawn a background ticker that emits `DaemonEvent::StatusChanged` every
/// [`STATUS_EVENT_TICK_SECS`] seconds (issue #228).
///
/// Why: replaces the per-write `state.emit(self.aggregate_status_event())`
/// call sites that used to recompute the aggregate every time a drawer was
/// created or deleted. Walking N palaces on every write blocks the async
/// runtime; coalescing the emit onto a ticker keeps dashboards up-to-date
/// without that cost.
/// What: spawns a detached tokio task that holds a full `AppState` clone
/// (cheap — every field is `Arc`-backed) and ticks every
/// [`STATUS_EVENT_TICK_SECS`] seconds. Each tick computes
/// `MemoryService::aggregate_status_event` (which now iterates the
/// in-memory registry, not disk) and broadcasts it via `state.emit`. If
/// no SSE subscribers are connected the broadcast `send` is a cheap no-op,
/// so the ticker imposes no cost when nobody is listening.
/// Test: not unit-tested (timing-dependent fire-and-forget); the underlying
/// `aggregate_status_event` math is exercised by the existing
/// `status_endpoint_returns_payload` path.
#[allow(dead_code)]
pub(crate) fn spawn_status_event_ticker(state: AppState) {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(STATUS_EVENT_TICK_SECS));
        loop {
            interval.tick().await;
            let event = crate::service::MemoryService::new(state.clone()).aggregate_status_event();
            state.emit(event);
        }
    });
}

/// Live SSE event stream — pushes `DaemonEvent` frames to dashboard clients.
///
/// Why: The dashboard subscribes once and reacts to live pushes (palace
/// created, drawer added/deleted, dream completed, status changed) instead of
/// polling `/api/v1/*` endpoints.
/// What: Subscribes to `state.events`, emits an initial `connected` frame,
/// then forwards every `DaemonEvent` as `data: <json>\n\n`. Lagged
/// subscribers receive a `lag` frame indicating skipped events; channel
/// closure ends the stream.
/// Test: `web::tests::sse_stream_emits_palace_created` (covers subscribe +
/// emit + receive); manual: `curl -N http://.../sse`.
pub(crate) async fn sse_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl axum::response::IntoResponse {
    use futures::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let rx = state.events.subscribe();
    let initial = futures::stream::once(async {
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(
            "data: {\"type\":\"connected\"}\n\n",
        ))
    });
    let events = BroadcastStream::new(rx).map(|res| {
        let frame = match res {
            Ok(event) => match serde_json::to_string(&event) {
                Ok(json) => format!("data: {json}\n\n"),
                Err(e) => format!("data: {{\"type\":\"error\",\"message\":\"{e}\"}}\n\n"),
            },
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                format!("data: {{\"type\":\"lag\",\"skipped\":{n}}}\n\n")
            }
        };
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(frame))
    });
    let stream = initial.chain(events);

    axum::response::Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(axum::body::Body::from_stream(stream))
        .expect("valid SSE response") // Why: invariant — SSE headers are compile-time constants; builder cannot fail
}

/// Run the optional HTTP/SSE + web admin server on a pre-bound listener.
///
/// Why: A long-running daemon mode lets non-stdio clients (browsers, curl,
/// future remote agents) hit `/health`, the `/api/v1/*` REST surface, and the
/// embedded admin SPA. The Unix-domain-socket transport and the
/// `trusty-memory-mcp-bridge` binary were removed in PR3 of the #914
/// stdio-cutover epic; the canonical MCP integration is now
/// `trusty-memory serve --stdio` (PR1 #919).
/// What: axum router built from `web::router()` plus a `/sse` stub for the
/// existing MCP-over-SSE clients. Caller provides a pre-bound listener so
/// port auto-detection lives at the call site. Before accepting connections
/// the daemon stamps the bound `host:port` onto `AppState.bound_addr` and
/// writes `~/.trusty-memory/http_addr` so clients can discover the live port.
/// On shutdown the file is removed best-effort (a stale file with the wrong
/// port is worse than a missing one).
/// Test: `cargo test -p trusty-memory web::tests` exercises the router shape;
/// manual: `curl http://127.0.0.1:<port>/health` returns `ok` with `addr`.
pub async fn run_http_on(state: AppState, listener: tokio::net::TcpListener) -> Result<()> {
    use axum::routing::get;

    // Issue #35: recompute the `data_root` disk footprint every 10 s on a
    // background task so `GET /health` reports `disk_bytes` without doing a
    // recursive directory walk on the request path.
    spawn_disk_size_ticker(state.clone());

    // Issue #228: emit aggregate `StatusChanged` on a fixed cadence rather
    // than on every drawer write. The previous design called
    // `aggregate_status_event` from every `memory_remember` / `memory_note`
    // / `memory_forget` (and the matching HTTP handlers), each of which
    // walked the data root + opened every palace handle. Coalescing the
    // emit to a 30 s ticker keeps dashboards live without dragging an
    // O(N palaces) recompute onto the write hot path.
    spawn_status_event_ticker(state.clone());

    // Capture and advertise the bound address BEFORE serving so the first
    // request handler — and the http_addr discovery file — see the real port
    // even if `local_addr()` would otherwise be racy.
    let local = listener.local_addr().ok();
    let (written_path, written_dotfile_path) = if let Some(a) = local {
        // Stash on state for handlers (e.g. /health) to surface.
        let _ = state.bound_addr.set(a);
        info!("HTTP server listening on http://{a}");
        eprintln!("HTTP server listening on http://{a}");
        // Primary: write to the OS-standard data dir (`~/Library/Application
        // Support/trusty-memory/http_addr` on macOS, `~/.local/share/…` on
        // Linux). This is what `trusty_common::read_daemon_addr` reads.
        // Best-effort: a missing $HOME or read-only fs is non-fatal.
        let primary = match crate::http_addr_path() {
            Some(p) => match write_http_addr_file(&p, &a) {
                Ok(()) => {
                    info!("wrote daemon address to {}", p.display());
                    Some(p)
                }
                Err(e) => {
                    tracing::warn!("could not write {}: {e}", p.display());
                    None
                }
            },
            None => {
                tracing::warn!("no $HOME — skipping http_addr discovery file");
                None
            }
        };
        // Issue #498: also write to `~/.trusty-memory/http_addr` so external
        // tools (e.g. claude-mpm's `migrate_trusty_autodetect`) that read the
        // dotfile path can discover the daemon's port. On macOS the OS-standard
        // path differs from the dotfile path; writing both ensures consumers
        // using either convention find the file. Best-effort: failures are
        // logged but do not block startup.
        let dotfile = match dotfile_http_addr_path() {
            Some(p) => match write_http_addr_file(&p, &a) {
                Ok(()) => {
                    info!("wrote daemon address to dotfile {}", p.display());
                    Some(p)
                }
                Err(e) => {
                    tracing::warn!("could not write dotfile {}: {e}", p.display());
                    None
                }
            },
            None => None,
        };
        (primary, dotfile)
    } else {
        (None, None)
    };

    // Keep a handle to the BM25 supervisor (if any) so we can call
    // `shutdown()` on the exit path. Cloning here is cheap (`Arc`) and
    // detaches the lifetime of the supervisor from the `state` move into
    // the router below.
    let bm25_supervisor = state.bm25_supervisor.clone();

    let app = crate::web::router()
        .route("/sse", get(sse_handler))
        .with_state(state);

    // Why (issue #534): bare axum::serve exits only on an internal error; SIGTERM
    // (launchctl bootout) would kill the process before the cleanup below had a
    // chance to run, leaving stale addr/socket files behind and dropping any
    // in-flight request without draining. `with_graceful_shutdown` installs a
    // SIGTERM + SIGINT watcher; when either fires axum stops accepting new
    // connections, drains active requests, then returns here so cleanup runs.
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(trusty_common::shutdown_signal())
        .await;

    // Best-effort cleanup: remove `http_addr` files so stale clients fail fast
    // instead of timing out against a dead port. Remove both the OS-standard
    // path and the dotfile path (#498).
    if let Some(p) = written_path.as_ref() {
        let _ = std::fs::remove_file(p);
    }
    if let Some(p) = written_dotfile_path.as_ref() {
        let _ = std::fs::remove_file(p);
    }

    // Issue #193: gracefully reap every spawned BM25 daemon before the
    // process exits so each one gets a chance to flush its snapshot and
    // unlink its socket. `kill_on_drop=true` on the children would
    // SIGKILL them on Drop anyway, but that skips the daemon's own
    // shutdown sequence and leaves stale sockets behind.
    if let Some(supervisor) = bm25_supervisor {
        supervisor.shutdown().await;
    }

    serve_result?;
    Ok(())
}

/// Convenience: bind `addr` and serve via [`run_http_on`].
///
/// Why: `trusty-memory serve --http <addr>` provides an explicit bind address.
/// What: wraps `tokio::net::TcpListener::bind` + `run_http_on`.
/// Test: integration path via `main.rs` `serve --http` subcommand.
pub async fn run_http(state: AppState, addr: std::net::SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    run_http_on(state, listener).await
}

/// Convenience: bind dynamically (7070..=7079, OS fallback) and serve.
///
/// Why: `trusty-memory serve` with no `--http` flag is the canonical
/// launchd-managed daemon entry point. Dynamic binding lets a stale daemon
/// or a hand-spawned `serve --http 127.0.0.1:7070` coexist without breaking
/// the launchd-managed instance.
/// What: calls [`crate::bind_dynamic_port`] then [`run_http_on`].
/// Test: integration via `trusty-memory serve` + `cat ~/.trusty-memory/http_addr`.
pub async fn run_http_dynamic(state: AppState) -> Result<()> {
    let listener = crate::bind_dynamic_port().await?;
    run_http_on(state, listener).await
}
