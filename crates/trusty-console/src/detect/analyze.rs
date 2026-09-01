//! `ServiceConnector` implementation for `trusty-analyze`.
//!
//! Why: trusty-analyze served TCP loopback HTTP and published its bound address
//! in `~/.trusty-analyze/http_addr`; this connector read that file, probed the
//! port, and fell back to probing 7879 when the file was absent. #6287
//! (ADR-0032) moved the daemon onto a hardened Unix socket, so all three of
//! those are gone — there is no port, no discovery file, and nothing to fall
//! back FROM: the socket path is derived, and the daemon and this connector
//! resolve it through the same `trusty_common::daemon_socket_path` call.
//!
//! The retired fallback probed `127.0.0.1:7879` whenever the file was missing,
//! which is the same shape of bug #6277 removed from the review connector: any
//! process that took 7879 read as a healthy trusty-analyze. It is deleted rather
//! than corrected.
//!
//! What: `AnalyzeConnector::detect()` dials `analyze.health` over the socket and
//! reads `version` off the answer. When nothing answers — the resting state of
//! an on-demand server (#6350) — the verdict comes off the binary instead, the
//! way the trusty-review connector's has since #6290.
//! Test: `analyze_connector_reports_available_when_nothing_is_serving`,
//! `analyze_connector_reads_the_version_off_a_live_socket`,
//! `analyze_reports_an_on_demand_lifecycle_on_every_verdict`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceLifecycle, ServiceStatus};

use super::helpers::{VersionProbe, binary_on_path, binary_version};

/// The binary this connector reports on.
const BINARY: &str = "trusty-analyze";

/// How long one health dial may take, end to end.
///
/// A local socket answers in single-digit milliseconds; trusty-analyze's health
/// handler probes trusty-search before answering, so this leaves headroom over
/// that without letting one wedged service stall the console's whole detection
/// pass.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// The method name `trusty-analyze`'s router registers for its health check.
///
/// Duplicated as a literal rather than imported: `trusty-console` has no Cargo
/// edge on `trusty-analyze` and adding one to share a `&str` would pull a
/// tree-sitter analysis engine into the console's build.
/// `service::rpc::METHOD_HEALTH` is the definition; this is the client's copy,
/// and the integration test in `trusty-analyze/tests/uds_consumer_contract.rs`
/// is what keeps them equal.
const METHOD_HEALTH: &str = "analyze.health";

/// The `result` half of an `analyze.health` response, as far as the console
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

/// ServiceConnector for `trusty-analyze`.
///
/// Why: the console's dashboard needs to know whether the analyzer daemon is
/// running, and since #6287 that question is answered by dialling its socket.
/// What: implements `detect()` — binary on PATH, then one `analyze.health` call.
/// Test: see the module docs.
pub struct AnalyzeConnector {
    /// Override for the socket path (used in tests).
    ///
    /// Before #6287 this was a HOME override, because the discovery file lived
    /// under `~`. The socket path comes from the data directory now, which
    /// `TRUSTY_DATA_DIR_OVERRIDE` already redirects — but that variable is
    /// process-global and this connector runs beside five others in one poll,
    /// so a path override keeps a test from redirecting its siblings too.
    socket: Option<PathBuf>,
}

impl AnalyzeConnector {
    /// Create a new `AnalyzeConnector`.
    pub fn new() -> Self {
        Self { socket: None }
    }

    /// Create a connector that dials `socket` instead of the resolved path.
    ///
    /// Why: unit tests must not dial the real user's running daemon, and the
    /// integration test needs to point this at a socket it bound itself.
    /// Test: `analyze_connector_reports_available_when_nothing_is_serving`.
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
            None => trusty_common::daemon_socket_path("trusty-analyze")
                .map_err(|e| format!("could not resolve the trusty-analyze socket path: {e:#}")),
        }
    }
}

impl Default for AnalyzeConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Dial `analyze.health` and return the envelope, or `None` if nothing answered.
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
/// Test: `analyze_connector_reports_available_when_nothing_is_serving`.
fn probe_health(socket: &Path) -> Option<HealthEnvelope> {
    let socket = socket.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("console-analyze-probe".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async {
                // #6555: a params-less frame decodes to null, which a struct-bound method rejects with -32602.
                let request = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": METHOD_HEALTH,
                    "params": {},
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

impl ServiceConnector for AnalyzeConnector {
    fn id(&self) -> &'static str {
        "trusty-analyze"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Analyze"
    }

    // #6416: #6287 moved it to a socket and #6350 made that server on-demand, so
    // a resident process is not what healthy looks like here.
    fn lifecycle(&self) -> ServiceLifecycle {
        ServiceLifecycle::OnDemand
    }

    /// Detect trusty-analyze status.
    ///
    /// Why: the console dashboard needs to know whether the daemon is up, and
    /// `tctl` makes the same call for a different reason — so the two must
    /// agree, which they do by dialling the same method on the same derived
    /// path (#6287).
    /// What: binary check → `analyze.health` over the socket → status, falling
    /// back to `trusty-analyze --version` when nothing answers. `url` is
    /// deliberately `None`: a UDS daemon has no URL, and ADR-0032 makes
    /// trusty-console the only HTTP surface in the workspace, so a synthesised
    /// `http://` address would be a link that cannot work. A socket path that
    /// cannot be resolved reports `Available` with the reason in `hint` — see
    /// [`AnalyzeConnector::socket_path`].
    /// Test: `analyze_connector_reports_available_when_nothing_is_serving`,
    /// `analyze_connector_reads_the_version_off_a_live_socket`,
    /// `analyze_connector_surfaces_an_unresolvable_socket_path_as_a_hint`.
    fn detect(&self) -> ServiceInfo {
        self.detect_from(self.socket_path())
    }
}

impl AnalyzeConnector {
    /// [`ServiceConnector::detect`]'s body, over an already-resolved path.
    ///
    /// Why: the unresolvable-path arm is only reachable when
    /// `trusty_common::daemon_socket_path` fails, and the only way to make it
    /// fail from a test is to set `TRUSTY_DATA_DIR_OVERRIDE` — which is
    /// process-global and, in this crate's test binary, is read by five sibling
    /// connectors running in parallel. Taking the resolved result as a parameter
    /// makes the arm assertable with no global state at all.
    /// What: binary check, then the three verdicts.
    ///
    /// 🔴 **This connector deliberately does not call `ensure_running`** (#6350),
    /// unlike every other analyze client. It is a DETECTOR, and the console
    /// renders it on a poll loop: a detector that started the service would keep
    /// an on-demand server alive for as long as anyone had the dashboard open —
    /// the exact outcome the idle window exists to prevent — and would then
    /// report `Running` about a process it had just created, which is not an
    /// observation.
    ///
    /// What that changes about the verdicts, now that resident is not the
    /// healthy state: `Absent` (not installed) and `Degraded` (installed but
    /// `--version` will not run) are the bad ones. `Available` means installed
    /// and startable, which for an on-demand service is its correct resting
    /// state, not a degradation. `Running` means a server happens to be up right
    /// now — a client is using it, or one has not yet reached its idle window.
    ///
    /// Test: `analyze_connector_surfaces_an_unresolvable_socket_path_as_a_hint`,
    /// `detect_never_starts_a_server`,
    /// `analyze_reports_an_on_demand_lifecycle_on_every_verdict`,
    /// `analyze_reads_a_version_off_the_binary_when_nothing_is_serving`.
    fn detect_from(&self, socket: Result<PathBuf, String>) -> ServiceInfo {
        let base =
            |status: ServiceStatus, version: Option<String>, hint: Option<String>| ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status,
                version,
                url: None,
                hint,
                // #6416: trusty-analyze serves on demand since #6287/#6350, so
                // `Available` is its resting state and the card must not offer
                // to start a daemon.
                lifecycle: self.lifecycle(),
            };

        if !binary_on_path(BINARY) {
            return base(ServiceStatus::Absent, None, None);
        }

        // An unresolvable socket path leaves nothing to dial, but the binary
        // question is still answerable — so the reason rides along as the hint
        // rather than short-circuiting the verdict.
        let (socket_hint, dialled) = match socket {
            Ok(path) => (None, probe_health(&path)),
            Err(reason) => (Some(reason), None),
        };

        if let Some(health) = dialled {
            return base(ServiceStatus::Running, health.version, None);
        }

        // #6416: nothing is serving, which for an on-demand member is healthy.
        // The verdict comes off the binary, exactly as trusty-review's does.
        match binary_version(BINARY) {
            VersionProbe::Ran(version) => base(ServiceStatus::Available, version, socket_hint),
            VersionProbe::CannotExecute(why) => base(
                ServiceStatus::Degraded,
                None,
                Some(format!(
                    "{BINARY} is on PATH but did not run: {why}. Reinstall it \
                     with `cargo install {BINARY}`."
                )),
            ),
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6287): the pre-migration connector fell back to probing
    /// `127.0.0.1:7879` when its discovery file was missing, so any process
    /// holding that port made this report a trusty-analyze that was not there.
    /// The fallback is gone, and this is what keeps it gone: an absent socket is
    /// `Available`, never `Running`, whatever else is listening on the machine.
    /// What: points the connector at a path in an empty temp dir and asserts the
    /// verdict, branching only on whether the binary is installed.
    /// Test: this is the test.
    #[test]
    fn analyze_connector_reports_available_when_nothing_is_serving() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let connector = AnalyzeConnector::with_socket(tmp.path().join("absent.sock"));
        let info = connector.detect();

        let expected = if which::which("trusty-analyze").is_ok() {
            ServiceStatus::Available
        } else {
            ServiceStatus::Absent
        };
        assert_eq!(info.status, expected);
        assert_eq!(info.id, "trusty-analyze");
        assert_eq!(info.display_name, "Trusty Analyze");
        assert!(info.url.is_none(), "a UDS daemon has no URL to render");
        assert!(
            info.status != ServiceStatus::Absent || info.version.is_none(),
            "Absent must have no version"
        );
    }

    /// Why (#6287): a data directory that cannot be resolved is operator-fixable
    /// — a permissions problem, or a `TRUSTY_DATA_DIR_OVERRIDE` pointing
    /// somewhere unusable — but on the dashboard it looks identical to a daemon
    /// that is merely stopped. Reporting the reason turns a silent under-report
    /// into something actionable, without upgrading the verdict.
    /// What: a resolution failure reports `Available` carrying the reason.
    /// Test: this is the test.
    #[test]
    fn analyze_connector_surfaces_an_unresolvable_socket_path_as_a_hint() {
        if which::which("trusty-analyze").is_err() {
            eprintln!("skip: trusty-analyze is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let info = AnalyzeConnector::new().detect_from(Err(
            "could not resolve the trusty-analyze socket path: nope".to_string(),
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
    fn analyze_connector_attaches_no_hint_when_the_path_resolves() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let info = AnalyzeConnector::new().detect_from(Ok(tmp.path().join("absent.sock")));
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
    /// What: binds a socket that answers one `analyze.health` frame with a real
    /// envelope, and asserts the connector reads the version off it.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn analyze_connector_reads_the_version_off_a_live_socket() {
        if which::which("trusty-analyze").is_err() {
            eprintln!("skip: trusty-analyze is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("analyze.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let reply =
                br#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"9.9.9","search_reachable":true}}"#;
            let _ = conn.write_all(reply).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });

        let connector = AnalyzeConnector::with_socket(socket);
        let info = tokio::task::spawn_blocking(move || connector.detect())
            .await
            .expect("detect");

        assert_eq!(info.status, ServiceStatus::Running);
        assert_eq!(info.version.as_deref(), Some("9.9.9"));
    }

    /// Why (#6350): the console polls `detect` while a dashboard is open. If it
    /// started trusty-analyze, an open browser tab would pin an on-demand
    /// server resident forever — and the connector would be reporting on a
    /// process it created rather than one it found.
    /// What: points the connector at a socket path inside a tempdir, calls
    /// `detect`, and asserts nothing bound it.
    /// Test: this is the test.
    #[test]
    fn detect_never_starts_a_server() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("must-stay-absent.sock");
        let info = AnalyzeConnector::with_socket(socket.clone()).detect();

        assert_ne!(
            info.status,
            ServiceStatus::Running,
            "nothing was serving that path, so no verdict may claim it was"
        );
        assert!(
            !socket.exists(),
            "detect must observe, never start: {} was created",
            socket.display()
        );
    }

    /// REGRESSION (#6416): the dashboard read "Binary found but daemon is not
    /// running" over the Trusty Analyze card, in amber, for a service #6287 and
    /// #6350 made on-demand — so "nothing is serving" is what healthy looks
    /// like here and the card was rendering it as a fault.
    ///
    /// Why: the assertion is on the SERIALISED payload because that JSON, not
    /// the Rust struct, is what the Svelte card branches on.
    /// What: the nothing-is-serving verdict must carry `"on_demand"`, and so
    /// must the not-installed one.
    /// Test: this is the test.
    #[test]
    fn analyze_reports_an_on_demand_lifecycle_on_every_verdict() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        for socket in [Ok(tmp.path().join("absent.sock")), Err("nope".to_string())] {
            let payload =
                serde_json::to_value(AnalyzeConnector::new().detect_from(socket)).expect("json");
            assert_eq!(
                payload.get("lifecycle"),
                Some(&serde_json::json!("on_demand")),
                "the card branches on this key: {payload}"
            );
        }
    }

    /// Why (#6416): an on-demand row is "installed + version = healthy", and
    /// before this the resting-state card showed no version at all — it only
    /// ever read one off a live socket, which for an idle server is never.
    /// What: with nothing serving, the version comes off `--version`.
    /// Test: this is the test.
    #[test]
    fn analyze_reads_a_version_off_the_binary_when_nothing_is_serving() {
        if which::which(BINARY).is_err() {
            eprintln!("skip: trusty-analyze is not on PATH, so detect() short-circuits to Absent");
            return;
        }
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let info = AnalyzeConnector::new().detect_from(Ok(tmp.path().join("absent.sock")));

        assert_eq!(info.status, ServiceStatus::Available);
        assert!(
            info.version.is_some(),
            "an installed on-demand member renders the version it prints"
        );
    }
}
