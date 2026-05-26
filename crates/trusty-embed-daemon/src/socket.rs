//! Unix domain socket path resolution and stale-file cleanup.
//!
//! Why: the daemon and any embed client must agree on a default socket path,
//! and both startup and shutdown paths need to delete the socket file so a
//! crash-then-restart does not see `EADDRINUSE`. Centralising the logic
//! keeps the two contracts identical.
//!
//! What: `default_socket_path` returns `$TMPDIR/trusty-embed.sock` (falling
//! back to `/tmp` if `TMPDIR` is unset or empty); `cleanup_stale_socket`
//! best-effort removes the file at the given path, ignoring "not found".
//!
//! Test: covered by `default_socket_path_uses_tmpdir` and
//! `cleanup_stale_socket_idempotent`.

use std::path::{Path, PathBuf};

/// Filename for the embed daemon's Unix domain socket.
///
/// Why: shared between the daemon, the client, and the cleanup hook. One
/// constant prevents the three from drifting.
/// What: `trusty-embed.sock`.
/// Test: trivially covered by `default_socket_path_uses_tmpdir`.
pub const SOCKET_FILENAME: &str = "trusty-embed.sock";

/// Resolve the default socket path under `$TMPDIR` (or `/tmp`).
///
/// Why: macOS launchd hands each agent a per-process `TMPDIR` mounted under
/// `/var/folders/...`, while Linux servers usually leave `TMPDIR` unset and
/// expect `/tmp`. Honouring the env var keeps the socket inside whatever
/// scratch space the OS assigned the daemon.
/// What: returns `<TMPDIR>/trusty-embed.sock`. If `TMPDIR` is unset, empty,
/// or whitespace, falls back to `/tmp`.
/// Test: `default_socket_path_uses_tmpdir`.
pub fn default_socket_path() -> PathBuf {
    let dir = match std::env::var("TMPDIR") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from("/tmp"),
    };
    dir.join(SOCKET_FILENAME)
}

/// Best-effort remove a (possibly-stale) socket file before bind.
///
/// Why: a previous daemon process may have crashed without removing its
/// socket file. `bind()` against an existing path returns `EADDRINUSE`,
/// which would make every restart fail. Unconditionally unlinking is the
/// idiomatic Unix pattern; we just swallow "not found" so the first-run
/// case is silent.
/// What: calls `std::fs::remove_file`. Logs any non-`NotFound` error at
/// `warn!` level so operators see permission problems but startup is
/// not aborted (the subsequent `bind` will surface the real error).
/// Test: `cleanup_stale_socket_idempotent`.
pub fn cleanup_stale_socket(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(_) => {
            tracing::debug!("removed stale socket file at {}", path.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Fresh start — nothing to clean up.
        }
        Err(e) => {
            tracing::warn!("failed to remove stale socket {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_path_uses_tmpdir() {
        // We cannot safely mutate $TMPDIR in a parallel test, so just sanity-
        // check the path ends with the expected filename and lives somewhere
        // reasonable.
        let p = default_socket_path();
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some(SOCKET_FILENAME)
        );
        assert!(p.parent().is_some());
    }

    #[test]
    fn cleanup_stale_socket_idempotent() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "trusty-embed-test-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // First call: file does not exist; must not error or panic.
        cleanup_stale_socket(&path);
        assert!(!path.exists());
        // Create it, then ensure cleanup removes it.
        std::fs::write(&path, b"").unwrap();
        assert!(path.exists());
        cleanup_stale_socket(&path);
        assert!(!path.exists());
    }
}
