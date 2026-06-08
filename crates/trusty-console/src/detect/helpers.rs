//! Low-level detection helpers shared by all service connectors.
//!
//! Why: Centralising the binary-probe, TCP-probe, and HTTP-health helpers
//! avoids duplicating them across every connector and makes them independently
//! testable.
//! What: Four free functions — `binary_on_path`, `read_addr_file`, `tcp_probe`,
//! `fetch_health_version` — plus the shared `detect_service` orchestrator.
//! Test: Unit tests at the bottom of this file. Run with
//! `cargo test -p trusty-console`.

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::connector::{ServiceInfo, ServiceStatus};

// ─── primitive helpers ────────────────────────────────────────────────────────

/// Return true if `binary` is found on PATH.
///
/// Why: The binary presence check is the outermost gate — if the binary isn't
/// installed, no daemon can ever be running.
/// What: Delegates to the `which` crate.
/// Test: Covered indirectly by every connector test; mocked via PATH override.
pub(super) fn binary_on_path(binary: &str) -> bool {
    which::which(binary).is_ok()
}

/// Read an `http_addr` file and return the trimmed address string.
///
/// Why: All three daemons write the bound address (e.g. `127.0.0.1:7879`) to a
/// well-known file on successful bind. Reading that file is cheaper than a
/// TCP probe and gives us the exact port without parsing config files.
/// What: Reads `path`, trims whitespace. Returns `None` if the file is absent,
/// empty, or unreadable.
/// Test: `test_read_addr_file_*` below.
pub(super) fn read_addr_file(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// TCP-probe `host:port` with a 300 ms connect timeout.
///
/// Why: The discovery file may be stale (daemon crashed without cleanup). A
/// fast TCP probe confirms the port is actually open before we call it Running.
/// What: Attempts a non-blocking TCP connect with a 300 ms timeout. Returns
/// `true` only on a successful connection.
/// Test: `test_tcp_probe_unreachable` below.
pub(super) fn tcp_probe(addr: &str) -> bool {
    if let Ok(stream) = TcpStream::connect_timeout(
        &addr.parse().ok().unwrap_or("127.0.0.1:1".parse().unwrap()),
        Duration::from_millis(300),
    ) {
        drop(stream);
        true
    } else {
        false
    }
}

/// Fetch `/health` from `host:port` and extract the `version` field.
///
/// Why: When the daemon is Running we surface the version in the card so
/// operators can see at a glance which build is deployed.
/// What: Issues a minimal HTTP/1.1 GET over a raw `TcpStream` with a 1s
/// read timeout. Uses no external HTTP client so detect() stays sync-only
/// without pulling in reqwest blocking. Returns `None` on any error
/// (network, parse, missing field) — the caller degrades gracefully.
/// Test: Not tested against a real daemon in unit tests; the server
/// integration test in `server.rs` verifies the overall JSON shape.
pub(super) fn fetch_health_version(addr: &str) -> Option<String> {
    use std::io::{Read, Write};

    let mut stream =
        TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(800)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .ok()?;

    let request = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;

    let raw = String::from_utf8_lossy(&buf);
    // Split headers from body on blank line.
    let body_start = raw.find("\r\n\r\n").map(|i| i + 4)?;
    let body = &raw[body_start..];

    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json.get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ─── orchestrator ─────────────────────────────────────────────────────────────

/// Run the standard P0 detection sequence for a service.
///
/// Why: All three connectors use the same three-step sequence; extracting it
/// avoids triplicating the logic.
/// What: Returns a fully-populated `ServiceInfo`. When the binary is absent
/// the function returns early with `Absent`. When the addr file is present and
/// the TCP probe succeeds it returns `Running` + optional version. Otherwise
/// `Available`.
/// Test: Each connector's unit test calls the connector's `detect()` method
/// with a custom `addr_file` path pointing into a tmpdir.
pub(super) fn detect_service(
    id: &'static str,
    display_name: &'static str,
    binary: &str,
    addr_file: PathBuf,
) -> ServiceInfo {
    if !binary_on_path(binary) {
        return ServiceInfo {
            id: id.to_string(),
            display_name: display_name.to_string(),
            status: ServiceStatus::Absent,
            version: None,
            url: None,
        };
    }

    if let Some(addr) = read_addr_file(&addr_file)
        && tcp_probe(&addr)
    {
        let base_url = format!("http://{addr}");
        let version = fetch_health_version(&addr);
        return ServiceInfo {
            id: id.to_string(),
            display_name: display_name.to_string(),
            status: ServiceStatus::Running,
            version,
            url: Some(base_url),
        };
    }

    ServiceInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        status: ServiceStatus::Available,
        version: None,
        url: None,
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── read_addr_file ──────────────────────────────────────────────────────

    /// Why: round-trips a valid address file.
    /// What: writes `127.0.0.1:9999` to a temp file; asserts read_addr_file returns it.
    /// Test: this test itself.
    #[test]
    fn test_read_addr_file_returns_trimmed_content() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("http_addr");
        fs::write(&path, "  127.0.0.1:9999\n").expect("write");
        assert_eq!(read_addr_file(&path), Some("127.0.0.1:9999".to_string()));
    }

    /// Why: absent file must yield None so callers degrade to Available.
    /// What: calls read_addr_file on a non-existent path.
    /// Test: this test itself.
    #[test]
    fn test_read_addr_file_absent_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        assert_eq!(read_addr_file(&tmp.path().join("no_file")), None);
    }

    /// Why: empty file must yield None (can happen if daemon wrote then crashed).
    /// What: writes an empty file and calls read_addr_file.
    /// Test: this test itself.
    #[test]
    fn test_read_addr_file_empty_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("http_addr");
        fs::write(&path, "   \n").expect("write");
        assert_eq!(read_addr_file(&path), None);
    }

    // ── tcp_probe ───────────────────────────────────────────────────────────

    /// Why: unreachable port must yield false (not panic).
    /// What: probes a port almost certainly unused in CI (14999).
    /// Test: this test itself.
    #[test]
    fn test_tcp_probe_unreachable() {
        // Port 14999 is unlikely to be in use in any CI environment.
        assert!(!tcp_probe("127.0.0.1:14999"));
    }

    /// Why: malformed address must not panic.
    /// What: calls tcp_probe with a non-SocketAddr string.
    /// Test: this test itself.
    #[test]
    fn test_tcp_probe_malformed_addr() {
        assert!(!tcp_probe("not-an-addr"));
    }
}
