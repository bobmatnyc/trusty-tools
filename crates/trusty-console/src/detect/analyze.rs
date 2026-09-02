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
//! What: `AnalyzeConnector::detect()` decides Running from a connect-only probe
//! and reads `version` off one `analyze.health` call per daemon lifetime. When
//! nothing answers — the resting state of an on-demand server (#6350) — the
//! verdict comes off the binary instead, the way the trusty-review connector's
//! has since #6290.
//!
//! #6621: the health call used to run on every poll, four times a minute
//! against a 600s idle window, and re-armed that window every time. The idle
//! accounting in `trusty_common::uds::server` now exempts the method outright;
//! this connector additionally stops asking, because the answer does not change
//! while one server is up. Only the version needs the RPC, and the version is a
//! property of the process — so it is read once and cached against the socket's
//! inode, which a respawn replaces.
//!
//! Test: `analyze_connector_reports_available_when_nothing_is_serving`,
//! `analyze_connector_reads_the_version_off_a_live_socket`,
//! `analyze_detect_dials_health_once_per_socket_identity`,
//! `analyze_reports_an_on_demand_lifecycle_on_every_verdict`.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
    /// The version last read, and the socket it was read from (#6621).
    ///
    /// One entry, not a map: a connector watches one path, and a new identity
    /// means the previous server is gone. `Mutex` because `detect` takes `&self`
    /// — the poller holds this connector across every poll, which is what makes
    /// the cache worth having.
    version: Mutex<Option<VersionForSocket>>,
}

/// A version reading, bound to the socket instance it came from (#6621).
///
/// Why device and inode rather than the path: the path is stable across the
/// daemon's whole life AND across every respawn, so it cannot distinguish one
/// process's version from the next one's. An on-demand server unlinks its socket
/// on the way out and the successor binds a fresh file, so the inode changes
/// exactly when the answer might.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    /// The socket file's device id.
    dev: u64,
    /// The socket file's inode.
    ino: u64,
}

/// What [`AnalyzeConnector::version`] holds.
#[derive(Debug)]
struct VersionForSocket {
    /// The socket instance the version was read from.
    identity: SocketIdentity,
    /// The version, or `None` when the daemon answered without one.
    version: Option<String>,
}

/// The socket file's identity, or `None` when there is no file to stat.
///
/// An absent file is the resting state of an on-demand service, not an error.
fn socket_identity(socket: &Path) -> Option<SocketIdentity> {
    let meta = std::fs::metadata(socket).ok()?;
    Some(SocketIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

impl AnalyzeConnector {
    /// Create a new `AnalyzeConnector`.
    pub fn new() -> Self {
        Self {
            socket: None,
            version: Mutex::new(None),
        }
    }

    /// Create a connector that dials `socket` instead of the resolved path.
    ///
    /// Why: unit tests must not dial the real user's running daemon, and the
    /// integration test needs to point this at a socket it bound itself.
    /// Test: `analyze_connector_reports_available_when_nothing_is_serving`.
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
            version: Mutex::new(None),
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

/// Run one async probe against `socket` on a thread of its own.
///
/// Why: `ServiceConnector::detect` is synchronous — the poller calls it inside
/// `spawn_blocking` — and the shared UDS client is async. The exchange runs on
/// a dedicated thread with its own current-thread runtime rather than through
/// `Handle::block_on`, for the reason `trusty-installer`'s
/// `probe_member_http_blocking` records: building a runtime and blocking on it
/// from inside another runtime's worker panics, and this way the call is safe
/// from any caller regardless of what it is running on.
///
/// A thread that will not spawn, a runtime that will not build, or a panicking
/// probe all read as `None` — the same verdict as nothing answering, which is
/// the honest one when nothing was observed.
fn blocking_probe<T, F>(socket: &Path, probe: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(PathBuf) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<T>>>>
        + Send
        + 'static,
{
    let socket = socket.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("console-analyze-probe".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(probe(socket))
        })
        .ok()?;
    handle.join().ok()?
}

/// Whether something is accepting connections on `socket` right now (#6621).
///
/// Why this and not `analyze.health`: a connect-and-close answers the only
/// question the dashboard asks every poll, and it is the one probe an on-demand
/// server's idle accounting has always exempted — it reaches the server as
/// `Served::LivenessProbe`, before any frame is read.
fn probe_is_serving(socket: &Path) -> bool {
    blocking_probe(socket, |socket| {
        Box::pin(async move {
            trusty_common::uds::socket_is_serving(&socket, HEALTH_TIMEOUT)
                .await
                .then_some(())
        })
    })
    .is_some()
}

/// Dial `analyze.health` and return the envelope, or `None` if nothing answered.
///
/// What: one `send_framed_request` bounded by [`HEALTH_TIMEOUT`], then a
/// JSON-RPC envelope check. A response carrying an `error` is `None`: the
/// daemon answered, but not with health, and the console has nothing to render.
///
/// #6621: called at most once per socket instance — see
/// [`AnalyzeConnector::version_for`].
///
/// Test: `analyze_connector_reads_the_version_off_a_live_socket`,
/// `analyze_detect_dials_health_once_per_socket_identity`.
fn probe_health(socket: &Path) -> Option<HealthEnvelope> {
    blocking_probe(socket, |socket| {
        Box::pin(async move {
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
    /// `analyze_detect_dials_health_once_per_socket_identity`,
    /// `analyze_reads_a_version_off_the_binary_when_nothing_is_serving`.
    /// The running daemon's version, dialling `analyze.health` at most once per
    /// socket instance (#6621).
    ///
    /// Why a cache and not simply a slower poll: a version does not change while
    /// one process is up, so a second reading of it carries no information —
    /// only an idle-window re-arm. Keying on the socket's inode is what makes
    /// "while one process is up" observable from outside: an on-demand server
    /// unlinks its socket on exit and the next one binds a new file.
    ///
    /// What: returns the cached version when the identity matches, otherwise
    /// dials once and stores what came back. A dial that answers without a
    /// version is cached too — repeating it would not produce one, and the
    /// verdict is `Running` either way because the connect probe already
    /// observed a server.
    ///
    /// A socket that cannot be stat'd is not cached: something is serving it (a
    /// caller only reaches here past the connect probe) but there is no identity
    /// to key on, so the next poll asks again rather than caching against a key
    /// it cannot recheck.
    ///
    /// Test: `analyze_detect_dials_health_once_per_socket_identity`,
    /// `analyze_version_cache_is_reread_when_the_socket_is_replaced`.
    fn version_for(&self, socket: &Path) -> Option<String> {
        let identity = socket_identity(socket);
        let mut cached = self.version.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(identity), Some(entry)) = (identity, cached.as_ref())
            && entry.identity == identity
        {
            return entry.version.clone();
        }

        let version = probe_health(socket).and_then(|health| health.version);
        *cached = identity.map(|identity| VersionForSocket {
            identity,
            version: version.clone(),
        });
        version
    }

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
        // #6621: Running is decided by the connect probe, which the server's
        // idle accounting has always exempted; the version comes off at most one
        // `analyze.health` per socket instance.
        let (socket_hint, running) = match socket {
            Ok(path) if probe_is_serving(&path) => (None, Some(self.version_for(&path))),
            Ok(_) => (None, None),
            Err(reason) => (Some(reason), None),
        };

        if let Some(version) = running {
            return base(ServiceStatus::Running, version, None);
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

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// Serve `socket` as a fake analyzer, counting the request FRAMES it is
    /// sent.
    ///
    /// Why the count is of frames and not of connections: the connect-only
    /// liveness probe is a connection that sends nothing, and it is precisely
    /// the traffic #6621 wants unbounded. What must stay rare is the frame — the
    /// `analyze.health` RPC that used to re-arm the idle window.
    ///
    /// The task loops rather than serving once: `detect` now opens two
    /// connections on its first pass and one on every pass after it.
    fn spawn_fake_analyzer(socket: &Path) -> Arc<AtomicUsize> {
        let listener = trusty_common::uds::bind_hardened(socket).expect("bind");
        let frames = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&frames);
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
            loop {
                let Ok((conn, _)) = listener.accept().await else {
                    return;
                };
                let counted = Arc::clone(&counted);
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(conn);
                    let mut frame = String::new();
                    // A connect-and-close reads zero bytes and is not a request.
                    if reader.read_line(&mut frame).await.unwrap_or(0) == 0 {
                        return;
                    }
                    counted.fetch_add(1, Ordering::SeqCst);
                    let reply = br#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"9.9.9","search_reachable":true}}"#;
                    let conn = reader.get_mut();
                    let _ = conn.write_all(reply).await;
                    let _ = conn.write_all(b"\n").await;
                    let _ = conn.flush().await;
                });
            }
        });
        frames
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn analyze_connector_reads_the_version_off_a_live_socket() {
        if which::which("trusty-analyze").is_err() {
            eprintln!("skip: trusty-analyze is not on PATH, so detect() short-circuits to Absent");
            return;
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("analyze.sock");
        spawn_fake_analyzer(&socket);

        let connector = AnalyzeConnector::with_socket(socket);
        let info = tokio::task::spawn_blocking(move || connector.detect())
            .await
            .expect("detect");

        assert_eq!(info.status, ServiceStatus::Running);
        assert_eq!(info.version.as_deref(), Some("9.9.9"));
    }

    /// REGRESSION (#6621): this connector dialled `analyze.health` on every
    /// poll — four times a minute against trusty-analyze's 600s idle window — so
    /// an open dashboard re-armed that window forever and the on-demand server
    /// it was watching stayed resident for 46 hours.
    ///
    /// Why the assertion is a frame COUNT rather than a cadence in seconds: the
    /// poll interval is the operator's to set, so a fix expressed as "at most
    /// once per N seconds" is only as good as N. Once per daemon lifetime holds
    /// at any interval.
    /// What: three `detect` passes over one live socket send exactly one request
    /// frame, and every pass still reports Running with the version.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn analyze_detect_dials_health_once_per_socket_identity() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("analyze.sock");
        let frames = spawn_fake_analyzer(&socket);

        let connector = Arc::new(AnalyzeConnector::with_socket(socket.clone()));
        for pass in 0..3 {
            let connector = Arc::clone(&connector);
            let dialled = socket.clone();
            let version = tokio::task::spawn_blocking(move || connector.version_for(&dialled))
                .await
                .expect("probe");
            assert_eq!(version.as_deref(), Some("9.9.9"), "pass {pass}");
        }

        assert_eq!(
            frames.load(Ordering::SeqCst),
            1,
            "monitoring must not issue an idle-re-arming call on every poll"
        );
    }

    /// The other half of the cache contract: a version cached against a socket
    /// that has since been replaced is stale, and must be re-read.
    ///
    /// Why it matters: an on-demand server exits and a successor binds a new
    /// file at the same path, so a cache keyed on the PATH would render the
    /// previous process's version indefinitely after an upgrade.
    /// What: reads a version, replaces the socket, and asserts a second frame
    /// was sent.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn analyze_version_cache_is_reread_when_the_socket_is_replaced() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("sockets").join("analyze.sock");
        let first = spawn_fake_analyzer(&socket);

        let connector = Arc::new(AnalyzeConnector::with_socket(socket.clone()));
        let probe = |connector: Arc<AnalyzeConnector>, socket: PathBuf| async move {
            tokio::task::spawn_blocking(move || connector.version_for(&socket))
                .await
                .expect("probe")
        };
        assert_eq!(
            probe(Arc::clone(&connector), socket.clone())
                .await
                .as_deref(),
            Some("9.9.9")
        );
        assert_eq!(first.load(Ordering::SeqCst), 1);

        std::fs::remove_file(&socket).expect("unlink");
        let second = spawn_fake_analyzer(&socket);
        assert_eq!(
            probe(connector, socket).await.as_deref(),
            Some("9.9.9"),
            "a replaced socket is a new process, so its version is read again"
        );
        assert_eq!(
            second.load(Ordering::SeqCst),
            1,
            "the successor must have been dialled exactly once"
        );
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
