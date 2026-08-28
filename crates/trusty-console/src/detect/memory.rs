//! `ServiceConnector` implementation for `trusty-memory`.
//!
//! Why: trusty-memory served TCP loopback HTTP and published its bound address
//! in `~/.trusty-memory/http_addr`; this connector read that file, probed the
//! port, and fell back to probing 7879 when the file was absent. #6286
//! (ADR-0032) moved the daemon onto a hardened Unix socket, so all three of
//! those are gone — there is no port, no discovery file, and nothing to fall
//! back FROM: the socket path is derived, and the daemon and this connector
//! resolve it through the same `trusty_common::daemon_socket_path` call.
//!
//! The retired fallback probed `127.0.0.1:7879` whenever the file was missing,
//! which is the same shape of bug #6277 removed from the review connector: any
//! process that took 7879 read as a healthy trusty-memory. It is deleted rather
//! than corrected.
//!
//! What: `MemoryConnector::detect()` dials `memory.health` over the socket and
//! reads `version` off the answer.
//! Test: `memory_connector_reports_available_when_nothing_is_serving`,
//! `memory_connector_reads_the_version_off_a_live_socket`,
//! `memory_connector_sends_params_so_a_strict_health_handler_answers`,
//! `memory_connector_accepts_the_envelope_a_real_daemon_sends`,
//! `memory_connector_reports_degraded_when_the_daemon_answers_with_an_error`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::binary_on_path;

/// How long one health dial may take, end to end.
///
/// A local socket answers in single-digit milliseconds; trusty-memory's health
/// handler probes trusty-search before answering, so this leaves headroom over
/// that without letting one wedged service stall the console's whole detection
/// pass.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// The method name `trusty-memory`'s router registers for its health check.
///
/// Duplicated as a literal rather than imported: `trusty-console` has no Cargo
/// edge on `trusty-memory` and adding one to share a `&str` would pull an ONNX
/// embedder and a redb store into the console's build.
/// `transport::uds::METHOD_HEALTH` is the definition; this is the client's
/// copy, and the integration test in
/// `trusty-memory/tests/uds_consumer_contract.rs` is what keeps them equal.
const METHOD_HEALTH: &str = "memory.health";

/// The `result` half of an `memory.health` response, as far as the console
/// reads it.
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

/// ServiceConnector for `trusty-memory`.
///
/// Why: the console's dashboard needs to know whether the analyzer daemon is
/// running, and since #6286 that question is answered by dialling its socket.
/// What: implements `detect()` — binary on PATH, then one `memory.health` call.
/// Test: see the module docs.
pub struct MemoryConnector {
    /// Override for the socket path (used in tests).
    ///
    /// Before #6286 this was a HOME override, because the dotfile lived under
    /// `~`. The socket path comes from the data directory now, which
    /// `TRUSTY_DATA_DIR_OVERRIDE` already redirects — but that variable is
    /// process-global and this connector runs beside five others in one poll,
    /// so a path override keeps a test from redirecting its siblings too.
    socket: Option<PathBuf>,
}

impl MemoryConnector {
    /// Create a new `MemoryConnector`.
    pub fn new() -> Self {
        Self { socket: None }
    }

    /// Create a connector that dials `socket` instead of the resolved path.
    ///
    /// Why: unit tests must not dial the real user's running daemon, and the
    /// integration test needs to point this at a socket it bound itself.
    /// Test: `memory_connector_reports_available_when_nothing_is_serving`.
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    /// The socket this connector dials, or why it could not be resolved.
    ///
    /// Why the error is carried rather than discarded: a data directory that
    /// cannot be resolved or created is an operator-fixable condition
    /// (permissions, a `TRUSTY_DATA_DIR_OVERRIDE` pointing somewhere unusable),
    /// and it is indistinguishable on the dashboard from a daemon that is simply
    /// not running. `detect()` still reports `Available` — nothing was observed,
    /// so claiming otherwise would be a guess — but puts the reason in `hint`, so
    /// the card says what to fix instead of silently under-reporting.
    fn socket_path(&self) -> Result<PathBuf, String> {
        match &self.socket {
            Some(p) => Ok(p.clone()),
            None => trusty_common::daemon_socket_path("trusty-memory")
                .map_err(|e| format!("could not resolve the trusty-memory socket path: {e:#}")),
        }
    }
}

impl Default for MemoryConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// What one `memory.health` dial observed.
///
/// Why (#6356): "the daemon did not answer" and "the daemon answered, but not
/// with health" are different facts about a machine, and collapsing both into
/// `None` is what let a live daemon render as "Binary found but daemon is not
/// running" for as long as the request was malformed. A daemon that answers at
/// all is running; only the first case is silence.
/// What: three variants, one per verdict `detect_from` can reach.
/// Test: `memory_connector_reports_degraded_when_the_daemon_answers_with_an_error`.
enum ProbeOutcome {
    /// The daemon answered with a readable health envelope.
    Healthy(HealthEnvelope),
    /// The daemon answered, but not with health. Carries the operator-facing
    /// reason, which becomes the card's hint.
    Unhealthy(String),
    /// Nothing answered - no socket, no listener, or the dial timed out.
    Silent,
}

/// Dial `memory.health` and report what came back.
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
/// JSON-RPC envelope check, mapped onto [`ProbeOutcome`]. A response carrying
/// an `error` is [`ProbeOutcome::Unhealthy`], not silence - the daemon is
/// there, and reporting otherwise is what #6356 was.
///
/// Test: `memory_connector_reports_available_when_nothing_is_serving`,
/// `memory_connector_sends_params_so_a_strict_health_handler_answers`,
/// `memory_connector_reports_degraded_when_the_daemon_answers_with_an_error`.
fn probe_health(socket: &Path) -> ProbeOutcome {
    let socket = socket.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("console-memory-probe".to_owned())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return ProbeOutcome::Silent;
            };
            rt.block_on(async {
                // #6356: `memory.health` binds `HealthQuery`, and
                // `RpcRouter::typed` decodes an absent `params` as
                // `Value::Null`, which a derived `Deserialize` refuses however
                // many of its fields default. The sibling daemons bind
                // `NoParams`, whose hand-written `Deserialize` accepts null,
                // which is why only this connector was affected. `{}` takes
                // every default - the cheap health path, not the embedder
                // round-trip `probe`/`deep` would ask for.
                let request = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": METHOD_HEALTH,
                    "params": {},
                });
                let sent = trusty_common::uds::send_framed_request::<
                    _,
                    trusty_common::uds::server::RpcResponse,
                >(&socket, &request, HEALTH_TIMEOUT)
                .await;
                let Ok(response) = sent else {
                    return ProbeOutcome::Silent;
                };
                if let Some(error) = response.error {
                    return ProbeOutcome::Unhealthy(format!(
                        "trusty-memory answered {METHOD_HEALTH} with an error (code {}): {}",
                        error.code, error.message
                    ));
                }
                match response.result.map(serde_json::from_value::<HealthEnvelope>) {
                    Some(Ok(health)) => ProbeOutcome::Healthy(health),
                    _ => ProbeOutcome::Unhealthy(format!(
                        "trusty-memory answered {METHOD_HEALTH} with a body that is not a health envelope"
                    )),
                }
            })
        });
    let Ok(handle) = spawned else {
        return ProbeOutcome::Silent;
    };
    handle.join().unwrap_or(ProbeOutcome::Silent)
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
    /// Why: the console dashboard needs to know whether the daemon is up, and
    /// `tctl` makes the same call for a different reason — so the two must
    /// agree, which they do by dialling the same method on the same derived
    /// path (#6286).
    /// What: binary check → `memory.health` over the socket → status. `url` is
    /// deliberately `None`: a UDS daemon has no URL, and ADR-0032 makes
    /// trusty-console the only HTTP surface in the workspace, so a synthesised
    /// `http://` address would be a link that cannot work. A socket path that
    /// cannot be resolved reports `Available` with the reason in `hint` — see
    /// [`MemoryConnector::socket_path`].
    /// Test: `memory_connector_reports_available_when_nothing_is_serving`,
    /// `memory_connector_reads_the_version_off_a_live_socket`,
    /// `memory_connector_surfaces_an_unresolvable_socket_path_as_a_hint`,
    /// `memory_connector_reports_degraded_when_the_daemon_answers_with_an_error`.
    fn detect(&self) -> ServiceInfo {
        self.detect_from(self.socket_path())
    }
}

impl MemoryConnector {
    /// [`ServiceConnector::detect`]'s body, over an already-resolved path.
    ///
    /// Why: the unresolvable-path arm is only reachable when
    /// `trusty_common::daemon_socket_path` fails, and the only way to make it
    /// fail from a test is to set `TRUSTY_DATA_DIR_OVERRIDE` — which is
    /// process-global and, in this crate's test binary, is read by five sibling
    /// connectors running in parallel. Taking the resolved result as a parameter
    /// makes the arm assertable with no global state at all.
    /// What: binary check, then the four verdicts.
    /// Test: `memory_connector_surfaces_an_unresolvable_socket_path_as_a_hint`,
    /// `memory_connector_reports_degraded_when_the_daemon_answers_with_an_error`.
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

        if !binary_on_path("trusty-memory") {
            return base(ServiceStatus::Absent, None, None);
        }

        let socket = match socket {
            Ok(p) => p,
            Err(reason) => return base(ServiceStatus::Available, None, Some(reason)),
        };

        match probe_health(&socket) {
            ProbeOutcome::Healthy(health) => base(ServiceStatus::Running, health.version, None),
            // #6356: a daemon that answers is running, so the row says so
            // rather than repeating the "not running" line an operator has
            // already disproved by seeing the socket. `Degraded` is the row
            // model's existing "reachable, but not answering as expected"
            // state, and the card renders its hint verbatim.
            ProbeOutcome::Unhealthy(reason) => base(ServiceStatus::Degraded, None, Some(reason)),
            ProbeOutcome::Silent => base(ServiceStatus::Available, None, None),
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Bind `socket` and answer exactly one frame with whatever `reply` makes
    /// of the request it received.
    ///
    /// Why: three tests need a `memory.health` responder and differ only in
    /// what it answers - one accepts anything, one mirrors trusty-memory's
    /// `HealthQuery` decode, one refuses everything. Sharing the
    /// accept-read-write half leaves that difference as the only thing each
    /// test states.
    /// What: binds a hardened socket, reads the request frame to EOF (the
    /// client half-closes its write side after sending), and writes
    /// `reply(frame)` followed by the newline the framing terminates on.
    /// Test: used by the three socket-backed tests below.
    fn spawn_health_socket(
        socket: &Path,
        reply: impl FnOnce(serde_json::Value) -> String + Send + 'static,
    ) {
        let listener = trusty_common::uds::bind_hardened(socket).expect("bind");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut raw = Vec::new();
            let _ = conn.read_to_end(&mut raw).await;
            let frame = serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
            let _ = conn.write_all(reply(frame).as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });
    }

    /// Why (#6286): the pre-migration connector read a dotfile nothing rewrites
    /// any more, so any process holding the port it named made this report a
    /// trusty-memory that was not there. The file read is gone, and this is what
    /// keeps it gone: an absent socket is `Available`, never `Running`, whatever
    /// else is listening on the machine.
    /// What: points the connector at a path in an empty temp dir and asserts the
    /// verdict, branching only on whether the binary is installed.
    /// Test: this is the test.
    #[test]
    fn memory_connector_reports_available_when_nothing_is_serving() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let connector = MemoryConnector::with_socket(tmp.path().join("absent.sock"));
        let info = connector.detect();

        let expected = if which::which("trusty-memory").is_ok() {
            ServiceStatus::Available
        } else {
            ServiceStatus::Absent
        };
        assert_eq!(info.status, expected);
        assert_eq!(info.id, "trusty-memory");
        assert_eq!(info.display_name, "Trusty Memory");
        assert!(info.url.is_none(), "a UDS daemon has no URL to render");
        assert!(
            info.status != ServiceStatus::Absent || info.version.is_none(),
            "Absent must have no version"
        );
    }

    /// Why (#6286): a data directory that cannot be resolved is operator-fixable
    /// — a permissions problem, or a `TRUSTY_DATA_DIR_OVERRIDE` pointing
    /// somewhere unusable — but on the dashboard it looks identical to a daemon
    /// that is merely stopped. Reporting the reason turns a silent under-report
    /// into something actionable, without upgrading the verdict.
    /// What: a resolution failure reports `Available` carrying the reason.
    /// Test: this is the test.
    #[test]
    fn memory_connector_surfaces_an_unresolvable_socket_path_as_a_hint() {
        if which::which("trusty-memory").is_err() {
            eprintln!("skip: trusty-memory is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let info = MemoryConnector::new().detect_from(Err(
            "could not resolve the trusty-memory socket path: nope".to_string(),
        ));

        assert_eq!(
            info.status,
            ServiceStatus::Available,
            "nothing was observed, so the verdict must not claim more than that"
        );
        let hint = info.hint.expect("an unresolvable path must explain itself");
        assert!(
            hint.contains("socket path"),
            "the hint must name what could not be resolved: {hint}"
        );
    }

    /// Why: the hint is for the failure case only. A connector that attached one
    /// to a healthy or merely-stopped daemon would put a permanent "something is
    /// wrong" note on a card where nothing is.
    /// Test: this is the test.
    #[test]
    fn memory_connector_attaches_no_hint_when_the_path_resolves() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let info = MemoryConnector::new().detect_from(Ok(tmp.path().join("absent.sock")));
        assert!(
            info.hint.is_none(),
            "a resolvable path must not carry a remediation hint: {:?}",
            info.hint
        );
    }

    /// Why: `Running` is the verdict that has to be earned by an ANSWER, and the
    /// version it carries is what the card renders. A connector that reported
    /// Running off a bare connect would have no version to show and would call a
    /// wedged daemon healthy.
    /// What: binds a socket that answers one `memory.health` frame with a real
    /// envelope, and asserts the connector reads the version off it.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn memory_connector_reads_the_version_off_a_live_socket() {
        if which::which("trusty-memory").is_err() {
            eprintln!("skip: trusty-memory is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("memory.sock");
        spawn_health_socket(&socket, |_frame| {
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"9.9.9","search_reachable":true}}"#.to_string()
        });

        let connector = MemoryConnector::with_socket(socket);
        let info = tokio::task::spawn_blocking(move || connector.detect())
            .await
            .expect("detect");

        assert_eq!(info.status, ServiceStatus::Running);
        assert_eq!(info.version.as_deref(), Some("9.9.9"));
    }

    /// Why (#6356): the probe sent no `params` at all, and trusty-memory 0.25.2
    /// answers `-32602` - "params do not decode: invalid type: null, expected
    /// struct HealthQuery" - instead of health, so a running daemon rendered as
    /// "Available - Binary found but daemon is not running". This is the test
    /// that fails without the `"params": {}` the request now carries.
    /// What: the responder mirrors the daemon's own decode - an object `params`
    /// is accepted, anything else is refused exactly as the real handler
    /// refuses it - and the connector must come back Running with the version.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn memory_connector_sends_params_so_a_strict_health_handler_answers() {
        if which::which("trusty-memory").is_err() {
            eprintln!("skip: trusty-memory is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("memory.sock");
        spawn_health_socket(&socket, |frame| {
            match frame.get("params") {
                Some(serde_json::Value::Object(_)) => r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"0.25.2"}}"#.to_string(),
                _ => r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"params do not decode: invalid type: null, expected struct HealthQuery"}}"#.to_string(),
            }
        });

        let connector = MemoryConnector::with_socket(socket);
        let info = tokio::task::spawn_blocking(move || connector.detect())
            .await
            .expect("detect");

        assert_eq!(
            info.status,
            ServiceStatus::Running,
            "a daemon that answers health must read as Running, not {:?} (hint: {:?})",
            info.status,
            info.hint
        );
        assert_eq!(info.version.as_deref(), Some("0.25.2"));
    }

    /// Why (#6356): the two earlier tests reply with a hand-written envelope
    /// carrying exactly the two fields `HealthEnvelope` names, so neither one
    /// can catch the connector refusing what the daemon actually sends. This
    /// replies with a frame captured verbatim from trusty-memory 0.25.2 over
    /// its live socket — eight extra fields, including a nested `worker`
    /// object — because the failure mode that recurred on 2026-08-28 was a
    /// running daemon reading as stopped, and a `HealthEnvelope` that grew a
    /// required field would reproduce it exactly.
    /// What: the real answer, unedited, must read as Running with its version.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn memory_connector_accepts_the_envelope_a_real_daemon_sends() {
        if which::which("trusty-memory").is_err() {
            eprintln!("skip: trusty-memory is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("memory.sock");
        spawn_health_socket(&socket, |_frame| {
            r#"{"jsonrpc":"2.0","id":1,"result":{"cpu_pct":2.501335620880127,"daemon_state":"ready","disk_bytes":0,"fd_soft_limit":8192,"open_fds":22,"rss_mb":5151,"socket":"/Users/x/Library/Application Support/trusty-memory/trusty-memory.sock","status":"ok","uptime_secs":12124,"version":"0.25.2","worker":{"in_flight":0,"wedged":false}}}"#
                .to_string()
        });

        let connector = MemoryConnector::with_socket(socket);
        let info = tokio::task::spawn_blocking(move || connector.detect())
            .await
            .expect("detect");

        assert_eq!(
            info.status,
            ServiceStatus::Running,
            "a real daemon's own health frame must read as Running (hint: {:?})",
            info.hint
        );
        assert_eq!(info.version.as_deref(), Some("0.25.2"));
    }

    /// Why (#6356): telling "answered wrongly" apart from "did not answer" must
    /// not turn the probe fail-open. An error answer is still not health, so
    /// the row must never claim `Running` or invent a version - it reports the
    /// daemon as reachable-but-wrong and hands the operator the error verbatim.
    /// What: a responder that refuses every call, whatever it is sent.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn memory_connector_reports_degraded_when_the_daemon_answers_with_an_error() {
        if which::which("trusty-memory").is_err() {
            eprintln!("skip: trusty-memory is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("memory.sock");
        spawn_health_socket(&socket, |_frame| {
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#
                .to_string()
        });

        let connector = MemoryConnector::with_socket(socket);
        let info = tokio::task::spawn_blocking(move || connector.detect())
            .await
            .expect("detect");

        assert_ne!(
            info.status,
            ServiceStatus::Running,
            "an error answer is not health, however reachable the daemon is"
        );
        assert_eq!(
            info.status,
            ServiceStatus::Degraded,
            "a daemon that answers is running, so the row must not read Available"
        );
        assert!(
            info.version.is_none(),
            "there was no health envelope to read a version off: {:?}",
            info.version
        );
        let hint = info.hint.expect("an error answer must explain itself");
        assert!(
            hint.contains("-32601") && hint.contains("method not found"),
            "the hint must carry the daemon's own error verbatim: {hint}"
        );
    }
}
