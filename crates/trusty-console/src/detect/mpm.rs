//! `ServiceConnector` implementation for `trusty-mpm` (#1222, #1849).
//!
//! Why: the console's Overview must show trusty-mpm alongside the other services,
//! and the `/proxy/mpm/*` reverse-proxy route (#1849 Phase 1) requires a live URL
//! from the connector so the proxy handler can resolve the upstream daemon.
//! What: `MpmConnector` implements `ServiceConnector::detect()` using the standard
//! `trusty-common` http_addr discovery file written by the daemon after bind
//! (#1849 Phase 1). Primary path: binary check → `trusty-mpm` http_addr file
//! (written via `write_daemon_addr`) → TCP probe → `Running`/`Available`/`Absent`.
//! Backward-compat fallback: when the http_addr file is absent (old daemon that
//! pre-dates #1849), the connector checks the TOML lock file `~/.trusty-mpm/
//! daemon.lock` and reports Running without a URL (so the service badge is
//! accurate but the proxy cannot reach it).
//! Test: `mpm_connector_absent_binary`, `mpm_connector_parses_lock_addr`,
//! `mpm_connector_no_lock_file`, `mpm_connector_surfaces_url_via_http_addr` below.

use std::path::PathBuf;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::{binary_on_path, detect_service, tcp_probe};

/// ServiceConnector for `trusty-mpm`.
///
/// Why: surfaces the running trusty-mpm daemon in the console Overview and
/// enables the `/proxy/mpm/*` route by providing the daemon's live base URL
/// via the standard `http_addr` discovery file (#1849 Phase 1). A backward-compat
/// fallback reads the TOML lock file for daemons that pre-date the http_addr write.
/// What: implements `detect()` using the standard `trusty-common` data-dir
/// discovery path (`resolve_data_dir("trusty-mpm")/http_addr`) as the primary
/// source and the TOML lock at `~/.trusty-mpm/daemon.lock` as a fallback.
/// Test: unit tests below; run with `cargo test -p trusty-console`.
pub struct MpmConnector {
    /// Override for the home directory (used in lock-file fallback tests).
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
    /// Why: unit tests for the lock-file fallback must not read the real user's
    /// `~/.trusty-mpm`.
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
/// What: splits each line on the FIRST `=` into key/value, matches the key
/// EXACTLY against `addr` (so `addr_extra` is rejected), consumes exactly one
/// `=`, strips the quotes and any `http(s)://` scheme, and returns the
/// `host:port`. Returns `None` when absent/malformed.
///
/// The exact-key match and single-`=` split are deliberate (review finding #4):
/// the previous `strip_prefix("addr")` + `trim_start_matches([' ', '='])` matched
/// `addr_extra = "…"` and stripped ALL leading spaces/`=`, which could yield a
/// garbage address. Splitting on the first `=` and comparing the trimmed key for
/// equality fixes both issues.
/// Test: `parse_lock_addr_strips_scheme`, `parse_lock_addr_none_when_absent`,
/// `parse_lock_addr_well_formed_no_scheme`, `parse_lock_addr_ignores_prefixed_key`,
/// `parse_lock_addr_prefers_exact_key_over_decoy`.
fn parse_lock_addr(body: &str) -> Option<String> {
    for line in body.lines() {
        // Split on the FIRST `=` only; a value like an IPv6 host:port has no `=`
        // but this keeps any stray `=` inside the quoted value intact.
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Exact key match — `addr_extra`, `addr2`, etc. must NOT match.
        if key.trim() != "addr" {
            continue;
        }
        let unquoted = value.trim().trim_matches('"');
        let host_port = unquoted
            .strip_prefix("http://")
            .or_else(|| unquoted.strip_prefix("https://"))
            .unwrap_or(unquoted);
        if !host_port.is_empty() {
            return Some(host_port.to_string());
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

    /// Detect trusty-mpm status, surfacing the daemon URL when reachable.
    ///
    /// Why: Phase 1 (#1849) wires trusty-mpm into the console reverse proxy;
    /// the proxy handler resolves the daemon base URL from the connector's
    /// `ServiceInfo.url` field, so this method must surface a URL when the
    /// daemon is reachable via the standard `http_addr` discovery file.
    /// Primary path: binary check → `resolve_data_dir("trusty-mpm")/http_addr`
    /// (written by the daemon via `write_daemon_addr` after bind) → TCP probe
    /// → `Running` with `url: Some(base_url)`. If the http_addr file is absent
    /// (old daemon that pre-dates #1849), falls back to the TOML lock file and
    /// reports `Running` without a URL (proxy cannot reach it but the badge is
    /// correct). Binary absent → `Absent`.
    /// What: delegates to `detect_service()` for the http_addr path (which adds
    /// the URL and version); the lock-file fallback uses a direct tcp_probe.
    /// Test: `mpm_connector_surfaces_url_via_http_addr` (primary path),
    /// `mpm_connector_parses_lock_addr` (fallback path).
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

        // Primary path: standard http_addr file written by #1849 daemon.
        // `resolve_data_dir` is infallible in practice; degrade to the lock-file
        // fallback if the data directory cannot be resolved.
        if let Ok(dir) = trusty_common::resolve_data_dir("trusty-mpm") {
            let addr_file = dir.join("http_addr");
            if addr_file.exists() {
                // Delegate to the shared helper which does addr-file read,
                // TCP probe, version fetch, and builds ServiceInfo with url.
                return detect_service(self.id(), self.display_name(), "trusty-mpm", addr_file);
            }
        }

        // Backward-compat fallback: old daemons (pre-#1849) only write the TOML
        // lock file. Report Running without a URL so the badge is correct, but
        // the proxy cannot be used until the daemon is restarted with the new
        // version that writes the http_addr file.
        if let Ok(body) = std::fs::read_to_string(self.lock_file_path())
            && let Some(addr) = parse_lock_addr(&body)
            && tcp_probe(&addr)
        {
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Running,
                version: None,
                // URL intentionally absent: old daemon does not write http_addr,
                // so the proxy allowlist cannot resolve a safe upstream URL.
                url: None,
                hint: Some(
                    "daemon is running but pre-dates #1849 — restart to enable proxy".to_string(),
                ),
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
    use std::net::TcpListener;
    use tempfile::TempDir;
    use trusty_common::DATA_DIR_OVERRIDE_ENV;

    /// Mutex serialising tests that mutate `TRUSTY_DATA_DIR_OVERRIDE`.
    ///
    /// Why: concurrent tests that set the same env var race with each other
    /// and with trusty-common's own test suite when run in the same binary.
    /// Sharing one lock prevents spurious env-var clobber failures.
    /// What: a `std::sync::Mutex<()>` locked by every env-mutating test.
    /// Test: used by the tests in this module; not itself a test.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// Why: a well-formed `addr = "host:port"` (no scheme) must parse to the bare
    /// host:port unchanged — the common case for a scheme-less lock value.
    /// Test: this test.
    #[test]
    fn parse_lock_addr_well_formed_no_scheme() {
        assert_eq!(
            parse_lock_addr("addr = \"127.0.0.1:9001\"\n").as_deref(),
            Some("127.0.0.1:9001")
        );
    }

    /// Why: the key match must be EXACT — a different key whose name merely starts
    /// with `addr` (e.g. `addr_extra`) must NOT be mistaken for the `addr` line.
    /// Test: this test (regression guard for review finding #4).
    #[test]
    fn parse_lock_addr_ignores_prefixed_key() {
        // Only `addr_extra` present — no real `addr` key — must yield None.
        assert_eq!(
            parse_lock_addr("addr_extra = \"http://6.6.6.6:6666\"\n"),
            None
        );
    }

    /// Why: when BOTH `addr_extra` and the real `addr` are present, the parser
    /// must return the value of the EXACT `addr` key, never the prefixed decoy —
    /// regardless of declaration order.
    /// Test: this test (regression guard for review finding #4).
    #[test]
    fn parse_lock_addr_prefers_exact_key_over_decoy() {
        let body = "addr_extra = \"http://6.6.6.6:6666\"\naddr = \"http://127.0.0.1:7880\"\n";
        assert_eq!(parse_lock_addr(body).as_deref(), Some("127.0.0.1:7880"));
    }

    /// Why: with no binary on PATH the connector must report Absent regardless of
    /// any stale lock file.
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
    /// present) — never Running — because the TCP probe fails, and there is no
    /// http_addr file to pick up.
    /// What: writes a lock with an unlikely port, calls detect(), and asserts the
    /// status is deterministic given binary presence.
    /// Test: this test.
    #[test]
    fn mpm_connector_parses_lock_addr() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().expect("tempdir");
        // Override data dir to a path that has NO http_addr file so the
        // connector falls through to the lock-file path.
        let data_tmp = TempDir::new().expect("data-tempdir");
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        }
        let lock = tmp.path().join(".trusty-mpm").join("daemon.lock");
        fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");
        fs::write(&lock, "pid = 1\naddr = \"http://127.0.0.1:14998\"\n").expect("write");
        let info = MpmConnector::with_home(tmp.path().to_path_buf()).detect();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        if which::which("trusty-mpm").is_ok() {
            // Binary present, no http_addr, dead lock port → Available (not Running).
            assert_eq!(info.status, ServiceStatus::Available);
        } else {
            assert_eq!(info.status, ServiceStatus::Absent);
        }
    }

    /// Why: no lock file and no http_addr with the binary present must yield
    /// Available.
    /// Test: this test.
    #[test]
    fn mpm_connector_no_lock_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().expect("tempdir");
        let data_tmp = TempDir::new().expect("data-tempdir");
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        }
        let info = MpmConnector::with_home(tmp.path().to_path_buf()).detect();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        if which::which("trusty-mpm").is_ok() {
            assert_eq!(info.status, ServiceStatus::Available);
        } else {
            assert_eq!(info.status, ServiceStatus::Absent);
        }
    }

    /// Why: the primary path (#1849) must surface `url: Some(base_url)` when the
    /// http_addr file exists and the port is reachable; this is what the proxy
    /// handler reads to forward requests.
    /// What: writes a valid addr to the standard http_addr file under a temp
    /// TRUSTY_DATA_DIR_OVERRIDE, binds a real listening port so tcp_probe passes,
    /// calls detect(), and asserts the url and Running status.
    /// Test: this test (key regression guard for #1849 Phase 1).
    #[test]
    fn mpm_connector_surfaces_url_via_http_addr() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data_tmp = TempDir::new().expect("data-tempdir");
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        }
        // Write the http_addr file with a listening port so tcp_probe passes.
        let mpm_dir = data_tmp.path().join("trusty-mpm");
        fs::create_dir_all(&mpm_dir).expect("mkdir");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
        let addr = listener.local_addr().expect("local_addr").to_string();
        fs::write(mpm_dir.join("http_addr"), &addr).expect("write addr");

        let info = MpmConnector::new().detect();

        // Drop listener after detect() so the port is open during the probe.
        drop(listener);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }

        if which::which("trusty-mpm").is_ok() {
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
