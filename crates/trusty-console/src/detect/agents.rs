//! `ServiceConnector` implementation for `trusty-agents` (#3331).
//!
//! Why: the loopback-only doctrine (#3328) makes the trusty-console reverse
//! proxy the intended remote path to the trusty-agents API (which now binds
//! `127.0.0.1` by default, #3329). For the `/api/agents/*` proxy route to
//! resolve an upstream URL, the connector must surface the daemon's live base
//! URL — exactly as the trusty-mpm connector does.
//! What: `AgentsConnector` implements `ServiceConnector::detect()` using the
//! standard `trusty-common` `http_addr` discovery file written by the daemon
//! after bind (`serve_with_config` calls `write_daemon_addr("trusty-agents")`).
//! Path: binary check (`tagent`) → `resolve_data_dir("trusty-agents")/http_addr`
//! → TCP probe → `Running`/`Available`/`Absent`. The daemon writes no TOML
//! lock file, so there is no lock-file fallback (unlike the mpm connector).
//! Test: `agents_connector_absent_binary`, `agents_connector_no_addr_file`,
//! `agents_connector_surfaces_url_via_http_addr` below.

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::{binary_on_path, detect_service};

/// ServiceConnector for `trusty-agents`.
///
/// Why: surfaces the running trusty-agents API daemon in the console Overview
/// and enables the `/api/agents/*` reverse-proxy route by providing the
/// daemon's live base URL via the standard `http_addr` discovery file (#3331).
/// What: implements `detect()` using the standard `trusty-common` data-dir
/// discovery path (`resolve_data_dir("trusty-agents")/http_addr`) written by
/// the daemon on bind. The primary (`tagent`) binary is the presence gate.
/// Test: unit tests below; run with `cargo test -p trusty-console`.
pub struct AgentsConnector {
    _priv: (),
}

impl AgentsConnector {
    /// Create a new `AgentsConnector`.
    ///
    /// Why: production callers use `new()`; there is no home override because
    /// detection reads the OS data dir via `resolve_data_dir` (overridable in
    /// tests through `TRUSTY_DATA_DIR_OVERRIDE`).
    /// What: stores no state.
    /// Test: created in `all_connectors()` and in unit tests.
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl Default for AgentsConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for AgentsConnector {
    fn id(&self) -> &'static str {
        "trusty-agents"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Agents"
    }

    /// Detect trusty-agents status, surfacing the daemon URL when reachable.
    ///
    /// Why: the `/api/agents/*` proxy handler resolves the daemon base URL from
    /// this connector's `ServiceInfo.url`, so `detect()` must surface a URL when
    /// the daemon is reachable via the standard `http_addr` discovery file.
    /// What: binary check (`tagent`) →
    /// `resolve_data_dir("trusty-agents")/http_addr` → TCP probe → `Running`
    /// with `url: Some(base_url)`; binary present but no reachable addr file →
    /// `Available`; binary absent → `Absent`. Delegates to the shared
    /// `detect_service()` helper (addr-file read, TCP probe, version fetch).
    /// Test: `agents_connector_surfaces_url_via_http_addr` (primary path),
    /// `agents_connector_no_addr_file`, `agents_connector_absent_binary`.
    fn detect(&self) -> ServiceInfo {
        // `resolve_data_dir` is infallible in practice; if the data directory
        // cannot be resolved, report status purely on binary presence.
        if let Ok(dir) = trusty_common::resolve_data_dir("trusty-agents") {
            return detect_service(
                self.id(),
                self.display_name(),
                "tagent",
                dir.join("http_addr"),
            );
        }

        ServiceInfo {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            status: if binary_on_path("tagent") {
                ServiceStatus::Available
            } else {
                ServiceStatus::Absent
            },
            version: None,
            url: None,
            hint: None,
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::ENV_LOCK;
    use super::*;
    use std::fs;
    use std::net::TcpListener;
    use tempfile::TempDir;
    use trusty_common::DATA_DIR_OVERRIDE_ENV;

    /// Why: with no binary on PATH the connector must report Absent regardless
    /// of any stale discovery file.
    /// Test: this test.
    #[test]
    fn agents_connector_absent_binary() {
        // Only meaningful when the binary is genuinely not installed (CI).
        if which::which("tagent").is_ok() {
            return;
        }
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data_tmp = TempDir::new().expect("data-tempdir");
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        }
        let info = AgentsConnector::new().detect();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(info.status, ServiceStatus::Absent);
        assert_eq!(info.id, "trusty-agents");
        assert_eq!(info.display_name, "Trusty Agents");
    }

    /// Why: no http_addr file with the binary present must yield Available (not
    /// Running); binary absent yields Absent.
    /// Test: this test.
    #[test]
    fn agents_connector_no_addr_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data_tmp = TempDir::new().expect("data-tempdir");
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        }
        let info = AgentsConnector::new().detect();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(info.url.is_none());
        if which::which("tagent").is_ok() {
            assert_eq!(info.status, ServiceStatus::Available);
        } else {
            assert_eq!(info.status, ServiceStatus::Absent);
        }
    }

    /// Why: the primary path (#3331) must surface `url: Some(base_url)` when the
    /// http_addr file exists and the port is reachable — this is what the proxy
    /// handler reads to forward `/api/agents/*` requests.
    /// What: writes a valid addr to the standard http_addr file under a temp
    /// TRUSTY_DATA_DIR_OVERRIDE, binds a real listening port so tcp_probe passes,
    /// calls detect(), and asserts the url and Running status.
    /// Test: this test (key regression guard for #3331).
    #[test]
    fn agents_connector_surfaces_url_via_http_addr() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data_tmp = TempDir::new().expect("data-tempdir");
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        }
        // Write the http_addr file with a listening port so tcp_probe passes.
        let agents_dir = data_tmp.path().join("trusty-agents");
        fs::create_dir_all(&agents_dir).expect("mkdir");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
        let addr = listener.local_addr().expect("local_addr").to_string();
        fs::write(agents_dir.join("http_addr"), &addr).expect("write addr");

        let info = AgentsConnector::new().detect();

        // Drop listener after detect() so the port is open during the probe.
        drop(listener);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }

        if which::which("tagent").is_ok() {
            assert_eq!(
                info.status,
                ServiceStatus::Running,
                "http_addr present + port open must yield Running, got: {info:?}"
            );
            assert_eq!(
                info.url,
                Some(format!("http://{addr}")),
                "Running status must include daemon base URL for proxy routing"
            );
        } else {
            assert_eq!(info.status, ServiceStatus::Absent);
        }
    }
}
