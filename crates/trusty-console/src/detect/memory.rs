//! `ServiceConnector` implementation for `trusty-memory`.
//!
//! Why: trusty-memory writes `http_addr` under its data root. The legacy
//! dotfile path `~/.trusty-memory/http_addr` is always written regardless of
//! platform and is the most reliable single location available without
//! importing `trusty-memory`'s own path-resolution logic.
//! What: `MemoryConnector` implements `detect()` using
//! `~/.trusty-memory/http_addr`.
//! Test: `test_memory_connector_*` in the module below. Run with
//! `cargo test -p trusty-console`.

use std::path::PathBuf;

use crate::connector::{ServiceConnector, ServiceInfo};

use super::helpers::detect_service;

/// ServiceConnector for `trusty-memory`.
///
/// Why: trusty-memory writes `http_addr` under its data root. The legacy
/// dotfile path is `~/.trusty-memory/http_addr`, which is the most reliable
/// single location available without importing `trusty-memory`'s own
/// path-resolution logic.
/// What: Implements `detect()` using `~/.trusty-memory/http_addr`.
/// Test: `test_memory_connector_with_stale_addr_file`,
/// `test_memory_connector_no_addr_file` below.
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

    /// Create a connector that uses `home_dir` instead of the real home.
    ///
    /// Why: Unit tests must not read or write the real user's `~/.trusty-*`
    /// directories. Injecting a temp dir keeps tests hermetic.
    /// What: Stores `home_dir` for use in `addr_file_path()`.
    /// Test: `test_memory_connector_with_stale_addr_file`,
    /// `test_memory_connector_no_addr_file`.
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
    /// Test: `test_memory_connector_with_stale_addr_file`,
    /// `test_memory_connector_no_addr_file`.
    fn detect(&self) -> ServiceInfo {
        detect_service(
            self.id(),
            self.display_name(),
            "trusty-memory",
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
}
