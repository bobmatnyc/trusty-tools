//! `ServiceConnector` implementation for `trusty-review`.
//!
//! Why: trusty-review served TCP loopback HTTP and published its bound address
//! in `~/.trusty-review/http_addr`; this connector read that file and probed the
//! port. #6277 (ADR-0032) moved the daemon onto a hardened Unix socket, so both
//! halves of that are gone — there is no port and no discovery file. It also
//! removes a bug the file made possible: a stale `http_addr` plus a squatter on
//! the fallback port read as a healthy trusty-review. There is no fallback now,
//! because there is nothing to fall back FROM: the socket path is derived, and
//! the daemon and this connector resolve it through the same
//! `trusty_common::daemon_socket_path` call.
//!
//! The retired fallback was `127.0.0.1:7880` — two port moves stale, and
//! trusty-mpm's daemon port since #2566, so a running `tm` reported
//! trusty-review as Running. It is deleted rather than corrected.
//!
//! What: `ReviewConnector::detect()` dials `review.health` over the socket and
//! reads `version` off the answer.
//! Test: `review_connector_reports_available_when_nothing_is_serving`,
//! `review_connector_reads_the_version_off_a_live_socket`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::binary_on_path;

/// How long one health dial may take, end to end.
///
/// A local socket answers in single-digit milliseconds; trusty-review's own
/// health handler bounds its dependency probes at 2 s (#3658), so this leaves
/// headroom over that without letting one wedged service stall the console's
/// whole detection pass.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// The method name `trusty-review`'s router registers for its health check.
///
/// Duplicated as a literal rather than imported: `trusty-console` has no Cargo
/// edge on `trusty-review` and adding one to share a `&str` would pull an
/// LLM-pipeline crate into the console's build. `rpc::METHOD_HEALTH` is the
/// definition; this is the client's copy, and the integration test in
/// `trusty-review/tests/uds_consumer_contract.rs` is what keeps them equal.
const METHOD_HEALTH: &str = "review.health";

/// The `result` half of a `review.health` response, as far as the console reads
/// it.
///
/// Only `version` is consumed — the card renders it. `status` is deserialised
/// too so a body that carries neither is refused as not-a-health-envelope
/// rather than silently rendering a versionless Running card.
#[derive(Debug, serde::Deserialize)]
struct HealthEnvelope {
    /// `"ok"` or `"degraded"`. Presence is what makes this a health answer.
    #[allow(dead_code)]
    status: String,
    /// The daemon's own version, rendered on the service card.
    version: Option<String>,
}

/// ServiceConnector for `trusty-review`.
///
/// Why: the console's dashboard needs to know whether the review daemon is
/// running, and since #6277 that question is answered by dialling its socket.
/// What: implements `detect()` — binary on PATH, then one `review.health` call.
/// Test: see the module docs.
pub struct ReviewConnector {
    /// Override for the socket path (used in tests).
    ///
    /// Before #6277 this was a HOME override, because the discovery file lived
    /// under `~`. The socket path comes from the data directory now, which
    /// `TRUSTY_DATA_DIR_OVERRIDE` already redirects — but that variable is
    /// process-global and this connector runs beside five others in one poll,
    /// so a path override keeps a test from redirecting its siblings too.
    socket: Option<PathBuf>,
}

impl ReviewConnector {
    /// Create a new `ReviewConnector`.
    pub fn new() -> Self {
        Self { socket: None }
    }

    /// Create a connector that dials `socket` instead of the resolved path.
    ///
    /// Why: unit tests must not dial the real user's running daemon, and the
    /// integration test needs to point this at a socket it bound itself.
    /// What: stores `socket` for use by `detect()`.
    /// Test: `review_connector_reports_available_when_nothing_is_serving`.
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    /// The socket this connector dials.
    ///
    /// Returns `None` when the data directory cannot be resolved — treated by
    /// `detect()` as "cannot tell", which reports `Available` rather than
    /// claiming the daemon is absent.
    fn socket_path(&self) -> Option<PathBuf> {
        match &self.socket {
            Some(p) => Some(p.clone()),
            None => trusty_common::daemon_socket_path("trusty-review").ok(),
        }
    }
}

impl Default for ReviewConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Dial `review.health` and return the envelope, or `None` if nothing answered.
///
/// Why: `ServiceConnector::detect` is synchronous — the poller calls it inside
/// `spawn_blocking` — and the shared UDS client is async. The exchange runs on
/// a dedicated thread with its own current-thread runtime rather than through
/// `Handle::block_on`, for the reason `trusty-installer`'s
/// `probe_member_http_blocking` records: building a runtime and blocking on it
/// from inside another runtime's worker panics, and this way the call is safe
/// from any caller regardless of what it is running on.
///
/// What: one `send_framed_request` bounded by [`HEALTH_TIMEOUT`], then a
/// JSON-RPC envelope check. A response carrying an `error` is `None`: the
/// daemon answered, but not with health, and the console has nothing to render.
///
/// Test: `review_connector_reports_available_when_nothing_is_serving`.
fn probe_health(socket: &Path) -> Option<HealthEnvelope> {
    let socket = socket.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("console-review-probe".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async {
                let request = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": METHOD_HEALTH,
                });
                let response: trusty_common::uds::server::RpcResponse =
                    trusty_common::uds::send_framed_request(&socket, &request, HEALTH_TIMEOUT)
                        .await
                        .ok()?;
                serde_json::from_value::<HealthEnvelope>(response.result?).ok()
            })
        })
        .ok()?;
    handle.join().ok()?
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
    /// Why: the console dashboard's Review tab needs to know whether the daemon
    /// is up, and `tctl` makes the same call for a different reason — so the two
    /// must agree, which they do by dialling the same method on the same
    /// derived path (#6277).
    /// What: binary check → `review.health` over the socket → status. `url` is
    /// deliberately `None`: a UDS daemon has no URL, and ADR-0032 makes
    /// trusty-console the only HTTP surface in the workspace, so a synthesised
    /// `http://` address would be a link that cannot work.
    /// Test: `review_connector_reports_available_when_nothing_is_serving`,
    /// `review_connector_reads_the_version_off_a_live_socket`.
    fn detect(&self) -> ServiceInfo {
        let base = |status: ServiceStatus, version: Option<String>| ServiceInfo {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            status,
            version,
            url: None,
            hint: None,
        };

        if !binary_on_path("trusty-review") {
            return base(ServiceStatus::Absent, None);
        }

        let Some(socket) = self.socket_path() else {
            return base(ServiceStatus::Available, None);
        };

        match probe_health(&socket) {
            Some(health) => base(ServiceStatus::Running, health.version),
            None => base(ServiceStatus::Available, None),
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the pre-#6277 connector fell back to probing `127.0.0.1:7880` when
    /// its discovery file was missing — trusty-mpm's port since #2566 — so a
    /// running `tm` made this report a trusty-review that was not there. The
    /// fallback is gone, and this is what keeps it gone: an absent socket is
    /// `Available`, never `Running`, whatever else is listening on the machine.
    /// What: points the connector at a path in an empty temp dir and asserts
    /// the verdict, branching only on whether the binary is installed.
    /// Test: this is the test.
    #[test]
    fn review_connector_reports_available_when_nothing_is_serving() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let connector = ReviewConnector::with_socket(tmp.path().join("absent.sock"));
        let info = connector.detect();

        let expected = if which::which("trusty-review").is_ok() {
            ServiceStatus::Available
        } else {
            ServiceStatus::Absent
        };
        assert_eq!(info.status, expected);
        assert_eq!(info.id, "trusty-review");
        assert_eq!(info.display_name, "Trusty Review");
        assert!(info.url.is_none(), "a UDS daemon has no URL to render");
    }

    /// Why: `Running` is the verdict that has to be earned by an ANSWER, and
    /// the version it carries is what the card renders. A connector that
    /// reported Running off a bare connect would have no version to show and
    /// would call a wedged daemon healthy.
    /// What: binds a socket that answers one `review.health` frame with a real
    /// envelope, and asserts the connector reads the version off it.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn review_connector_reads_the_version_off_a_live_socket() {
        if which::which("trusty-review").is_err() {
            eprintln!("skip: trusty-review is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("review.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let reply = br#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"9.9.9"}}"#;
            let _ = conn.write_all(reply).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });

        let connector = ReviewConnector::with_socket(socket);
        let info = tokio::task::spawn_blocking(move || connector.detect())
            .await
            .expect("detect");

        assert_eq!(info.status, ServiceStatus::Running);
        assert_eq!(info.version.as_deref(), Some("9.9.9"));
    }
}
