//! `ServiceConnector` implementation for `trusty-mpm` (#1222, #6288).
//!
//! Why: the console's Overview must show trusty-mpm alongside the other
//! services. Before #6288 this connector read the daemon's `http_addr` file,
//! TCP-probed it, and published the HTTP base URL for the `/api/mpm/*`
//! reverse proxy. PR 1a moved every in-repo mpm client onto the daemon's Unix
//! socket and PR 1b deleted that proxy, so nothing here reads a port, an
//! `http_addr` file or the lock file's `addr` any more — PR 2 retires the
//! daemon's 7880 bind, and a connector still reading it would report a live
//! daemon as stopped.
//! What: `MpmConnector::detect()` dials `mpm.health` on the socket
//! `trusty_common::mpm_rpc::mpm_socket` resolves — the same path, and the same
//! `TRUSTY_MPM_SOCKET` override, every mpm client uses — and reads `version`
//! off the answer. `url` is always `None`.
//! Test: `mpm_connector_absent_binary`,
//! `mpm_connector_reports_available_when_nothing_is_serving`,
//! `mpm_connector_reads_the_version_off_a_live_socket`,
//! `mpm_connector_reports_degraded_when_the_daemon_answers_with_an_error`,
//! `mpm_connector_never_reads_an_http_addr`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceLifecycle, ServiceStatus};

use super::helpers::binary_on_path;
use super::socket_health::{ProbeOutcome, probe_health};

/// The binary whose presence on PATH means trusty-mpm is installed. `tm` is
/// the one full binary; `trusty-mpm` is only a compatibility alias of it.
const MPM_BINARY: &str = "tm";

/// The daemon's health method (`trusty-mpm`'s `daemon::rpc::core`).
///
/// Duplicated as a literal: the console has no Cargo edge on `trusty-mpm`.
const METHOD_HEALTH: &str = "mpm.health";

/// How long one health dial may take, end to end. A local socket answers in
/// milliseconds; this bounds a wedged daemon's cost to one detection pass.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// ServiceConnector for `trusty-mpm`.
///
/// Why: surfaces the running trusty-mpm daemon in the console Overview.
/// What: binary on PATH, then one `mpm.health` call over the daemon socket.
/// Test: see the module docs.
pub struct MpmConnector {
    /// Override for the socket path (used in tests), so a test never dials the
    /// operator's live daemon and never mutates a process-global variable.
    socket: Option<PathBuf>,
}

impl MpmConnector {
    /// Create a new `MpmConnector`.
    pub fn new() -> Self {
        Self { socket: None }
    }

    /// Create a connector that dials `socket` instead of the resolved path.
    ///
    /// Test: `mpm_connector_reports_available_when_nothing_is_serving`.
    #[cfg(test)]
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    /// The socket this connector dials. `mpm_socket` is fail-open: an
    /// unresolvable data directory yields a path nothing serves, which this
    /// connector reports as `Available`, never `Running`.
    fn socket_path(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(trusty_common::mpm_rpc::mpm_socket)
    }

    /// [`ServiceConnector::detect`]'s body, with the PATH check as a parameter.
    ///
    /// Why: `tm` is not on PATH on CI or on the Linux gate host, and a test
    /// that branched on it would assert nothing there. Taking `installed` as a
    /// parameter makes every verdict assertable on any host.
    /// What: not installed → `Absent`; a health envelope → `Running` with its
    /// version; any other answer → `Degraded` with the reason; silence →
    /// `Available`. A failed probe never reads as `Running`.
    /// Test: the `mpm_connector_*` tests below.
    fn detect_from(&self, installed: bool, socket: &Path) -> ServiceInfo {
        let base =
            |status: ServiceStatus, version: Option<String>, hint: Option<String>| ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status,
                version,
                // #6288: a socket daemon has no URL; the console proxies nothing to it.
                url: None,
                hint,
                // #6416: trusty-mpm is a resident daemon; `Available` means stopped.
                lifecycle: ServiceLifecycle::Daemon,
                cpu_pct: None,
                rss_bytes: None,
            };
        if !installed {
            return base(ServiceStatus::Absent, None, None);
        }
        match probe_health(socket, "trusty-mpm", METHOD_HEALTH, HEALTH_TIMEOUT) {
            ProbeOutcome::Healthy(health) => base(ServiceStatus::Running, health.version, None),
            ProbeOutcome::Unhealthy(reason) => base(ServiceStatus::Degraded, None, Some(reason)),
            ProbeOutcome::Silent => base(ServiceStatus::Available, None, None),
        }
    }
}

impl Default for MpmConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for MpmConnector {
    fn id(&self) -> &'static str {
        "trusty-mpm"
    }

    fn display_name(&self) -> &'static str {
        "Trusty MPM"
    }

    /// Detect trusty-mpm status over its socket (#6288).
    ///
    /// Test: `mpm_connector_absent_binary`, and through
    /// [`MpmConnector::detect_from`] the rest of the module's tests.
    fn detect(&self) -> ServiceInfo {
        self.detect_from(binary_on_path(MPM_BINARY), &self.socket_path())
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Bind `socket` and answer one frame with `reply`.
    fn spawn_health_socket(socket: &Path, reply: &'static str) {
        let listener = trusty_common::uds::bind_hardened(socket).expect("bind");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut raw = Vec::new();
            let _ = conn.read_to_end(&mut raw).await;
            let _ = conn.write_all(reply.as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });
    }

    /// Why: with no binary on PATH the connector must report Absent.
    /// Test: this test.
    #[test]
    fn mpm_connector_absent_binary() {
        let tmp = TempDir::new().expect("tempdir");
        let info = MpmConnector::with_socket(tmp.path().join("absent.sock"))
            .detect_from(false, &tmp.path().join("absent.sock"));
        assert_eq!(info.status, ServiceStatus::Absent);
        assert_eq!(info.id, "trusty-mpm");
    }

    /// Why: an absent socket is `Available` (installed, stopped), never
    /// `Running` — the Fail-Open Check for a dial that reaches nothing.
    /// Test: this test.
    #[test]
    fn mpm_connector_reports_available_when_nothing_is_serving() {
        let tmp = TempDir::new().expect("tempdir");
        let socket = tmp.path().join("absent.sock");
        let info = MpmConnector::with_socket(socket.clone()).detect_from(true, &socket);
        assert_eq!(info.status, ServiceStatus::Available);
        assert!(info.url.is_none(), "a socket daemon has no URL");
        assert!(info.hint.is_none(), "{:?}", info.hint);
    }

    /// Why: `Running` is earned by a health ANSWER, and its version is what the
    /// card renders.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread")]
    async fn mpm_connector_reads_the_version_off_a_live_socket() {
        let tmp = TempDir::new().expect("tempdir");
        let socket = tmp.path().join("s").join("trusty-mpm.sock");
        spawn_health_socket(
            &socket,
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","catalog_stale":false,"catalog_unknown":true,"catalog_changes":[],"supervised":true,"version":"1.9.0"}}"#,
        );
        let connector = MpmConnector::with_socket(socket.clone());
        let info = tokio::task::spawn_blocking(move || connector.detect_from(true, &socket))
            .await
            .expect("detect");
        assert_eq!(info.status, ServiceStatus::Running, "hint: {:?}", info.hint);
        assert_eq!(info.version.as_deref(), Some("1.9.0"));
        assert!(info.url.is_none(), "a socket daemon has no URL");
    }

    /// Why: an error answer is not health. The row must not claim `Running`;
    /// it reports reachable-but-wrong with the daemon's error verbatim.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread")]
    async fn mpm_connector_reports_degraded_when_the_daemon_answers_with_an_error() {
        let tmp = TempDir::new().expect("tempdir");
        let socket = tmp.path().join("s").join("trusty-mpm.sock");
        spawn_health_socket(
            &socket,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#,
        );
        let connector = MpmConnector::with_socket(socket.clone());
        let info = tokio::task::spawn_blocking(move || connector.detect_from(true, &socket))
            .await
            .expect("detect");
        assert_eq!(info.status, ServiceStatus::Degraded);
        assert!(info.version.is_none());
        let hint = info.hint.expect("an error answer explains itself");
        assert!(
            hint.contains("-32601") && hint.contains("mpm.health"),
            "{hint}"
        );
    }

    /// Why (#6288): the old connector read `<data dir>/trusty-mpm/http_addr`,
    /// TCP-probed it and reported `Running` with an `http://` URL. A live port
    /// in that file must not make the row `Running` while the socket is silent:
    /// once PR 2 retires the bind, that file is stale on every machine.
    /// What: a listening TCP port recorded in an `http_addr` file beside an
    /// absent socket yields `Available` and no URL.
    /// Test: this test.
    #[test]
    fn mpm_connector_never_reads_an_http_addr() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path().join("trusty-mpm");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        std::fs::write(dir.join("http_addr"), &addr).expect("write http_addr");
        let socket = dir.join("trusty-mpm.sock");

        let info = MpmConnector::with_socket(socket.clone()).detect_from(true, &socket);
        drop(listener);
        assert_eq!(info.status, ServiceStatus::Available, "{info:?}");
        assert!(info.url.is_none(), "no HTTP URL is published: {info:?}");
    }
}
