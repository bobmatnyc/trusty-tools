//! `ServiceConnector` implementation for `trusty-review`.
//!
//! Why: trusty-review writes its bound address to `~/.trusty-review/http_addr`
//! on successful bind. This connector reads that file and probes the TCP port,
//! completing the four-daemon P0 coverage required by issue #959.
//! What: `ReviewConnector` implements `ServiceConnector::detect()` using
//! `~/.trusty-review/http_addr` as the discovery file and `trusty-review` as
//! the binary name.
//! Test: `test_review_connector_*` in the module below. Run with
//! `cargo test -p trusty-console`.

use std::path::PathBuf;

use crate::connector::{ServiceConnector, ServiceInfo};

use super::helpers::detect_service;

/// ServiceConnector for `trusty-review`.
///
/// Why: trusty-review writes its bound address to `~/.trusty-review/http_addr`
/// on successful bind. This connector reads that file and probes the TCP port
/// to determine if the daemon is running.
/// What: Implements `detect()` using `~/.trusty-review/http_addr` as the
/// discovery file and `trusty-review` as the binary name.
/// Test: `test_review_connector_with_stale_addr_file` and
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
        // NOTE: This uses `~/.trusty-review/http_addr` (home-anchored), which
        // is consistent with the path that trusty-review's write_daemon_addr
        // writes when `dirs::data_dir()` is None and the fallback
        // `~/.trusty-review` branch of `resolve_data_dir` is taken. On macOS
        // where `dirs::data_dir()` returns `~/Library/Application Support`,
        // the daemon writes to `~/Library/Application Support/trusty-review/
        // http_addr` instead. This connector reads the home-fallback path only
        // — it will miss the daemon on macOS unless `TRUSTY_DATA_DIR_OVERRIDE`
        // forces the home path. Tracked for future alignment in #979.
        let home = self
            .home_dir
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".trusty-review").join("http_addr")
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
    /// immediately after successfully binding its port.
    /// What: Three-step sequence: binary check → addr file + TCP probe → status.
    /// Test: `test_review_connector_with_stale_addr_file`,
    /// `test_review_connector_no_addr_file`.
    fn detect(&self) -> ServiceInfo {
        detect_service(
            self.id(),
            self.display_name(),
            "trusty-review",
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
    /// connector must return Available (binary present, file present, TCP failed)
    /// when the binary is on PATH, or Absent when it is not.
    /// What: creates a fake HOME with `.trusty-review/http_addr = 127.0.0.1:14995`
    /// and calls detect(); branches on `which::which("trusty-review")` so the
    /// assertion is deterministic in both CI (no binary) and dev (binary present).
    /// Test: this test itself.
    #[test]
    fn test_review_connector_with_stale_addr_file() {
        let tmp = make_home_with_addr(".trusty-review/http_addr", "127.0.0.1:14995");
        let connector = ReviewConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        let binary_present = which::which("trusty-review").is_ok();
        if binary_present {
            assert_eq!(
                info.status,
                ServiceStatus::Available,
                "binary present, stale TCP → Available"
            );
        } else {
            assert_eq!(info.status, ServiceStatus::Absent, "binary absent → Absent");
        }
        assert_eq!(info.id, "trusty-review");
        assert_eq!(info.display_name, "Trusty Review");
    }

    /// Why: when no http_addr file exists, the result depends only on whether
    /// the binary is on PATH.
    /// What: temp HOME with no trusty-review dir; branches on
    /// `which::which("trusty-review")` for a deterministic assertion.
    /// Test: this test itself.
    #[test]
    fn test_review_connector_no_addr_file() {
        let tmp = TempDir::new().expect("tempdir");
        let connector = ReviewConnector::with_home(tmp.path().to_path_buf());
        let info = connector.detect();
        let binary_present = which::which("trusty-review").is_ok();
        if binary_present {
            assert_eq!(
                info.status,
                ServiceStatus::Available,
                "binary present, no addr file → Available"
            );
        } else {
            assert_eq!(info.status, ServiceStatus::Absent, "binary absent → Absent");
        }
        assert!(info.url.is_none());
    }
}
