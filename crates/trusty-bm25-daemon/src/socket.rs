//! Unix domain socket path resolution and stale-file cleanup.
//!
//! Why: the daemon and any BM25 client must agree on a per-palace default
//! socket path, and both startup and shutdown paths need to delete the socket
//! file so a crash-then-restart does not see `EADDRINUSE`. Centralising the
//! logic keeps the two contracts identical.
//!
//! What: `default_socket_path(palace)` returns
//! `$TMPDIR/trusty-bm25-<palace>.sock` (falling back to `/tmp` if `TMPDIR`
//! is unset or empty); `cleanup_stale_socket` best-effort removes the file
//! at the given path, ignoring "not found".
//!
//! Test: covered by `default_socket_path_uses_tmpdir` and
//! `cleanup_stale_socket_idempotent`.

use std::path::{Path, PathBuf};

/// Filename prefix for the BM25 daemon's per-palace Unix domain socket.
///
/// Why: the daemon serves one palace per instance, so the socket name embeds
/// the palace id. Pinning the prefix in one place prevents the daemon and
/// the client from drifting.
/// What: `trusty-bm25-` — the suffix is `<palace>.sock`, sanitised so it
/// never contains a path separator.
/// Test: trivially covered by `default_socket_path_uses_tmpdir`.
pub const SOCKET_PREFIX: &str = "trusty-bm25-";

/// Filename suffix for the BM25 daemon's per-palace Unix domain socket.
pub const SOCKET_SUFFIX: &str = ".sock";

/// Sanitise a palace id into a filesystem-safe socket-name fragment.
///
/// Why: a palace id may legally contain characters that are illegal (or
/// confusing) in a filesystem path — `/`, `..`, NUL, whitespace. Sockets
/// also have a tight `sun_path` limit (~104 bytes on macOS, ~108 on Linux),
/// so we trim and replace rather than letting `UnixListener::bind` blow up
/// with a cryptic EINVAL.
/// What: replaces every char that isn't `[A-Za-z0-9._-]` with `_`, and
/// truncates the result to 64 bytes so the full path stays well under the
/// kernel's `sun_path` ceiling.
/// Test: `sanitise_palace_strips_separators`.
pub fn sanitise_palace(palace: &str) -> String {
    let mut out = String::with_capacity(palace.len());
    for ch in palace.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out.truncate(64);
    out
}

/// Resolve the default socket path for `palace` under `$TMPDIR` (or `/tmp`).
///
/// Why: macOS launchd hands each agent a per-process `TMPDIR` mounted under
/// `/var/folders/...`, while Linux servers usually leave `TMPDIR` unset and
/// expect `/tmp`. Honouring the env var keeps the socket inside whatever
/// scratch space the OS assigned the daemon.
/// What: returns `<TMPDIR>/trusty-bm25-<palace>.sock`. If `TMPDIR` is
/// unset, empty, or whitespace, falls back to `/tmp`. The palace fragment
/// is sanitised via [`sanitise_palace`].
/// Test: `default_socket_path_uses_tmpdir`.
pub fn default_socket_path(palace: &str) -> PathBuf {
    let dir = match std::env::var("TMPDIR") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from("/tmp"),
    };
    let fname = format!("{SOCKET_PREFIX}{}{SOCKET_SUFFIX}", sanitise_palace(palace));
    dir.join(fname)
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
        let p = default_socket_path("default");
        let name = p.file_name().and_then(|s| s.to_str()).unwrap();
        assert!(name.starts_with(SOCKET_PREFIX));
        assert!(name.ends_with(SOCKET_SUFFIX));
        assert!(p.parent().is_some());
    }

    #[test]
    fn sanitise_palace_strips_separators() {
        assert_eq!(sanitise_palace("a/b"), "a_b");
        assert_eq!(sanitise_palace(".."), "..");
        assert_eq!(sanitise_palace("ok-name_1"), "ok-name_1");
        assert_eq!(sanitise_palace(""), "_");
        assert_eq!(sanitise_palace("with space"), "with_space");
        let very_long = "x".repeat(200);
        assert!(sanitise_palace(&very_long).len() <= 64);
    }

    #[test]
    fn cleanup_stale_socket_idempotent() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "trusty-bm25-test-cleanup-{}-{}",
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
