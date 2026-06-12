//! Daemon HTTP-address file helpers.
//!
//! Why: Both trusty-search and trusty-memory persist their bound `host:port`
//! to disk so MCP clients and follow-up CLI invocations can discover where
//! the daemon ended up after auto-port-walking. Centralising the path layout
//! keeps the two projects in sync and prevents a third trusty-* daemon from
//! inventing yet another location.

use anyhow::{Context, Result};
use std::path::Path;

/// Filename used inside each app's data directory to record the daemon's
/// bound HTTP address. Kept as a module-level constant so writers and readers
/// can't drift.
///
/// Why: a single agreed-upon name means any consumer (CLI, MCP bridge) finds
/// the address file without per-daemon configuration.
/// What: the constant value `"http_addr"` — a plain UTF-8 filename.
/// Test: `daemon_addr_round_trips` relies on this name indirectly.
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
    let dir = crate::data_dir::resolve_data_dir(app_name)?;
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
    let dir = crate::data_dir::resolve_data_dir(app_name)?;
    let path = dir.join(DAEMON_ADDR_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("read daemon addr from {}", path.display())),
    }
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
/// than racing a duplicate process against the port walker.
/// What: returns `Some("http://<addr>")` only when (a) `addr_file` exists and
/// is readable, (b) its trimmed contents parse as a non-empty `host:port`, and
/// (c) an HTTP `GET http://<addr><health_path>` returns a 2xx within ~1.5 s.
/// Returns `None` on every other outcome. Stale-file cleanup: when the file
/// exists but the probe fails, the function best-effort deletes it so the
/// next caller does not chase the same dead address.
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
        let _ = std::fs::remove_file(addr_file);
        return None;
    }
    let url = format!("http://{addr}");
    if crate::health_probe::probe_health(&url, health_path).await {
        Some(url)
    } else {
        let _ = std::fs::remove_file(addr_file);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_dir::{DATA_DIR_OVERRIDE_ENV, ENV_LOCK};
    use std::path::PathBuf;

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

    #[test]
    fn daemon_addr_round_trips() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
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

    #[tokio::test]
    async fn check_already_running_returns_none_when_file_missing() {
        let tmp = tempfile_like_dir();
        let missing = tmp.join("does-not-exist");
        let got = check_already_running(&missing, "/health").await;
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn check_already_running_returns_none_when_file_empty() {
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
        let tmp = tempfile_like_dir();
        let path = tmp.join("http_addr");
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
}
