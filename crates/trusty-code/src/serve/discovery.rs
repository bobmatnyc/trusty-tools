//! `tcode serve --http` daemon discovery file (issue #3415, DOC-50 §3.4).
//!
//! Why: `tcode tui` (`crate::tui_client::CodeEngine`) needs to find a running
//! `tcode serve --http` daemon without the operator copying a port number by
//! hand. DOC-50 §3.4's prose sketched a NEW, tcode-specific JSON file
//! (`~/.trusty-code/daemon.json`, `{daemon_url, pid, started_at}`) for this —
//! but this crate's siblings already solve the identical problem, and their
//! convention is different: `trusty-memory` (`crates/trusty-memory/src/http_server.rs::write_http_addr_file`)
//! and `trusty-search` (`crates/trusty-search/src/service/daemon.rs`) both
//! write the bound `host:port` as PLAIN TEXT to a file named `http_addr`
//! under `trusty_common::resolve_data_dir(<app_name>)`, atomically (tmp +
//! rename) so a reader never observes a half-written value. This module
//! follows THAT established convention for tcode instead of inventing a
//! second, JSON-shaped one: `pid`/`started_at` have no consumer anywhere in
//! this codebase yet, and a plain-text `host:port` is exactly what
//! `discover_daemon_url` (`crate::tui_client::discovery`) needs to build
//! `http://{addr}`.
//! What: [`http_addr_path`] resolves `{resolve_data_dir("trusty-code")}/http_addr`
//! (server AND client side share this one path-resolution function so they
//! can never drift onto two different locations); [`write_http_addr_file`]
//! (called from `crate::serve::http::run_http` after binding) atomically
//! writes the bound address; [`remove_http_addr_file`] (called on graceful
//! shutdown) clears it so a stopped daemon doesn't leave a stale pointer for
//! the next reader; [`read_http_addr_file`] (called from
//! `crate::tui_client::discovery`) reads and trims it back. Every operation
//! is best-effort: a write/remove failure only degrades discovery to the
//! `TCODE_DAEMON_URL` env var; a read failure (file absent, stale content,
//! unreadable) means "no candidate from this source," not a hard error — the
//! caller's overall discovery still fails with a clear, actionable message
//! when no source yields a live daemon.
//! Test: `discovery_tests::*`.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Filename of the discovery file, written under
/// `resolve_data_dir("trusty-code")` — mirrors `trusty-memory`'s
/// `http_addr` convention exactly (same filename, same directory-resolution
/// helper, same plain-text `host:port` content).
pub const HTTP_ADDR_FILENAME: &str = "http_addr";

/// Resolve `{resolve_data_dir("trusty-code")}/http_addr`, or `None` if the
/// data directory cannot be resolved (matches
/// `trusty-memory::http_server::http_addr_path`'s degrade-gracefully
/// contract — a resolution failure here is never fatal, only a missing
/// discovery source).
pub fn http_addr_path() -> Option<PathBuf> {
    trusty_common::resolve_data_dir("trusty-code")
        .ok()
        .map(|d| d.join(HTTP_ADDR_FILENAME))
}

/// Atomically write `addr` to `path` (tmp + rename).
///
/// Why: a client reading this file mid-write must never observe a partial
/// value; writing to a `.addr.tmp` sibling and renaming over the target
/// gives POSIX atomicity. Mirrors
/// `trusty-memory::http_server::write_http_addr_file` byte-for-byte in
/// behaviour — duplicated here (rather than adding a cross-product
/// dependency on trusty-memory for one ~10-line helper) since both crates
/// are independent binaries with no other shared coupling.
/// What: creates the parent directory if missing; writes `addr` followed by
/// a trailing newline; renames the tmp file over `path`.
/// Test: `discovery_tests::write_then_read_round_trips_bound_addr`.
pub(crate) fn write_http_addr_file(path: &Path, addr: &SocketAddr) -> io::Result<()> {
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

/// Best-effort remove of the discovery file.
///
/// Why: called on graceful `tcode serve --http` shutdown so a stopped
/// daemon doesn't leave a stale `http_addr` pointing at a dead port for the
/// next `CodeEngine::discover` call — a reader would otherwise pass the
/// "candidate found" step and only fail at the liveness ping, which is a
/// worse error message than "no daemon found" for the common
/// stop-then-later-reconnect case.
/// What: `std::fs::remove_file`, ignoring any error (already-absent is the
/// common case, e.g. `write_http_addr_file` itself failed earlier).
/// Test: `discovery_tests::remove_is_best_effort_on_missing_file`.
pub(crate) fn remove_http_addr_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Read and trim the discovery file's contents (just `host:port`), or
/// `None` if it's absent/unreadable/empty.
///
/// Why: the client side of discovery — deliberately infallible (`Option`,
/// not `Result`) since "no file" and "unreadable file" are both just "no
/// candidate from this source" to the caller, not distinct error states
/// worth surfacing.
/// Test: `discovery_tests::write_then_read_round_trips_bound_addr`,
/// `discovery_tests::read_missing_file_returns_none`.
pub(crate) fn read_http_addr_file(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    /// The write/read round trip must recover exactly the address that was
    /// written, with no surrounding whitespace.
    #[test]
    fn write_then_read_round_trips_bound_addr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(HTTP_ADDR_FILENAME);
        let addr: SocketAddr = "127.0.0.1:7882".parse().expect("parse addr");
        write_http_addr_file(&path, &addr).expect("write");
        assert_eq!(
            read_http_addr_file(&path).as_deref(),
            Some("127.0.0.1:7882")
        );
    }

    /// A missing file is "no candidate," not an error.
    #[test]
    fn read_missing_file_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(HTTP_ADDR_FILENAME);
        assert_eq!(read_http_addr_file(&path), None);
    }

    /// An empty (or whitespace-only) file must also read back as "no
    /// candidate" — a truncated write mid-crash should never hand the
    /// client an empty daemon URL.
    #[test]
    fn read_blank_file_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(HTTP_ADDR_FILENAME);
        std::fs::write(&path, "   \n").expect("write blank");
        assert_eq!(read_http_addr_file(&path), None);
    }

    /// Removing an already-absent file must not panic or return an
    /// observable error — this is the common case (daemon exits after a
    /// write failure, or two shutdown paths race).
    #[test]
    fn remove_is_best_effort_on_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(HTTP_ADDR_FILENAME);
        remove_http_addr_file(&path); // must not panic
        assert!(!path.exists());
    }

    /// `remove_http_addr_file` must actually delete an existing file (not
    /// just no-op on the missing case).
    #[test]
    fn remove_deletes_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(HTTP_ADDR_FILENAME);
        let addr: SocketAddr = "127.0.0.1:7882".parse().expect("parse addr");
        write_http_addr_file(&path, &addr).expect("write");
        assert!(path.exists());
        remove_http_addr_file(&path);
        assert!(!path.exists());
    }

    /// `http_addr_path` must join the filename onto whatever base
    /// `resolve_data_dir` returns — not asserting the exact OS-standard base
    /// (platform-dependent), only that the join is correct when resolution
    /// succeeds.
    #[test]
    fn http_addr_path_ends_with_filename() {
        if let Some(p) = http_addr_path() {
            assert_eq!(
                p.file_name().and_then(|f| f.to_str()),
                Some(HTTP_ADDR_FILENAME)
            );
        }
    }
}
