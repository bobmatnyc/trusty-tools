//! `ServiceConnector` implementation for `trusty-mpm` (#1222).
//!
//! Why: the console's Overview must show trusty-mpm alongside the other services,
//! and the Sessions tab depends on the trusty-mpm daemon being reachable. Unlike
//! the other services (which write a plain `http_addr` file), the trusty-mpm
//! daemon records its bound address in a TOML lock file at
//! `~/.trusty-mpm/daemon.lock` (`addr = "http://127.0.0.1:<port>"`). This
//! connector parses that lock file and TCP-probes the port.
//! What: `MpmConnector` implements `ServiceConnector::detect()`: binary check →
//! parse `daemon.lock` `addr` → TCP probe → `Running`/`Available`/`Absent`. The
//! daemon's HTTP is internal plumbing (#1104); the console never calls it
//! directly — the connector only probes liveness for the status badge.
//! Test: `mpm_connector_absent_binary`, `mpm_connector_parses_lock_addr`,
//! `mpm_connector_no_lock_file` below.

use std::path::PathBuf;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::{binary_on_path, tcp_probe};

/// ServiceConnector for `trusty-mpm`.
///
/// Why: trusty-mpm stores its daemon lock under `~/.trusty-mpm/daemon.lock`. The
/// `addr` line there gives the exact loopback address the daemon bound (the port
/// is dynamic/auto). When present and reachable the service is `Running`.
/// What: implements `detect()` parsing the lock file's `addr` and TCP-probing it.
/// Test: `mpm_connector_parses_lock_addr`, `mpm_connector_no_lock_file`.
pub struct MpmConnector {
    /// Override for the home directory (used in tests).
    home_dir: Option<PathBuf>,
}

impl MpmConnector {
    /// Create a new `MpmConnector`.
    ///
    /// Why: production callers use `new()`; tests use `with_home()`.
    /// What: stores no state except the optional home override.
    /// Test: created in `all_connectors()` and in unit tests.
    pub fn new() -> Self {
        Self { home_dir: None }
    }

    /// Create a connector that uses `home_dir` instead of the real home.
    ///
    /// Why: unit tests must not read the real user's `~/.trusty-mpm`.
    /// What: stores `home_dir` for use in `lock_file_path()`.
    /// Test: `mpm_connector_parses_lock_addr`, `mpm_connector_no_lock_file`.
    #[cfg(test)]
    pub fn with_home(home_dir: PathBuf) -> Self {
        Self {
            home_dir: Some(home_dir),
        }
    }

    fn lock_file_path(&self) -> PathBuf {
        let home = self
            .home_dir
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".trusty-mpm").join("daemon.lock")
    }
}

impl Default for MpmConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the host:port from a trusty-mpm `daemon.lock` TOML body.
///
/// Why: the lock file is TOML with `addr = "http://127.0.0.1:<port>"`; the TCP
/// probe needs a bare `host:port` with no scheme. A tiny line scan avoids
/// pulling a TOML dependency into the console for one field.
/// What: finds the `addr = "..."` line, strips the quotes and any `http(s)://`
/// scheme, and returns the `host:port`. Returns `None` when absent/malformed.
/// Test: `parse_lock_addr_strips_scheme`, `parse_lock_addr_none_when_absent`.
fn parse_lock_addr(body: &str) -> Option<String> {
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("addr") {
            // rest looks like: ` = "http://127.0.0.1:7880"`
            let value = rest.trim_start_matches([' ', '=']).trim();
            let unquoted = value.trim_matches('"');
            let host_port = unquoted
                .strip_prefix("http://")
                .or_else(|| unquoted.strip_prefix("https://"))
                .unwrap_or(unquoted);
            if !host_port.is_empty() {
                return Some(host_port.to_string());
            }
        }
    }
    None
}

impl ServiceConnector for MpmConnector {
    fn id(&self) -> &'static str {
        "trusty-mpm"
    }

    fn display_name(&self) -> &'static str {
        "Trusty MPM"
    }

    /// Detect trusty-mpm status.
    ///
    /// Why: reads `~/.trusty-mpm/daemon.lock` (TOML) — the file the daemon writes
    /// after binding its (dynamic) port. The console only probes liveness; it
    /// never calls the daemon HTTP directly (#1104).
    /// What: binary check → parse lock `addr` → TCP probe → status. No discovery
    /// file (or unreachable) with the binary present yields `Available`.
    /// Test: `mpm_connector_parses_lock_addr`, `mpm_connector_no_lock_file`.
    fn detect(&self) -> ServiceInfo {
        if !binary_on_path("trusty-mpm") {
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Absent,
                version: None,
                url: None,
                hint: None,
            };
        }

        if let Ok(body) = std::fs::read_to_string(self.lock_file_path())
            && let Some(addr) = parse_lock_addr(&body)
            && tcp_probe(&addr)
        {
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Running,
                // The daemon HTTP is internal plumbing; do not surface a URL the
                // operator might call directly (use the console's session routes).
                version: None,
                url: None,
                hint: None,
            };
        }

        ServiceInfo {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            status: ServiceStatus::Available,
            version: None,
            url: None,
            hint: None,
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Why: the TCP probe needs a bare host:port; the parser must strip the TOML
    /// quoting and the `http://` scheme.
    /// Test: this test.
    #[test]
    fn parse_lock_addr_strips_scheme() {
        let body = "pid = 42\naddr = \"http://127.0.0.1:7880\"\nstarted_at = \"x\"\n";
        assert_eq!(parse_lock_addr(body).as_deref(), Some("127.0.0.1:7880"));
    }

    /// Why: a lock file without an addr line must yield None (treated Available).
    /// Test: this test.
    #[test]
    fn parse_lock_addr_none_when_absent() {
        assert_eq!(parse_lock_addr("pid = 42\n"), None);
    }

    /// Why: with no binary on PATH the connector must report Absent regardless of
    /// any stray lock file.
    /// Test: this test.
    #[test]
    fn mpm_connector_absent_binary() {
        // Only meaningful when the binary is genuinely not installed (CI).
        if which::which("trusty-mpm").is_ok() {
            return;
        }
        let tmp = TempDir::new().expect("tempdir");
        let info = MpmConnector::with_home(tmp.path().to_path_buf()).detect();
        assert_eq!(info.status, ServiceStatus::Absent);
        assert_eq!(info.id, "trusty-mpm");
    }

    /// Why: a stale lock pointing at a dead port must yield Available (binary
    /// present) — never Running — because the TCP probe fails.
    /// What: writes a lock with an unlikely port, calls detect(), and asserts the
    /// status is deterministic given binary presence.
    /// Test: this test.
    #[test]
    fn mpm_connector_parses_lock_addr() {
        let tmp = TempDir::new().expect("tempdir");
        let lock = tmp.path().join(".trusty-mpm").join("daemon.lock");
        fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");
        fs::write(&lock, "pid = 1\naddr = \"http://127.0.0.1:14998\"\n").expect("write");
        let info = MpmConnector::with_home(tmp.path().to_path_buf()).detect();
        if which::which("trusty-mpm").is_ok() {
            // Binary present, dead port → Available (not Running).
            assert_eq!(info.status, ServiceStatus::Available);
        } else {
            assert_eq!(info.status, ServiceStatus::Absent);
        }
    }

    /// Why: no lock file with the binary present must yield Available.
    /// Test: this test.
    #[test]
    fn mpm_connector_no_lock_file() {
        let tmp = TempDir::new().expect("tempdir");
        let info = MpmConnector::with_home(tmp.path().to_path_buf()).detect();
        if which::which("trusty-mpm").is_ok() {
            assert_eq!(info.status, ServiceStatus::Available);
        } else {
            assert_eq!(info.status, ServiceStatus::Absent);
        }
    }
}
