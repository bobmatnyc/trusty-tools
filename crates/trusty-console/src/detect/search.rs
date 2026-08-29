//! `ServiceConnector` implementation for `trusty-search`.
//!
//! Why: trusty-search served TCP loopback HTTP and published its bound address
//! in `~/.trusty-search/http_addr`; this connector read that file and probed the
//! port. #6285 (ADR-0032) moves the daemon onto a hardened Unix socket, so both
//! are gone — there is no port and no discovery file, and the file that is still
//! on disk from before the migration names 7878, which any process can now hold.
//! The socket path is derived, and the daemon and this connector resolve it
//! through the same `trusty_common::daemon_socket_path` call.
//!
//! What: `SearchConnector::detect()` dials `search.health` over the socket and
//! reads `version` off the answer.
//! Test: `search_connector_reports_available_when_nothing_is_serving`,
//! `search_connector_reads_the_version_off_a_live_socket`,
//! `search_connector_surfaces_an_unresolvable_socket_path_as_a_hint`,
//! `search_connector_reports_an_error_frame_as_not_running`.

use std::path::{Path, PathBuf};

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};
use crate::search_uds::{HEALTH_TIMEOUT, METHOD_HEALTH, SEARCH_SERVICE};

use super::helpers::binary_on_path;

/// The `result` half of a `search.health` response, as far as the console reads
/// it.
///
/// Only `version` is consumed — the card renders it. `status` is deserialised
/// too so a body carrying neither is refused as not-a-health-envelope rather
/// than silently rendering a versionless Running card.
#[derive(Debug, serde::Deserialize)]
struct HealthEnvelope {
    /// `"ok"` or `"degraded"`. Presence is what makes this a health answer.
    #[allow(dead_code)]
    status: String,
    /// The daemon's own version, rendered on the service card.
    version: Option<String>,
}

/// ServiceConnector for `trusty-search`.
///
/// Why: the dashboard needs to know whether the search daemon is running, and
/// since #6285 that question is answered by dialling its socket.
/// What: implements `detect()` — binary on PATH, then one `search.health` call.
/// Test: see the module docs.
pub struct SearchConnector {
    /// Override for the socket path (used in tests).
    ///
    /// Before #6285 this was a HOME override, because the discovery file lived
    /// under `~`. The socket path comes from the data directory now, which
    /// `TRUSTY_DATA_DIR_OVERRIDE` already redirects — but that variable is
    /// process-global and this connector runs beside five others in one poll,
    /// so a path override keeps a test from redirecting its siblings too.
    socket: Option<PathBuf>,
}

impl SearchConnector {
    /// Create a new `SearchConnector`.
    pub fn new() -> Self {
        Self { socket: None }
    }

    /// Create a connector that dials `socket` instead of the resolved path.
    ///
    /// Why: unit tests must not dial the real user's running daemon.
    /// Test: `search_connector_reports_available_when_nothing_is_serving`.
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    /// The socket this connector dials, or why it could not be resolved.
    ///
    /// Why the error is carried rather than discarded: a data directory that
    /// cannot be resolved or created is operator-fixable (permissions, a
    /// `TRUSTY_DATA_DIR_OVERRIDE` pointing somewhere unusable), and it is
    /// indistinguishable on the dashboard from a daemon that is simply not
    /// running. `detect()` still reports `Available` — nothing was observed, so
    /// claiming otherwise would be a guess — but puts the reason in `hint`.
    fn socket_path(&self) -> Result<PathBuf, String> {
        match &self.socket {
            Some(p) => Ok(p.clone()),
            None => crate::search_uds::socket_path(),
        }
    }
}

impl Default for SearchConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Dial `search.health` and return the envelope, or `None` if nothing answered.
///
/// Why a dedicated thread: `ServiceConnector::detect` is synchronous — the
/// poller calls it inside `spawn_blocking` — and the shared UDS client is async.
/// The exchange runs on its own current-thread runtime rather than through
/// `Handle::block_on`, for the reason `trusty-installer`'s
/// `probe_member_http_blocking` records: building a runtime and blocking on it
/// from inside another runtime's worker panics, and this way the call is safe
/// from any caller regardless of what it is running on. The same shape
/// `detect::AnalyzeConnector` uses.
///
/// What: one [`crate::search_uds::call`] bounded by [`HEALTH_TIMEOUT`]. A
/// response carrying an `error` is `None`: the daemon answered, but not with
/// health, and the console has nothing to render.
///
/// Test: `search_connector_reports_an_error_frame_as_not_running`.
fn probe_health(socket: &Path) -> Option<HealthEnvelope> {
    let socket = socket.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("console-search-probe".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async {
                let result = crate::search_uds::call(
                    &socket,
                    METHOD_HEALTH,
                    serde_json::json!({}),
                    HEALTH_TIMEOUT,
                )
                .await
                .ok()?;
                serde_json::from_value::<HealthEnvelope>(result).ok()
            })
        })
        .ok()?;
    handle.join().ok()?
}

impl ServiceConnector for SearchConnector {
    fn id(&self) -> &'static str {
        SEARCH_SERVICE
    }

    fn display_name(&self) -> &'static str {
        "Trusty Search"
    }

    /// Detect trusty-search status.
    ///
    /// Why: the dashboard needs to know whether the daemon is up, and `tctl`'s
    /// probe asks the same question — so the two must agree, which they do by
    /// dialling the same method on the same derived path (#6285).
    /// What: binary check → `search.health` over the socket → status. `url` is
    /// deliberately `None`: a UDS daemon has no URL, and ADR-0032 makes
    /// trusty-console the only HTTP surface in the workspace, so a synthesised
    /// `http://` address would be a link that cannot work. The dashboard reaches
    /// the daemon's own UI at `/tools/search/` instead (#6155).
    /// Test: see the module docs.
    fn detect(&self) -> ServiceInfo {
        self.detect_from(self.socket_path())
    }
}

impl SearchConnector {
    /// [`ServiceConnector::detect`]'s body, over an already-resolved path.
    ///
    /// Why separate: the unresolvable-path arm is only reachable when
    /// `trusty_common::daemon_socket_path` fails, and the only way to make it
    /// fail from a test is to set `TRUSTY_DATA_DIR_OVERRIDE` — which is
    /// process-global and, in this crate's test binary, is read by five sibling
    /// connectors running in parallel. Taking the resolved result as a parameter
    /// makes the arm assertable with no global state at all.
    /// What: binary check, then the three verdicts. `Absent` means the binary is
    /// not installed; `Available` means installed with nothing answering on the
    /// socket; `Running` means the daemon answered `search.health`.
    /// Test: `search_connector_surfaces_an_unresolvable_socket_path_as_a_hint`.
    fn detect_from(&self, socket: Result<PathBuf, String>) -> ServiceInfo {
        let base =
            |status: ServiceStatus, version: Option<String>, hint: Option<String>| ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status,
                version,
                url: None,
                hint,
            };

        if !binary_on_path(SEARCH_SERVICE) {
            return base(ServiceStatus::Absent, None, None);
        }

        let socket = match socket {
            Ok(p) => p,
            Err(reason) => return base(ServiceStatus::Available, None, Some(reason)),
        };

        match probe_health(&socket) {
            Some(health) => base(ServiceStatus::Running, health.version, None),
            None => base(ServiceStatus::Available, None, None),
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn installed() -> ServiceStatus {
        if which::which(SEARCH_SERVICE).is_ok() {
            ServiceStatus::Available
        } else {
            ServiceStatus::Absent
        }
    }

    /// Bind a socket that answers exactly one framed request with `reply`.
    ///
    /// Must be called from inside a tokio runtime — `bind_hardened` registers
    /// the listener with the reactor. `detect()` itself is blocking and runs its
    /// dial on its own thread, so it is safe to call from a `#[tokio::test]`.
    fn stub_daemon(dir: &Path, reply: impl Into<String>) -> PathBuf {
        let socket = dir.join("sockets").join("search.sock");
        let reply = reply.into();
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let _ = conn.write_all(reply.as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });
        socket
    }

    /// Run `detect()` off the runtime's worker so the blocking probe inside it
    /// cannot stall the stub task that has to answer it.
    async fn detect_against(socket: PathBuf) -> ServiceInfo {
        tokio::task::spawn_blocking(move || SearchConnector::with_socket(socket).detect())
            .await
            .expect("detect")
    }

    /// Why (#6285): the pre-migration connector read `~/.trusty-search/http_addr`
    /// and probed the port it named, so once the daemon stops writing that file
    /// any process holding 7878 would make this report a trusty-search that is
    /// not there. The file path is gone, and this is what keeps it gone: an
    /// absent socket is `Available`, never `Running`, whatever else is listening
    /// on the machine.
    /// Test: this is the test.
    #[test]
    fn search_connector_reports_available_when_nothing_is_serving() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let connector = SearchConnector::with_socket(tmp.path().join("absent.sock"));
        let info = connector.detect();

        assert_eq!(info.status, installed());
        assert_eq!(info.id, SEARCH_SERVICE);
        assert_eq!(info.display_name, "Trusty Search");
        assert!(
            info.url.is_none(),
            "a UDS daemon has no URL to link to: {info:?}"
        );
    }

    /// Why: the service card renders the daemon's version, so a live socket has
    /// to produce `Running` with that version rather than a bare liveness bit.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn search_connector_reads_the_version_off_a_live_socket() {
        if which::which(SEARCH_SERVICE).is_err() {
            // The binary check short-circuits to Absent before any dial, so
            // there is nothing to assert on a machine without it installed.
            return;
        }
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(
            tmp.path(),
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"0.49.6","indexes":3}}"#,
        );
        let info = detect_against(socket).await;
        assert_eq!(info.status, ServiceStatus::Running);
        assert_eq!(info.version.as_deref(), Some("0.49.6"));
    }

    /// Why: the fail-open arm. A daemon that answers a JSON-RPC `error` has told
    /// us nothing about its health, and rendering `Running` off the fact that
    /// SOMETHING replied is exactly the false-healthy card #6285 must not
    /// introduce.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn search_connector_reports_an_error_frame_as_not_running() {
        if which::which(SEARCH_SERVICE).is_err() {
            return;
        }
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(
            tmp.path(),
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#,
        );
        let info = detect_against(socket).await;
        assert_eq!(info.status, ServiceStatus::Available);
        assert!(info.version.is_none(), "{info:?}");
    }

    /// Why: an answer that is not a health envelope must not render a
    /// versionless Running card — the daemon replied, but not with health.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn search_connector_reports_a_non_health_answer_as_not_running() {
        if which::which(SEARCH_SERVICE).is_err() {
            return;
        }
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(tmp.path(), r#"{"jsonrpc":"2.0","id":1,"result":{"hi":1}}"#);
        let info = detect_against(socket).await;
        assert_eq!(info.status, ServiceStatus::Available);
    }

    /// Why: an unusable data directory is operator-fixable and looks identical
    /// on the dashboard to a daemon that is not running, so the reason has to
    /// reach the card rather than being swallowed.
    /// Test: this is the test.
    #[test]
    fn search_connector_surfaces_an_unresolvable_socket_path_as_a_hint() {
        let connector = SearchConnector::new();
        let info = connector.detect_from(Err("no data directory".to_string()));
        if info.status == ServiceStatus::Absent {
            // No binary on PATH: the check short-circuits before the path arm.
            return;
        }
        assert_eq!(info.status, ServiceStatus::Available);
        assert_eq!(info.hint.as_deref(), Some("no data directory"));
    }
}
