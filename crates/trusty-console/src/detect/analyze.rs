//! `ServiceConnector` implementation for `trusty-analyze`.
//!
//! Why: trusty-analyze stores its runtime state under `~/.trusty-analyze/`.
//! The daemon PID file is `~/.trusty-analyze/daemon.pid` and the default port
//! is 7879. For P0 we use a written `http_addr` file in the same directory —
//! if absent we fall back to probing the fixed default port.
//! What: `AnalyzeConnector` implements `detect()` checking
//! `~/.trusty-analyze/http_addr`, then falling back to `http://127.0.0.1:7879`
//! TCP probe when the file is absent.
//! Test: `test_analyze_connector_*` in the module below. Run with
//! `cargo test -p trusty-console`.

use std::path::PathBuf;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::{binary_on_path, fetch_health_version, read_addr_file, tcp_probe};

/// ServiceConnector for `trusty-analyze`.
///
/// Why: trusty-analyze stores its data under `~/.trusty-analyze/`. An
/// `http_addr` file there (if written by the running daemon) gives the exact
/// address; otherwise we probe the default 7879.
/// What: Implements `detect()` checking `~/.trusty-analyze/http_addr`, then
/// falling back to `http://127.0.0.1:7879` TCP probe when the file is absent.
/// Test: `test_analyze_connector_with_stale_addr_file`,
/// `test_analyze_connector_no_addr_file` below.
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

    /// Create a connector that uses `home_dir` instead of the real home.
    ///
    /// Why: Unit tests must not read or write the real user's `~/.trusty-*`
    /// directories. Injecting a temp dir keeps tests hermetic.
    /// What: Stores `home_dir` for use in `addr_file_path()`.
    /// Test: `test_analyze_connector_with_stale_addr_file`,
    /// `test_analyze_connector_no_addr_file`.
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
    /// Test: `test_analyze_connector_with_stale_addr_file`,
    /// `test_analyze_connector_no_addr_file`.
    fn detect(&self) -> ServiceInfo {
        // Why: this method intentionally re-implements detection rather than
        // delegating to the shared `detect_service()` helper in helpers.rs.
        // `detect_service()` only tries the addr-file path; it has no fallback
        // to a well-known default port. trusty-analyze may be running on its
        // fixed default port (7879) without having written an http_addr file
        // (e.g. launched manually or upgraded in place). The extra
        // default-port probe below covers that case. Do not "simplify" this
        // by calling detect_service() — the fallback step would be silently
        // dropped and the daemon would appear as Available when it is Running.
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

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
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
}
