//! `ServiceConnector` implementation for `trusty-search`.
//!
//! Why: trusty-search writes its bound address to `~/.trusty-search/http_addr`
//! on successful bind. This connector reads that file and probes the TCP port.
//! What: `SearchConnector` implements `ServiceConnector::detect()` using
//! `~/.trusty-search/http_addr` as the discovery file and `trusty-search` as
//! the binary name.
//! Test: `test_search_connector_*` in the module below. Run with
//! `cargo test -p trusty-console`.

use std::path::PathBuf;

use crate::connector::{ServiceConnector, ServiceInfo};

use super::helpers::detect_service;

/// ServiceConnector for `trusty-search`.
///
/// Why: trusty-search writes its bound address to `~/.trusty-search/http_addr`
/// on successful bind. This connector reads that file and probes the TCP port.
/// What: Implements `detect()` using `~/.trusty-search/http_addr` as the
/// discovery file and `trusty-search` as the binary name.
/// Test: `test_search_connector_with_stale_addr_file` and
/// `test_search_connector_no_addr_file` below.
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
    /// Test: `test_search_connector_with_stale_addr_file`,
    /// `test_search_connector_no_addr_file`.
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
    /// Test: `test_search_connector_with_stale_addr_file`,
    /// `test_search_connector_no_addr_file`.
    fn detect(&self) -> ServiceInfo {
        detect_service(
            self.id(),
            self.display_name(),
            "trusty-search",
            self.addr_file_path(),
        )
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ServiceStatus;
    use std::fs;
    use tempfile::TempDir;

    fn make_home_with_addr(rel_path: &str, content: &str) -> TempDir {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join(rel_path);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, content).expect("write addr file");
        tmp
    }

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
}
