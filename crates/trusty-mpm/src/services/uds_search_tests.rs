//! #9543: `tm services` finds trusty-search by socket health, never by TCP.
//!
//! Why: trusty-search 0.59.0 binds no TCP port (ADR-0032). A discoverer that
//! probes `http://localhost:7878/health` reports that live daemon DOWN, and a
//! user manifest written by an older `tm services init` still names 7878.
//! What: the UP arm against a mock daemon answering `search.health`, the DOWN
//! arm against a missing, a stale and a hung socket, the default manifest's
//! shape, the HTTP prober's zero call count, the `port`/`url` refusal naming
//! the socket, and the legacy user-manifest mapping that never writes the file.
//! Test: these tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serial_test::serial;

use crate::daemon::search_rpc::{METHOD_HEALTH, TRUSTY_SEARCH_SOCKET_ENV};
use crate::secret_source::test_env::EnvVarGuard;
use crate::services::discoverer::{
    HttpProber, PortProber, ProcessProber, RealSearchSocketProber, SearchSocketProber,
    VersionRunner,
};
use crate::services::{
    Discoverer, HEALTH_PROBE_TIMEOUT, HealthProbe, HealthState, ServicesManifest,
    load_user_manifest,
};

const SEARCH: &str = "trusty-search";

// ── Mocks: no process, no port file, no version, and a counting HTTP prober ──

struct NoProcess;

impl ProcessProber for NoProcess {
    fn pgrep(&self, _pattern: &str) -> Option<u32> {
        None
    }
}

struct NoPortFile;

impl PortProber for NoPortFile {
    fn read_port_file(&self, _path: &Path) -> Option<u16> {
        None
    }
}

struct NoVersion;

impl VersionRunner for NoVersion {
    fn run(&self, _cmd: &str) -> Option<String> {
        None
    }
}

/// Answers `Ok` to every HTTP probe and counts them; a socket service must
/// leave the count at zero.
#[derive(Clone, Default)]
struct CountingHttp(Arc<Mutex<u32>>);

impl CountingHttp {
    fn calls(&self) -> u32 {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl HttpProber for CountingHttp {
    fn get_health(&self, _url: &str, _timeout: Duration) -> HealthState {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        HealthState::Ok
    }
}

/// A socket prober at a fixed path with a fixed verdict.
struct FixedSocket {
    socket: PathBuf,
    state: HealthState,
}

impl SearchSocketProber for FixedSocket {
    fn socket(&self) -> anyhow::Result<PathBuf> {
        Ok(self.socket.clone())
    }

    fn search_health(&self, _socket: &Path, _timeout: Duration) -> HealthState {
        self.state.clone()
    }
}

/// The default manifest with every prober mocked except `socket_prober`.
fn discoverer(http: &CountingHttp, socket_prober: Box<dyn SearchSocketProber>) -> Discoverer {
    Discoverer::with_probers(
        ServicesManifest::default_manifest(),
        Box::new(NoProcess),
        Box::new(NoPortFile),
        Box::new(http.clone()),
        Box::new(NoVersion),
    )
    .with_socket_prober(socket_prober)
}

/// Why: the embedded manifest is what a fresh install probes with; its
/// trusty-search entry must be the socket variant with no TCP port, and every
/// other service must keep the HTTP default.
/// Test: this test.
#[test]
fn default_manifest_declares_trusty_search_by_socket() {
    let m = ServicesManifest::default_manifest();
    let ts = &m.services[SEARCH];
    assert_eq!(ts.health_probe, HealthProbe::UdsSearch);
    assert_eq!(
        ts.default_port, None,
        "no TCP port for a socket-only daemon"
    );
    assert_eq!(ts.health_url, None, "no HTTP health URL");
    for (name, decl) in m.services.iter().filter(|(n, _)| *n != SEARCH) {
        assert_eq!(decl.health_probe, HealthProbe::Http, "{name} keeps HTTP");
    }
}

/// Why: the UP arm — a socket-only daemon answering `search.health` is
/// healthy and running, with no port, no PID and no HTTP.
/// What: a mock daemon on a temp socket that `TRUSTY_SEARCH_SOCKET` names,
/// probed through the real socket prober.
/// Test: this test.
#[test]
#[serial]
fn uds_search_is_up_against_a_socket_only_daemon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("ts.sock");
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let _daemon = crate::uds_mock::spawn_blocking_at(socket.clone(), move |method, _params| {
        log.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(method.to_string());
        Box::pin(async { Ok(serde_json::json!({ "status": "ok" })) })
    });
    let _env = EnvVarGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &socket);
    let http = CountingHttp::default();
    let mut d = discoverer(&http, Box::new(RealSearchSocketProber));

    let health = d.health(SEARCH).expect("trusty-search is declared");
    assert_eq!(health.state, HealthState::Ok, "{}", health.message);

    let status = d.status(SEARCH).expect("trusty-search is declared");
    assert!(status.running, "an answering socket is a running daemon");
    assert_eq!(status.pid, None, "UP needs no PID: pgrep found nothing");
    let json = serde_json::to_value(&status).expect("serialise");
    assert!(json["port"].is_null(), "port must be null, got {json}");
    assert!(json["url"].is_null(), "url must be null, got {json}");
    assert_eq!(json["health"], "ok");

    assert!(
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|m| m == METHOD_HEALTH),
        "the probe must call search.health on the socket"
    );
    assert_eq!(
        http.calls(),
        0,
        "a socket service is never probed over HTTP"
    );
}

/// Why: the DOWN arm — a missing socket, a stale socket file with no
/// listener, and a listener that never answers are each `Fail` with an error
/// naming the socket, returned within the probe timeout.
/// Test: this test.
#[test]
#[serial]
fn uds_search_is_down_on_a_missing_stale_or_hung_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.sock");
    let stale = dir.path().join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).expect("bind stale"));
    assert!(
        stale.exists(),
        "the stale socket file must outlive its listener"
    );
    let hung = dir.path().join("hung.sock");
    // Bound and never accepted: the dial succeeds, the answer never comes.
    let _hung_listener = std::os::unix::net::UnixListener::bind(&hung).expect("bind hung");

    for socket in [missing, stale, hung] {
        let _env = EnvVarGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &socket);
        let http = CountingHttp::default();
        let mut d = discoverer(&http, Box::new(RealSearchSocketProber));

        let started = Instant::now();
        let health = d.health(SEARCH).expect("trusty-search is declared");
        let took = started.elapsed();
        match &health.state {
            HealthState::Fail { detail } => assert!(
                detail.contains(&socket.display().to_string()),
                "the error must name {}: {detail}",
                socket.display()
            ),
            other => panic!("{} must be DOWN, got {other:?}", socket.display()),
        }
        assert!(
            took < HEALTH_PROBE_TIMEOUT + Duration::from_secs(1),
            "{} took {took:?}, past the probe timeout",
            socket.display()
        );
        let status = d.status(SEARCH).expect("trusty-search is declared");
        assert!(!status.running, "no answer and no process is not running");
        assert_eq!(status.port, None);
        assert_eq!(
            http.calls(),
            0,
            "a socket service is never probed over HTTP"
        );
    }
}

/// Why: no listing, status, health, port or URL query may reach the HTTP
/// prober for a `uds_search` service, healthy or not.
/// Test: this test.
#[test]
fn uds_search_never_calls_the_http_prober() {
    for state in [
        HealthState::Ok,
        HealthState::Fail {
            detail: "dead".into(),
        },
    ] {
        let http = CountingHttp::default();
        let mut d = Discoverer::with_probers(
            ServicesManifest {
                version: 1,
                services: ServicesManifest::default_manifest()
                    .services
                    .into_iter()
                    .filter(|(n, _)| n == SEARCH)
                    .collect(),
            },
            Box::new(NoProcess),
            Box::new(NoPortFile),
            Box::new(http.clone()),
            Box::new(NoVersion),
        )
        .with_socket_prober(Box::new(FixedSocket {
            socket: PathBuf::from("/tmp/ts-9543.sock"),
            state: state.clone(),
        }));
        let listed = d.list();
        assert_eq!(listed[0].health, state);
        let _ = d.health(SEARCH);
        let _ = d.port(SEARCH);
        let _ = d.url(SEARCH);
        assert_eq!(http.calls(), 0, "HTTP prober called for {state:?}");
    }
}

/// Why: `tm services port|url trusty-search` must fail naming the socket, not
/// print a TCP port nothing binds; other services keep theirs.
/// Test: this test.
#[test]
fn port_and_url_for_trusty_search_name_the_socket() {
    let http = CountingHttp::default();
    let mut d = discoverer(
        &http,
        Box::new(FixedSocket {
            socket: PathBuf::from("/tmp/ts-9543.sock"),
            state: HealthState::Ok,
        }),
    );
    let port = d.port(SEARCH).expect("declared");
    let err = port.expect_err("trusty-search has no TCP port");
    assert!(
        err.contains("has no TCP port") && err.contains("/tmp/ts-9543.sock"),
        "{err}"
    );
    let url = d.url(SEARCH).expect("declared");
    let err = url.expect_err("trusty-search has no TCP URL");
    assert!(
        err.contains("has no TCP URL") && err.contains("/tmp/ts-9543.sock"),
        "{err}"
    );
    assert_eq!(d.port("trusty-analyze"), Some(Ok(7879)));
    assert!(d.port("no-such-service").is_none());
}

/// Why (#5965): `tm`'s `main` is `#[tokio::main]`, so the real socket prober
/// runs inside a runtime and must not panic on a nested one.
/// Test: this test.
#[tokio::test]
async fn real_socket_prober_survives_being_called_inside_a_tokio_runtime() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = RealSearchSocketProber
        .search_health(&dir.path().join("none.sock"), Duration::from_millis(250));
    assert!(matches!(state, HealthState::Fail { .. }), "got {state:?}");
}

/// The trusty-search entry an older `tm services init` wrote, beside another
/// static HTTP service that must be left alone.
const LEGACY_YAML: &str = r#"version: 1
services:
  trusty-search:
    description: "Hybrid BM25 + vector + KG code search daemon"
    default_port: 7878
    port_discovery: static
    health_url: "http://localhost:{port}/health"
    process_match: "trusty-search"
    start_cmd: "trusty-search start"
  trusty-analyze:
    description: "Code analysis sidecar daemon"
    default_port: 7879
    port_discovery: static
    health_url: "http://localhost:{port}/health"
"#;

/// Why: the old 7878 entry is mapped on read to the socket variant with one
/// WARN line naming the file, and the user's file is never written.
/// Test: this test.
#[test]
fn legacy_search_entry_maps_to_socket_and_leaves_the_file_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("services.yaml");
    std::fs::write(&path, LEGACY_YAML).expect("write manifest");
    let before = std::fs::read(&path).expect("read manifest");

    let mut warn = Vec::new();
    let m = load_user_manifest(&path, &mut warn).expect("load");

    let ts = &m.services[SEARCH];
    assert_eq!(ts.health_probe, HealthProbe::UdsSearch);
    assert_eq!(ts.default_port, None);
    assert_eq!(ts.health_url, None);
    assert_eq!(ts.start_cmd.as_deref(), Some("trusty-search start"));
    let analyze = &m.services["trusty-analyze"];
    assert_eq!(analyze.health_probe, HealthProbe::Http);
    assert_eq!(analyze.default_port, Some(7879));

    let warn = String::from_utf8(warn).expect("utf8");
    assert_eq!(warn.lines().count(), 1, "exactly one WARN line: {warn:?}");
    assert!(warn.starts_with("WARN"), "{warn}");
    assert!(warn.contains(&path.display().to_string()), "{warn}");
    assert_eq!(
        std::fs::read(&path).expect("reread"),
        before,
        "the file is never written"
    );
}

/// Why: only the exact legacy shape is mapped; a trusty-search entry on
/// another port is the user's own choice and stays as written, silently.
/// Test: this test.
#[test]
fn other_trusty_search_shapes_are_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("services.yaml");
    std::fs::write(&path, LEGACY_YAML.replace("7878", "7900")).expect("write manifest");

    let mut warn = Vec::new();
    let m = load_user_manifest(&path, &mut warn).expect("load");

    let ts = &m.services[SEARCH];
    assert_eq!(ts.health_probe, HealthProbe::Http);
    assert_eq!(ts.default_port, Some(7900));
    assert!(warn.is_empty(), "no WARN for a non-legacy entry");
}
