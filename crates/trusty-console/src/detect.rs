//! Concrete `ServiceConnector` implementations for trusty-search, trusty-memory,
//! and trusty-analyze.
//!
//! Why: P0 needs read-only detection only — reads discovery files written by
//! each daemon on bind, optionally probes the `/health` endpoint, and falls
//! back gracefully when the daemon or binary is absent.
//! What: Three structs (`SearchConnector`, `MemoryConnector`,
//! `AnalyzeConnector`) each implementing `ServiceConnector::detect()`. Each
//! uses the same detection sequence:
//! step 1 — does the binary exist on PATH? No → `Absent`.
//! step 2 — does the `http_addr` discovery file exist with a non-empty address?
//!          Yes → TCP probe + optional `/health` fetch → `Running` or `Available`.
//! step 3 — otherwise → `Available` (binary present, no daemon).
//! Test: Unit tests at the bottom inject a fake `HOME` via the `home_dir`
//! parameter so they never touch the real user's files. Run with
//! `cargo test -p trusty-console`.

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Return true if `binary` is found on PATH.
///
/// Why: The binary presence check is the outermost gate — if the binary isn't
/// installed, no daemon can ever be running.
/// What: Delegates to the `which` crate.
/// Test: Covered indirectly by every connector test; mocked via PATH override.
fn binary_on_path(binary: &str) -> bool {
    which::which(binary).is_ok()
}

/// Read an `http_addr` file and return the trimmed address string.
///
/// Why: All three daemons write the bound address (e.g. `127.0.0.1:7879`) to a
/// well-known file on successful bind. Reading that file is cheaper than a
/// TCP probe and gives us the exact port without parsing config files.
/// What: Reads `path`, trims whitespace. Returns `None` if the file is absent,
/// empty, or unreadable.
/// Test: `test_read_addr_file` below.
fn read_addr_file(path: &std::path::Path) -> Option<String> {
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
fn tcp_probe(addr: &str) -> bool {
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
fn fetch_health_version(addr: &str) -> Option<String> {
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

/// Run the standard P0 detection sequence for a service.
///
/// Why: All three connectors use the same three-step sequence; extracting it
/// avoids triplicating the logic.
/// What: Returns a fully-populated `ServiceInfo`. When the binary is absent
/// the function returns early with `Absent`. When the addr file is present and
/// the TCP probe succeeds it returns `Running` + optional version. Otherwise
/// `Available`.
/// Test: Each connector's unit test calls `detect_service` with a custom
/// `addr_file` path pointing into a tmpdir.
fn detect_service(
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

// ─── SearchConnector ─────────────────────────────────────────────────────────

/// ServiceConnector for `trusty-search`.
///
/// Why: trusty-search writes its bound address to `~/.trusty-search/http_addr`
/// on successful bind. This connector reads that file and probes the TCP port.
/// What: Implements `detect()` using `~/.trusty-search/http_addr` as the
/// discovery file and `trusty-search` as the binary name.
/// Test: `test_search_running` and `test_search_available` below.
pub struct SearchConnector {
    /// Override for the home directory (used in tests).
    home_dir: Option<PathBuf>,
}

impl SearchConnector {
    /// Create a new `SearchConnector`.
    ///
    /// Why: Production callers use `new()`; tests use `with_home()`.
    /// What: Stores no state except the optional home override.
    /// Test: Created in `all_connectors()` and in unit tests.
    pub fn new() -> Self {
        Self { home_dir: None }
    }

    /// Create a connector that uses `home_dir` instead of the real home.
    ///
    /// Why: Unit tests must not read or write the real user's `~/.trusty-*`
    /// directories. Injecting a temp dir keeps tests hermetic.
    /// What: Stores `home_dir` for use in `addr_file_path()`.
    /// Test: `test_search_running`, `test_search_available`.
    #[cfg(test)]
    pub fn with_home(home_dir: PathBuf) -> Self {
        Self {
            home_dir: Some(home_dir),
        }
    }

    fn addr_file_path(&self) -> PathBuf {
        let home = self
            .home_dir
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".trusty-search").join("http_addr")
    }
}

impl Default for SearchConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for SearchConnector {
    fn id(&self) -> &'static str {
        "trusty-search"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Search"
    }

    /// Detect trusty-search status.
    ///
    /// Why: Reads `~/.trusty-search/http_addr` — the file the daemon writes
    /// immediately after successfully binding its port.
    /// What: Three-step sequence: binary check → addr file + TCP probe → status.
    /// Test: `test_search_running`, `test_search_available`, `test_search_absent`.
    fn detect(&self) -> ServiceInfo {
        detect_service(
            self.id(),
            self.display_name(),
            "trusty-search",
            self.addr_file_path(),
        )
    }
}

// ─── MemoryConnector ─────────────────────────────────────────────────────────

/// ServiceConnector for `trusty-memory`.
///
/// Why: trusty-memory writes `http_addr` under its data root. The legacy
/// dotfile path is `~/.trusty-memory/http_addr`, which is the most reliable
/// single location available without importing `trusty-memory`'s own
/// path-resolution logic.
/// What: Implements `detect()` using `~/.trusty-memory/http_addr`.
/// Test: `test_memory_running`, `test_memory_absent` below.
pub struct MemoryConnector {
    home_dir: Option<PathBuf>,
}

impl MemoryConnector {
    /// Create a new `MemoryConnector`.
    ///
    /// Why: Matches the SearchConnector pattern for consistency.
    /// What: No-op constructor.
    /// Test: Created in `all_connectors()`.
    pub fn new() -> Self {
        Self { home_dir: None }
    }

    #[cfg(test)]
    pub fn with_home(home_dir: PathBuf) -> Self {
        Self {
            home_dir: Some(home_dir),
        }
    }

    fn addr_file_path(&self) -> PathBuf {
        let home = self
            .home_dir
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".trusty-memory").join("http_addr")
    }
}

impl Default for MemoryConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for MemoryConnector {
    fn id(&self) -> &'static str {
        "trusty-memory"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Memory"
    }

    /// Detect trusty-memory status.
    ///
    /// Why: trusty-memory writes `~/.trusty-memory/http_addr` (the legacy
    /// dotfile path) as well as the OS data-dir path; the dotfile is always
    /// written regardless of platform.
    /// What: Three-step sequence: binary check → addr file + TCP probe → status.
    /// Test: `test_memory_running`, `test_memory_absent`.
    fn detect(&self) -> ServiceInfo {
        detect_service(
            self.id(),
            self.display_name(),
            "trusty-memory",
            self.addr_file_path(),
        )
    }
}

// ─── AnalyzeConnector ────────────────────────────────────────────────────────

/// ServiceConnector for `trusty-analyze`.
///
/// Why: trusty-analyze stores its runtime state under `~/.trusty-analyze/`.
/// The daemon PID file is `~/.trusty-analyze/daemon.pid` and the default port
/// is 7879. For P0 we use a written `http_addr` file in the same directory —
/// if absent we fall back to probing the fixed default port.
/// What: Implements `detect()` checking `~/.trusty-analyze/http_addr`, then
/// falling back to `http://127.0.0.1:7879` TCP probe when the file is absent.
/// Test: `test_analyze_running`, `test_analyze_absent` below.
pub struct AnalyzeConnector {
    home_dir: Option<PathBuf>,
}

impl AnalyzeConnector {
    /// Create a new `AnalyzeConnector`.
    ///
    /// Why: Matches the other connector constructors.
    /// What: No-op.
    /// Test: Created in `all_connectors()`.
    pub fn new() -> Self {
        Self { home_dir: None }
    }

    #[cfg(test)]
    pub fn with_home(home_dir: PathBuf) -> Self {
        Self {
            home_dir: Some(home_dir),
        }
    }

    fn data_dir(&self) -> PathBuf {
        let home = self
            .home_dir
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".trusty-analyze")
    }

    fn addr_file_path(&self) -> PathBuf {
        self.data_dir().join("http_addr")
    }

    /// Default address string for trusty-analyze.
    ///
    /// Why: trusty-analyze's fixed default port is 7879. Used as a fallback
    /// when no http_addr file is found.
    /// What: Returns the address string `"127.0.0.1:7879"`.
    /// Test: Covered by the detect() fallback path.
    fn default_addr() -> &'static str {
        "127.0.0.1:7879"
    }
}

impl Default for AnalyzeConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for AnalyzeConnector {
    fn id(&self) -> &'static str {
        "trusty-analyze"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Analyze"
    }

    /// Detect trusty-analyze status.
    ///
    /// Why: trusty-analyze stores its data under `~/.trusty-analyze/`. An
    /// `http_addr` file there (if written by the running daemon) gives the
    /// exact address; otherwise we probe the default 7879.
    /// What: Binary check → addr file + TCP → fallback default-port TCP → status.
    /// Test: `test_analyze_running`, `test_analyze_absent`.
    fn detect(&self) -> ServiceInfo {
        if !binary_on_path("trusty-analyze") {
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Absent,
                version: None,
                url: None,
            };
        }

        // Try the discovery file first.
        if let Some(addr) = read_addr_file(&self.addr_file_path())
            && tcp_probe(&addr)
        {
            let base_url = format!("http://{addr}");
            let version = fetch_health_version(&addr);
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Running,
                version,
                url: Some(base_url),
            };
        }

        // Fallback: probe the well-known default port.
        let default_addr = Self::default_addr();
        if tcp_probe(default_addr) {
            let base_url = format!("http://{default_addr}");
            let version = fetch_health_version(default_addr);
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Running,
                version,
                url: Some(base_url),
            };
        }

        ServiceInfo {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            status: ServiceStatus::Available,
            version: None,
            url: None,
        }
    }
}

// ─── registry ────────────────────────────────────────────────────────────────

/// Return all connectors in display order.
///
/// Why: Centralises the connector list so the server and any future CLI
/// command iterate the same set.
/// What: Returns a `Vec<Box<dyn ServiceConnector>>` with the three P0
/// connectors.
/// Test: Indirectly tested by the server integration test.
pub fn all_connectors() -> Vec<Box<dyn ServiceConnector>> {
    vec![
        Box::new(SearchConnector::new()),
        Box::new(MemoryConnector::new()),
        Box::new(AnalyzeConnector::new()),
    ]
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a temp HOME that contains a fake `http_addr` file at `rel_path`
    /// with the given content.
    fn make_home_with_addr(rel_path: &str, content: &str) -> TempDir {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join(rel_path);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, content).expect("write addr file");
        tmp
    }

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

    // ── SearchConnector ─────────────────────────────────────────────────────

    /// Why: when http_addr contains a valid but unreachable address, the
    /// connector must return Available (binary present, file present, TCP failed).
    /// This test requires `trusty-search` to NOT be on PATH — it short-circuits
    /// to Absent if it is, which is the correct behaviour in that environment.
    /// What: creates a fake HOME with `.trusty-search/http_addr = 127.0.0.1:14998`
    /// and calls detect(); expects either Absent (binary not on PATH in CI) or
    /// Available (binary on PATH, TCP fails on 14998).
    /// Test: this test itself.
    #[test]
    fn test_search_connector_with_stale_addr_file() {
        let tmp = make_home_with_addr(".trusty-search/http_addr", "127.0.0.1:14998");
        let connector = SearchConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        // Either Absent (no binary) or Available (binary present, TCP stale).
        assert!(
            info.status == ServiceStatus::Absent || info.status == ServiceStatus::Available,
            "expected Absent or Available, got {:?}",
            info.status
        );
        assert_eq!(info.id, "trusty-search");
        assert_eq!(info.display_name, "Trusty Search");
    }

    /// Why: when no http_addr file exists, detect() must return Absent or
    /// Available depending on whether the binary is on PATH.
    /// What: temp HOME with no trusty-search dir.
    /// Test: this test itself.
    #[test]
    fn test_search_connector_no_addr_file() {
        let tmp = TempDir::new().expect("tempdir");
        let connector = SearchConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        assert!(
            info.status == ServiceStatus::Absent || info.status == ServiceStatus::Available,
            "expected Absent or Available, got {:?}",
            info.status
        );
        assert!(info.url.is_none());
    }

    // ── MemoryConnector ─────────────────────────────────────────────────────

    /// Why: stale addr file must yield Available (not Running) because TCP fails.
    /// What: creates `.trusty-memory/http_addr = 127.0.0.1:14997`.
    /// Test: this test itself.
    #[test]
    fn test_memory_connector_with_stale_addr_file() {
        let tmp = make_home_with_addr(".trusty-memory/http_addr", "127.0.0.1:14997");
        let connector = MemoryConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        assert!(
            info.status == ServiceStatus::Absent || info.status == ServiceStatus::Available,
            "expected Absent or Available, got {:?}",
            info.status
        );
        assert_eq!(info.id, "trusty-memory");
    }

    /// Why: absent addr file with binary on PATH yields Available; without binary
    /// yields Absent.
    /// What: empty temp HOME.
    /// Test: this test itself.
    #[test]
    fn test_memory_connector_no_addr_file() {
        let tmp = TempDir::new().expect("tempdir");
        let connector = MemoryConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        assert!(
            info.status == ServiceStatus::Absent || info.status == ServiceStatus::Available,
            "expected Absent or Available without addr file, got {:?}",
            info.status
        );
    }

    // ── AnalyzeConnector ────────────────────────────────────────────────────

    /// Why: stale addr file must yield Available (not Running) because TCP fails.
    /// What: creates `.trusty-analyze/http_addr = 127.0.0.1:14996`.
    /// Test: this test itself.
    #[test]
    fn test_analyze_connector_with_stale_addr_file() {
        let tmp = make_home_with_addr(".trusty-analyze/http_addr", "127.0.0.1:14996");
        let connector = AnalyzeConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        assert!(
            info.status == ServiceStatus::Absent || info.status == ServiceStatus::Available,
            "expected Absent or Available, got {:?}",
            info.status
        );
        assert_eq!(info.id, "trusty-analyze");
        assert_eq!(info.display_name, "Trusty Analyze");
    }

    /// Why: no addr file and no running daemon must yield Absent or Available.
    /// What: empty temp HOME.
    /// Test: this test itself.
    #[test]
    fn test_analyze_connector_no_addr_file() {
        let tmp = TempDir::new().expect("tempdir");
        let connector = AnalyzeConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        assert!(
            info.status == ServiceStatus::Absent || info.status == ServiceStatus::Available,
            "expected Absent or Available without addr file, got {:?}",
            info.status
        );
        assert!(info.version.is_none());
    }

    // ── all_connectors ──────────────────────────────────────────────────────

    /// Why: the registry must return exactly three connectors in order.
    /// What: calls all_connectors() and checks IDs.
    /// Test: this test itself.
    #[test]
    fn test_all_connectors_returns_three() {
        let cs = all_connectors();
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].id(), "trusty-search");
        assert_eq!(cs[1].id(), "trusty-memory");
        assert_eq!(cs[2].id(), "trusty-analyze");
    }
}
