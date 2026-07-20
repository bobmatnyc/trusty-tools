//! HTTP service layer for trusty-review — axum router and shared state.
//!
//! Why: wraps the existing review pipeline in a long-lived HTTP daemon so
//! trusty-review can receive GitHub webhooks and respond to on-demand review
//! requests without requiring the caller to spawn a CLI process.
//!
//! What: exports `AppState`, `build_router`, and the `serve` entry-point.
//! All axum handler logic lives in focused sibling files:
//!   - `handlers.rs`  — route handler functions
//!   - `webhook.rs`   — GitHub webhook event parsing + dispatch
//!
//! Test: `cargo test -p trusty-review --features http-server` exercises the
//! router via `tower::ServiceExt::oneshot` without a bound socket.
//!
//! Feature gate: the entire module is compiled only under `http-server`.

pub mod handlers;
pub mod inference_probe;
pub mod webhook;

pub use handlers::AppState;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    Router,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::service::handlers::{handle_health, handle_review, handle_status};
use crate::service::webhook::handle_github_webhook;

/// Default listen port for the trusty-review daemon.
///
/// Why: must be distinct from EVERY sibling daemon's default, not just the
/// ones spec REV-803 originally enumerated. The original default (7880) was
/// discovered live (#2566 review) to collide with trusty-mpm's
/// `DEFAULT_DAEMON_ADDR` (`crates/trusty-mpm/src/core/discovery.rs`) — the
/// `tm` daemon binds `127.0.0.1:7880` on every managed machine, so a launchd
/// agent that runs `trusty-review serve` with no explicit `--port` collided
/// with it and crash-looped (`KeepAlive::Always`, 10s throttle) on install.
/// The follow-up default (7890) was itself found (#2573) to collide with
/// `trusty-embedderd`'s `--http` mode default (`127.0.0.1:7890`,
/// `crates/trusty-embedderd/src/lib.rs`) — not reachable via any
/// `tctl`-managed automation, since the embedder auto-spawn always uses
/// `--stdio`/UDS, so it was a manual dev-run footgun only, but still a real
/// collision for anyone running `trusty-embedderd --http` and
/// `trusty-review serve` on the same host with no explicit `--port`/`--http`.
/// 7891 is verified free against the full known set: 7070 = trusty-memory,
/// 7878 = trusty-search, 7879 = trusty-analyze, 7788 = trusty-console,
/// 7880 = trusty-mpm (`tm`), 7890 = trusty-embedderd (`--http` mode).
/// What: 7891 (superseding the #2566 fix's 7890, itself superseding the
/// REV-803 spec's original 7880).
/// Test: `serve_help_shows_default_port` checks the CLI default;
/// `default_port_does_not_collide_with_known_siblings` pins uniqueness
/// against the full known-port table, which now includes trusty-embedderd.
pub const DEFAULT_PORT: u16 = 7891;

/// Resolve the dotfile discovery path `~/.trusty-review/http_addr`.
///
/// Why: claude-mpm's `_is_present` / autodetect reads `~/.trusty-review/http_addr`
/// to find a running daemon. On macOS `resolve_data_dir` points to
/// `~/Library/Application Support/trusty-review/`, which differs from the dotfile
/// path that external tools expect. Writing both ensures every consumer finds the
/// file regardless of which convention it uses (mirrors trusty-memory issue #498).
/// What: returns `$HOME/.trusty-review/http_addr`, or `None` when `$HOME` is unset.
/// Test: `dotfile_http_addr_path_is_under_home` in the tests module.
pub(crate) fn dotfile_http_addr_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".trusty-review").join("http_addr"))
}

/// Write `addr` to a path, creating parent directories as needed.
///
/// Why: both the OS-standard path and the dotfile path require their parent
/// directory to exist before `fs::write` will succeed. Grouping the mkdir +
/// write avoids repeating the pattern at each call site.
/// What: calls `create_dir_all` on the parent, then `fs::write`. Returns the
/// `std::io::Error` from either step unchanged so callers can log it.
/// Test: exercised indirectly through `serve`'s best-effort write path; direct
/// coverage in `addr_file_write_creates_parent_dir` unit test.
pub(crate) fn write_addr_to_path(path: &std::path::Path, addr: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, addr)
}

/// Build the axum router for trusty-review.
///
/// Why: separating router construction from `serve` lets tests call
/// `build_router(state).oneshot(request)` without binding a socket.
/// What: registers GET /health, GET /status, POST /review, and
/// POST /pr/github/webhook; attaches a tracing layer.
/// Test: `router_builds_without_panic`, `health_returns_ok_json`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/status", get(handle_status))
        .route("/review", post(handle_review))
        .route("/pr/github/webhook", post(handle_github_webhook))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Bind the axum server and run until SIGTERM/SIGINT.
///
/// Why: the `serve` CLI subcommand needs a single async entry-point that
/// builds state, binds the socket, and runs the graceful-shutdown loop
/// following the workspace's connection-safe daemon convention (issue #534).
/// After binding, the actual `host:port` is written to two discovery paths so
/// claude-mpm autodetect and other consumers can find the daemon regardless of
/// which port it ended up on (closes #665). Both writes are best-effort: a
/// failure logs a warning to stderr but never prevents the daemon from starting.
/// What: calls `build_router`, binds `addr`, writes discovery files, logs the
/// listening address to stderr, then awaits `axum::serve` with
/// `trusty_common::shutdown_signal`. On shutdown removes both discovery files.
/// Test: `addr_string_format_is_host_colon_port` unit test; integration via
/// `trusty-review serve` then `cat ~/.trusty-review/http_addr`.
pub async fn serve(state: AppState, addr: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    let addr_str = actual.to_string();

    // Log to stderr — stdout must stay clean (spec REV-722, CLAUDE.md).
    info!("trusty-review listening on http://{actual}");
    eprintln!("trusty-review: listening on http://{actual}");

    // Write OS-standard discovery file (e.g. ~/Library/Application Support/trusty-review/http_addr
    // on macOS, ~/.local/share/trusty-review/http_addr on Linux).
    // Read by trusty_common::read_daemon_addr("trusty-review").
    // Best-effort: never fail startup on an I/O error.
    let primary_path = match trusty_common::write_daemon_addr("trusty-review", &addr_str) {
        Ok(()) => {
            // Reconstruct the path for cleanup; resolve_data_dir is cheap.
            trusty_common::resolve_data_dir("trusty-review")
                .ok()
                .map(|d| d.join("http_addr"))
        }
        Err(e) => {
            warn!("trusty-review: could not write OS-standard http_addr discovery file: {e:#}");
            None
        }
    };

    // Also write to ~/.trusty-review/http_addr so external tools (e.g.
    // claude-mpm _is_present) that read the dotfile convention find the
    // daemon on non-default ports. On macOS the OS-standard path differs
    // from the dotfile path so we write both (mirrors trusty-memory #498).
    let dotfile_path = match dotfile_http_addr_path() {
        Some(p) => match write_addr_to_path(&p, &addr_str) {
            Ok(()) => {
                info!(
                    "trusty-review: wrote dotfile discovery address to {}",
                    p.display()
                );
                Some(p)
            }
            Err(e) => {
                warn!(
                    "trusty-review: could not write dotfile http_addr {}: {e}",
                    p.display()
                );
                None
            }
        },
        None => {
            warn!("trusty-review: no $HOME — skipping dotfile http_addr discovery file");
            None
        }
    };

    // Clone the shutdown sender before consuming `state` into the router.
    // After axum::serve returns (shutdown), we broadcast to all outcome-poll tasks.
    let shutdown_tx = Arc::clone(&state.shutdown_tx);
    let app = build_router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(trusty_common::shutdown_signal())
        .await?;
    // Signal all sleeping outcome-poll background tasks to exit gracefully.
    let _ = shutdown_tx.send(true);

    // Best-effort cleanup: remove discovery files so stale clients fail fast
    // instead of timing out against a dead port.
    if let Some(p) = primary_path.as_ref() {
        let _ = std::fs::remove_file(p);
    }
    if let Some(p) = dotfile_path.as_ref() {
        let _ = std::fs::remove_file(p);
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the dotfile discovery path is `$HOME/.trusty-review/http_addr`.
    ///
    /// Why: claude-mpm reads `~/.trusty-review/http_addr`; if the path helper
    /// returns the wrong location the daemon will start successfully but
    /// autodetect will silently fail. This test pins the expected layout.
    /// What: calls `dotfile_http_addr_path()` and asserts the path ends with
    /// `.trusty-review/http_addr` relative to `$HOME`.
    /// Test: runs in-process with `TRUSTY_DATA_DIR_OVERRIDE` unset so it uses
    /// the real `dirs::home_dir()`.
    #[test]
    fn dotfile_http_addr_path_is_under_home() {
        let Some(home) = dirs::home_dir() else {
            // No $HOME in the test environment — skip gracefully.
            eprintln!("skip: no $HOME");
            return;
        };
        let path = dotfile_http_addr_path()
            .expect("dotfile_http_addr_path returned None when $HOME is set");
        assert_eq!(
            path,
            home.join(".trusty-review").join("http_addr"),
            "dotfile path must be $HOME/.trusty-review/http_addr"
        );
    }

    /// Verify the format of the addr string written to the discovery file.
    ///
    /// Why: both `trusty_common::read_daemon_addr` and the claude-mpm reader
    /// expect a bare `host:port` string with no scheme or trailing newline.
    /// Encoding it via `SocketAddr::to_string()` is correct but this test
    /// documents (and guards) that contract.
    /// What: formats a loopback `SocketAddr` and asserts the string matches
    /// the expected `host:port` form.
    /// Test: pure unit test; no I/O.
    #[test]
    fn addr_string_format_is_host_colon_port() {
        let addr: SocketAddr = "127.0.0.1:7880".parse().unwrap();
        let s = addr.to_string();
        assert_eq!(
            s, "127.0.0.1:7880",
            "addr format must be host:port bare string"
        );
        // No scheme prefix.
        assert!(!s.contains("http"), "addr must not include http:// scheme");
        // No trailing newline.
        assert!(!s.ends_with('\n'), "addr must not have trailing newline");
    }

    /// Verify `write_addr_to_path` creates the parent directory and writes the file.
    ///
    /// Why: the dotfile path `~/.trusty-review/http_addr` requires that
    /// `~/.trusty-review/` exists, which may not be the case on a fresh install.
    /// `write_addr_to_path` must create the directory before writing.
    /// What: writes an addr to a path inside a temp dir that does not pre-exist,
    /// reads it back, and asserts the content matches.
    /// Test: uses `tempfile::tempdir()` for isolation.
    #[test]
    fn addr_file_write_creates_parent_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("subdir").join("http_addr");
        write_addr_to_path(&target, "127.0.0.1:7880").expect("write_addr_to_path");
        let content = std::fs::read_to_string(&target).expect("read back");
        assert_eq!(content, "127.0.0.1:7880");
    }

    /// Cross-crate port-uniqueness contract (#2566, extended by #2573).
    ///
    /// Why: the original `DEFAULT_PORT` value (7880) silently collided with
    /// trusty-mpm's `DEFAULT_DAEMON_ADDR` (`core/discovery.rs`), which is bound
    /// on every managed machine — a launchd agent running `trusty-review serve`
    /// crash-looped against the live `tm` daemon on install. The follow-up
    /// default (7890) then collided with `trusty-embedderd`'s `--http` mode
    /// default (#2573) — that gap existed because this table only tracked
    /// `tctl`-managed daemons and omitted trusty-embedderd, whose HTTP listener
    /// is a manual/dev-only opt-in (auto-spawn always uses `--stdio`/UDS), not
    /// a "full known set" as originally claimed. This table now encodes every
    /// sibling process's known default port — both `tctl`-managed daemons AND
    /// manually-run HTTP listeners like trusty-embedderd — pointer-commented to
    /// its real source constant (mirroring `trusty-installer`'s
    /// `plist_label.rs` override table), so a future edit to `DEFAULT_PORT`
    /// that reintroduces a collision fails this test instead of shipping a
    /// crash-loop or a silent bind failure.
    /// What: asserts `DEFAULT_PORT` is absent from the known-sibling-ports list.
    /// Test: this is the test.
    #[test]
    fn default_port_does_not_collide_with_known_siblings() {
        // (binary, port, source-of-truth pointer)
        let known_siblings: &[(&str, u16, &str)] = &[
            (
                "trusty-memory",
                7070,
                "trusty-memory/src/http_server.rs::DEFAULT_HTTP_PORT",
            ),
            (
                "trusty-search",
                7878,
                "trusty-search/src/service/constants.rs::DEFAULT_PORT",
            ),
            (
                "trusty-analyze",
                7879,
                "trusty-analyze/src/service/events.rs::DEFAULT_PORT",
            ),
            (
                "trusty-console",
                7788,
                "trusty-console/src/lib.rs::DEFAULT_PORT",
            ),
            (
                "trusty-mpm",
                7880,
                "trusty-mpm/src/core/discovery.rs::DEFAULT_DAEMON_ADDR",
            ),
            (
                "trusty-embedderd",
                7890,
                "trusty-embedderd/src/lib.rs::Args::http_addr (--http default_value, manual/dev-run only)",
            ),
            (
                // #3364: trusty-mpm's supervisor metrics listener — a distinct
                // process/port from the `tm` daemon (7880) above, deployed via
                // launchd and easy to miss since it isn't `tctl`-managed.
                "trusty-mpm-supervisor",
                7881,
                "trusty-mpm/src/supervisor/config.rs::DEFAULT_METRICS_ADDR",
            ),
            (
                // #3364: trusty-code's own default HTTP port, which previously
                // reused 7881 and collided with the supervisor entry above.
                "trusty-code",
                7882,
                "trusty-code/src/serve/mod.rs::DEFAULT_HTTP_PORT",
            ),
            (
                "trusty-agents",
                8080,
                "trusty-agents/src/runtime/mode_dispatch.rs (--port default 8080)",
            ),
        ];
        for (binary, port, source) in known_siblings {
            assert_ne!(
                DEFAULT_PORT, *port,
                "trusty-review DEFAULT_PORT {DEFAULT_PORT} collides with {binary}'s {port} ({source})"
            );
        }
    }
}
