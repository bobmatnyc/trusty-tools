//! `ServiceConnector` implementation for `trusty-review`.
//!
//! Why: trusty-review writes its bound address to `~/.trusty-review/http_addr`
//! on successful bind. This connector reads that file and probes the TCP port.
//! The console previously excluded trusty-review per decision #1069; issue #1163
//! lifts that exclusion now that the Review dashboard tab is implemented.
//! What: `ReviewConnector` implements `ServiceConnector::detect()` using
//! `~/.trusty-review/http_addr` as the discovery file and `trusty-review` as
//! the binary name. Falls back to probing the default port (7880) when no
//! discovery file is found, matching the AnalyzeConnector pattern.
//! Test: `test_review_connector_with_stale_addr_file` and
//! `test_review_connector_no_addr_file` below.

use std::path::PathBuf;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::{binary_on_path, fetch_health_version, read_addr_file, tcp_probe};

/// ServiceConnector for `trusty-review`.
///
/// Why: trusty-review stores its data under `~/.trusty-review/`. An `http_addr`
/// file there (if written by the running daemon) gives the exact address;
/// otherwise we probe the default port 7880.
/// What: Implements `detect()` checking `~/.trusty-review/http_addr`, then
/// falling back to `http://127.0.0.1:7880` TCP probe when the file is absent.
/// Test: `test_review_connector_with_stale_addr_file`,
/// `test_review_connector_no_addr_file` below.
pub struct ReviewConnector {
    /// Override for the home directory (used in tests).
    home_dir: Option<PathBuf>,
}

impl ReviewConnector {
    /// Create a new `ReviewConnector`.
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
    /// Test: `test_review_connector_with_stale_addr_file`,
    /// `test_review_connector_no_addr_file`.
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
        home.join(".trusty-review").join("http_addr")
    }

    /// Default address string for trusty-review.
    ///
    /// Why: trusty-review's fixed default port is 7880. Used as a fallback
    /// when no http_addr file is found.
    /// What: Returns the address string `"127.0.0.1:7880"`.
    /// Test: Covered by the detect() fallback path.
    pub(crate) fn default_addr() -> &'static str {
        "127.0.0.1:7880"
    }
}

impl Default for ReviewConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for ReviewConnector {
    fn id(&self) -> &'static str {
        "trusty-review"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Review"
    }

    /// Detect trusty-review status.
    ///
    /// Why: Reads `~/.trusty-review/http_addr` — the file the daemon writes
    /// immediately after successfully binding its port. Falls back to probing
    /// the default port 7880 when no discovery file is present (matches the
    /// AnalyzeConnector pattern).
    /// What: Binary check → addr file + TCP probe → fallback default-port TCP → status.
    /// Test: `test_review_connector_with_stale_addr_file`,
    /// `test_review_connector_no_addr_file`.
    fn detect(&self) -> ServiceInfo {
        if !binary_on_path("trusty-review") {
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Absent,
                version: None,
                url: None,
                hint: None,
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
                hint: None,
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
    use super::super::helpers::tcp_probe;
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_home_with_addr(rel_path: &str, content: &str) -> TempDir {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join(rel_path);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, content).expect("write addr file");
        tmp
    }

    /// Why: stale addr file must yield Available (not Running) because TCP on
    /// port 14997 fails. However, the ReviewConnector also probes the default
    /// port 7880 as a fallback. If `trusty-review` is running on 7880 in the
    /// test environment this test returns Running — which is correct behaviour.
    /// What: creates `.trusty-review/http_addr = 127.0.0.1:14997` and calls
    /// detect(); branches on whether the binary is present and the default port
    /// is reachable, producing a deterministic assertion regardless of environment.
    /// Test: this test itself.
    #[test]
    fn test_review_connector_with_stale_addr_file() {
        let tmp = make_home_with_addr(".trusty-review/http_addr", "127.0.0.1:14997");
        let connector = ReviewConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        let binary_present = which::which("trusty-review").is_ok();
        let default_running = tcp_probe(ReviewConnector::default_addr());
        if !binary_present {
            assert_eq!(info.status, ServiceStatus::Absent, "binary absent → Absent");
        } else if default_running {
            assert_eq!(
                info.status,
                ServiceStatus::Running,
                "binary present, default port running → Running"
            );
        } else {
            assert_eq!(
                info.status,
                ServiceStatus::Available,
                "binary present, no running daemon → Available"
            );
        }
        assert_eq!(info.id, "trusty-review");
        assert_eq!(info.display_name, "Trusty Review");
    }

    /// Why: no addr file; result depends on whether the binary is present and
    /// the daemon is running on the default port.
    /// What: empty temp HOME; branches deterministically on
    /// `which::which("trusty-review")` and a probe of the default port.
    /// Test: this test itself.
    #[test]
    fn test_review_connector_no_addr_file() {
        let tmp = TempDir::new().expect("tempdir");
        let connector = ReviewConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        let binary_present = which::which("trusty-review").is_ok();
        let default_running = tcp_probe(ReviewConnector::default_addr());
        if !binary_present {
            assert_eq!(info.status, ServiceStatus::Absent, "binary absent → Absent");
        } else if default_running {
            assert_eq!(
                info.status,
                ServiceStatus::Running,
                "binary present, default port running → Running"
            );
        } else {
            assert_eq!(
                info.status,
                ServiceStatus::Available,
                "binary present, no running daemon → Available"
            );
        }
        assert!(info.url.is_none() || info.status == ServiceStatus::Running);
    }
}
