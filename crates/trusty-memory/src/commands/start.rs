//! Handler for `trusty-memory start` — boots the daemon in the background.
//!
//! Why: the trusty-* daemons historically had diverging `start` / `serve` /
//! `stop` semantics. `trusty-memory start` mirrors `trusty-search start`: it
//! spawns a detached `serve --foreground`. A second `start` while the daemon is
//! already serving is a no-op rather than a second instance racing the first for
//! redb's write lock.
//!
//! What (#6286): probes the socket and, on a miss, goes through
//! [`crate::commands::daemon_guard::ensure_daemon_running`] — which takes the
//! same [`start_lock_path`] lock the `serve --stdio` bridge takes, so a `start`
//! racing a bridge still cannot produce two daemons. It used to probe an
//! `http_addr` file plus `GET /health` and delegate to
//! `trusty_mcp::ensure_daemon_up_single_flight`; that helper is built around a
//! health URL this daemon no longer has.
//! Test: `start_lock_lives_beside_the_socket`; the exclusion itself in
//! `crate::commands::daemon_guard::tests`.

use anyhow::Result;
use colored::Colorize;

/// Path to the exclusive lock that serialises daemon starts (#5267).
///
/// Why: `handle_start` and the `serve --stdio` bridge both start the daemon.
/// They must contend for the SAME lock file or the exclusion is only within each
/// path and a `start` racing a bridge still yields two daemons — #1152's failure
/// mode. One derivation, used by both, is what makes that impossible.
/// What: returns `{resolve_data_dir("trusty-memory")}/start.lock`, under the
/// same (test-overridable) data dir the socket is derived from. Returns `None`
/// when the data dir cannot be resolved.
/// Test: `start_lock_lives_beside_the_socket`.
pub(crate) fn start_lock_path() -> Option<std::path::PathBuf> {
    trusty_common::resolve_data_dir("trusty-memory")
        .ok()
        .map(|d| d.join("start.lock"))
}

/// Boot the trusty-memory daemon in the background.
///
/// Why: the daemon must outlive the invoking shell, so it runs detached rather
/// than tied to the controlling terminal (which broke shell profiles, tmux
/// panes, and `make`-driven dev loops). Since #5267 this also waits for
/// readiness before returning: the previous fire-and-forget return released the
/// start lock before the daemon was listening, which let the next contender
/// re-probe a dead endpoint and start a second daemon (#1152).
///
/// What: delegates to [`crate::commands::daemon_guard::ensure_daemon_running`],
/// which fast-paths a live socket, then starts `serve --foreground` at most once
/// across all processes and waits for it to answer. Fails closed if it never
/// does.
///
/// Test: `start_lock_lives_beside_the_socket`; the exclusion itself in
/// `crate::commands::daemon_guard::tests`.
pub async fn handle_start() -> Result<()> {
    let socket = crate::transport::uds::socket_path()?;
    if crate::commands::daemon_guard::probe(&socket).await {
        eprintln!(
            "{} trusty-memory is already running on {}",
            "◉".green(),
            socket.display()
        );
        return Ok(());
    }

    let lock_path = start_lock_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve the trusty-memory data directory"))?;
    crate::commands::daemon_guard::ensure_daemon_running(&socket, &lock_path).await?;
    eprintln!(
        "{} trusty-memory daemon ready on {}",
        "✓".green(),
        socket.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `handle_start` and the stdio bridge must contend for the SAME lock
    /// file. If they derived different paths the exclusion would hold only
    /// within each path, and a `start` racing a bridge would still yield two
    /// daemons — #1152's failure mode through a side door.
    /// What: asserts the lock is derived from the same data directory the socket
    /// is, so both callers resolve it identically.
    ///
    /// The two spawn-shape tests that used to sit here retired with
    /// `daemon_start_config` (#6286): they pinned `serve --foreground` with no
    /// `--http 127.0.0.1:0`, and there is no `DaemonBridgeConfig` left to
    /// inspect. `daemon_guard::spawn_daemon` is now the one place that names
    /// the spawn args, and it passes exactly `serve --foreground`.
    /// Test: itself.
    #[test]
    fn start_lock_lives_beside_the_socket() {
        let (Some(lock), Ok(socket)) = (start_lock_path(), crate::transport::uds::socket_path())
        else {
            return; // No resolvable data dir in this environment.
        };
        assert_eq!(lock.file_name().expect("a file name"), "start.lock");
        assert!(
            socket.starts_with(lock.parent().expect("the lock has a parent")),
            "the start lock ({}) and the socket ({}) must derive from one data \
             directory",
            lock.display(),
            socket.display()
        );
    }
}
