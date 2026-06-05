//! Supervised `serve --foreground` entry point (issue #787).
//!
//! Why: extracted from `lib.rs` to keep that file under the 500-line-cap
//! ratchet budget. All three issue-#787 fixes (lock file ownership, http_addr
//! guarantee, bind-abort-on-collision) live here so they are easy to find and
//! test together.
//! What: exports `bind_foreground_port` (Fix C) and `run_http_foreground`
//! (Fix A + Fix B + Fix C combined entry point) for use by `main.rs`.
//! Test: `bind_foreground_port_refuses_collision` (unit, real TCP bind);
//! `daemon_lock` module tests cover the lock-file logic.

use anyhow::Result;
use std::net::SocketAddr;

use crate::commands::daemon_lock;
use crate::{run_http_on, AppState, DEFAULT_HTTP_PORT};

/// Bind a `TcpListener` to `127.0.0.1:DEFAULT_HTTP_PORT` for the
/// launchd/supervisor `serve --foreground` path — abort on collision.
///
/// Why (issue #787, Fix C): the generic `bind_dynamic_port` silently walks
/// 7070→7071→…→7079 when port 7070 is taken, then falls back to an
/// OS-assigned port. Under `serve --foreground` (the launchd path) this
/// produces a hidden second daemon on a different port rather than failing
/// loudly. The correct behavior for a supervised `serve --foreground` is to
/// abort with a clear error when port 7070 is already bound — the existing
/// daemon is already serving and the new instance should not start at all.
/// The `single_instance_check` in `main.rs` catches the live-daemon case
/// before reaching this function; the only cases that slip through are
/// non-trusty-memory processes bound to 7070 (a genuine conflict) and race
/// windows (two simultaneous launchd respawns) — both are better served by
/// a loud error than a silent port-walk.
/// What: attempts to bind exactly `127.0.0.1:DEFAULT_HTTP_PORT`. Returns
/// `Err` with a clear human-readable message when the port is already in
/// use (EADDRINUSE), so callers can surface it to launchd's log and the
/// user's terminal instead of silently spawning on a different port.
/// Test: `bind_foreground_port_refuses_collision` occupies port 7070 then
/// asserts `bind_foreground_port` returns `Err` containing "already in use".
pub async fn bind_foreground_port() -> Result<tokio::net::TcpListener> {
    let addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], DEFAULT_HTTP_PORT));
    tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!(
                "port {} is already in use — another trusty-memory daemon is \
                     likely running. Check with `trusty-memory port` or \
                     `lsof -nP -iTCP:{} -sTCP:LISTEN`. If the existing daemon is \
                     healthy, this instance will exit 0 (expected under launchd \
                     KeepAlive). If the existing daemon is stale, run \
                     `trusty-memory stop` first.",
                DEFAULT_HTTP_PORT,
                DEFAULT_HTTP_PORT
            )
        } else {
            anyhow::anyhow!("bind {}:{}: {e}", addr.ip(), DEFAULT_HTTP_PORT)
        }
    })
}

/// The canonical `serve --foreground` entry point used by launchd and systemd
/// supervisors (issue #787).
///
/// Why (issue #787): previously `serve --foreground` shared the same
/// `run_http_dynamic` path used by ad-hoc CLI invocations. That path
/// silently port-walked (7070→7071→…→7079→OS-assigned) on bind collision,
/// producing hidden second instances that never appeared in the `http_addr`
/// discovery file at the expected port. This function replaces that path
/// for the supervised case with three explicit guarantees:
///
/// 1. **Lock file ownership** (Fix A): writes `daemon.lock` containing the
///    current PID before binding. The RAII guard removes the file on any
///    exit (graceful shutdown, panic, launchd SIGTERM). `start` and the
///    single-instance guard read this file as a second detection layer when
///    `http_addr` is absent or stale.
///
/// 2. **http_addr written on bind** (Fix B): `run_http_on` writes both the
///    OS-standard `http_addr` file and the legacy dotfile path
///    (`~/.trusty-memory/http_addr`) immediately after binding, before
///    accepting the first request. Both files are removed on clean shutdown.
///    This ensures `trusty-memory port` and the MCP bridge always find the
///    running daemon.
///
/// 3. **Abort on port collision** (Fix C): uses `bind_foreground_port`
///    (binds exactly port 7070, returns `Err` on `EADDRINUSE`) instead of
///    the port-walking `bind_dynamic_port`. If 7070 is already taken the
///    function returns `Err` with a clear message; the caller (`main.rs`)
///    exits non-zero, launchd logs the error, applies `ThrottleInterval`,
///    and the single-instance guard prevents a respawn storm.
///
/// What: acquires the daemon lock, binds `127.0.0.1:7070` (aborts on
/// collision), then runs `run_http_on` which writes the addr file and
/// serves until graceful shutdown. The lock guard is dropped after
/// `run_http_on` returns, removing `daemon.lock` best-effort.
///
/// Test: `bind_foreground_port_refuses_collision` (unit), plus the
/// integration path `trusty-memory service start` followed by a second
/// `trusty-memory serve --foreground` which should exit immediately with
/// the "already in use" error.
#[cfg(feature = "axum-server")]
pub async fn run_http_foreground(state: AppState) -> Result<()> {
    // Fix A (issue #787): acquire and own the daemon PID lock file for the
    // duration of this daemon instance. A stale lock (dead PID) is reclaimed
    // automatically; a live lock causes an `Err` that propagates to `main`,
    // which exits non-zero so launchd logs the message and applies its
    // ThrottleInterval instead of hot-looping. The RAII guard removes the
    // file on any exit path (clean, panic, SIGTERM→graceful-shutdown).
    let lock_path = daemon_lock::lock_file_path();
    let _lock_guard: Option<daemon_lock::DaemonLock> = match lock_path {
        Some(ref p) => match daemon_lock::acquire_lock(p) {
            Ok(g) => {
                tracing::info!(
                    "daemon lock acquired at {} (pid {})",
                    p.display(),
                    std::process::id()
                );
                Some(g)
            }
            Err(e) => {
                // Lock held by a live process: this is a real duplicate-start.
                // Exit non-zero so launchd records the error in its log and
                // respects ThrottleInterval. The single-instance guard in
                // main.rs handles the healthy-daemon case (exit 0); this
                // branch handles the edge case where the addr probe passed
                // but the lock is still contested.
                return Err(e.context(
                    "serve --foreground refused to start: daemon lock held by live process",
                ));
            }
        },
        None => {
            // No data dir resolvable (container, no $HOME). Log and continue
            // without a lock — discovery files also won't work in this env.
            tracing::warn!(
                "could not resolve data dir for daemon lock — \
                 starting without PID lock (containerised / no $HOME)"
            );
            None
        }
    };

    // Fix C (issue #787): bind exactly port 7070, abort on collision.
    // `bind_foreground_port` returns `Err(EADDRINUSE)` when 7070 is taken.
    // Under launchd, the single-instance guard already exits 0 before we
    // reach this point when a healthy daemon is on 7070; `Err` here means a
    // non-trusty-memory process owns the port, which is a genuine conflict.
    let listener = bind_foreground_port().await?;

    // Fix B (issue #787): `run_http_on` writes `http_addr` (and the dotfile
    // path) immediately after binding, before accepting the first request, so
    // `trusty-memory port` and the MCP bridge can locate this daemon
    // immediately. Both files are removed on graceful shutdown.
    run_http_on(state, listener).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (issue #787, Fix C): `bind_foreground_port` must return `Err`
    /// when port 7070 is already in use rather than silently walking to
    /// the next free port. This is the key guard against a launchd
    /// respawn colliding with the live daemon and producing an orphan on
    /// port 7071+. The single-instance guard catches the healthy-daemon
    /// case (exits 0) before this function is reached; this test covers
    /// the "port taken by a non-trusty-memory process" scenario.
    /// What: occupies `127.0.0.1:DEFAULT_HTTP_PORT`, then asserts that
    /// `bind_foreground_port` returns `Err` containing "already in use".
    /// Test: itself (real TCP bind, no daemon spawned).
    #[tokio::test]
    async fn bind_foreground_port_refuses_collision() {
        use std::net::SocketAddr;
        // Occupy the default port with a plain TcpListener (not trusty-memory).
        let addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], DEFAULT_HTTP_PORT));
        let Ok(_holder) = tokio::net::TcpListener::bind(addr).await else {
            // Port already taken by something else on the CI host — skip the
            // test rather than fail spuriously.
            return;
        };
        // Now `bind_foreground_port` must refuse.
        let result = bind_foreground_port().await;
        assert!(
            result.is_err(),
            "bind_foreground_port must return Err when port {DEFAULT_HTTP_PORT} \
             is already bound; got Ok"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("already in use"),
            "error message must mention 'already in use'; got: {msg}"
        );
    }
}
