//! #6288 PR 1b: `tm services` finds the trusty-mpm daemon by socket health,
//! never by its retiring HTTP port 7880.
//!
//! Why: every client reaches the daemon over its socket since PR 1a, and PR 2
//! removes the 7880 bind. A default manifest that GETs `:7880/health` reports a
//! live daemon DOWN from then on, and a user manifest an older `tm services
//! init` wrote still names 7880.
//! What: the default manifest's shape, the UP arm against a mock daemon
//! answering `mpm.health`, the DOWN arm against a missing socket and an error
//! answer, the HTTP prober's zero call count, and the legacy user-manifest
//! mapping.
//! Test: these tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trusty_common::uds::server::RpcError;

use crate::services::discoverer::{
    HttpProber, METHOD_MPM_HEALTH, MpmSocketProber, PortProber, ProcessProber, RealMpmSocketProber,
    VersionRunner,
};
use crate::services::{Discoverer, HealthProbe, HealthState, ServicesManifest, load_user_manifest};

const MPM: &str = "trusty-mpm-daemon";

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

/// The real prober's dial, aimed at a socket this test owns rather than the
/// one `TRUSTY_MPM_SOCKET` or the data directory would name.
struct RealAt(PathBuf);

impl MpmSocketProber for RealAt {
    fn socket(&self) -> PathBuf {
        self.0.clone()
    }

    fn mpm_health(&self, socket: &Path, timeout: Duration) -> HealthState {
        RealMpmSocketProber.mpm_health(socket, timeout)
    }
}

/// The default manifest with every prober mocked except the mpm one.
fn discoverer(http: &CountingHttp, socket: PathBuf) -> Discoverer {
    Discoverer::with_probers(
        ServicesManifest::default_manifest(),
        Box::new(NoProcess),
        Box::new(NoPortFile),
        Box::new(http.clone()),
        Box::new(NoVersion),
    )
    .with_mpm_prober(Box::new(RealAt(socket)))
}

/// Why: the embedded manifest is what a fresh install probes with; the mpm
/// daemon entry must be the socket variant with no TCP port and no URL.
/// Test: this test.
#[test]
fn default_manifest_declares_the_mpm_daemon_by_socket() {
    let m = ServicesManifest::default_manifest();
    let mpm = &m.services[MPM];
    assert_eq!(mpm.health_probe, HealthProbe::UdsMpm);
    assert_eq!(mpm.default_port, None, "no TCP port: 7880 is retiring");
    assert_eq!(mpm.health_url, None, "no HTTP health URL");
}

/// Why: the UP arm — a socket-only daemon answering `mpm.health` is healthy
/// and running, with no port, no URL and no HTTP call.
/// Test: this test.
#[test]
fn uds_mpm_is_up_against_a_socket_only_daemon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("mpm.sock");
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let _daemon = crate::uds_mock::spawn_blocking_at(socket.clone(), move |method, _params| {
        log.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(method.to_string());
        Box::pin(async { Ok(serde_json::json!({ "status": "ok", "version": "test" })) })
    });
    let http = CountingHttp::default();
    let mut d = discoverer(&http, socket.clone());

    let health = d.health(MPM).expect("the mpm daemon is declared");
    assert_eq!(health.state, HealthState::Ok, "{}", health.message);
    let status = d.status(MPM).expect("the mpm daemon is declared");
    assert!(status.running, "an answering socket is a running daemon");
    assert_eq!(status.port, None);
    assert_eq!(status.url, None);
    assert!(
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|m| m == METHOD_MPM_HEALTH),
        "the probe must call mpm.health on the socket"
    );
    assert_eq!(http.calls(), 0, "the mpm daemon is never probed over HTTP");
    let err = d.port(MPM).expect("declared").expect_err("no TCP port");
    assert!(err.contains(&socket.display().to_string()), "{err}");
}

/// Why: the DOWN arm and the Fail-Open Check — a missing socket and a daemon
/// that answers with an error frame are each `Fail`, never `Ok`, and neither
/// falls back to HTTP.
/// Test: this test.
#[test]
fn uds_mpm_is_down_on_a_missing_socket_or_an_error_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.sock");
    let erroring = dir.path().join("erroring.sock");
    let _daemon = crate::uds_mock::spawn_blocking_at(erroring.clone(), |_method, _params| {
        Box::pin(async { Err(RpcError::internal("state lock poisoned")) })
    });

    for socket in [missing, erroring] {
        let http = CountingHttp::default();
        let mut d = discoverer(&http, socket.clone());
        let health = d.health(MPM).expect("the mpm daemon is declared");
        match &health.state {
            HealthState::Fail { detail } => assert!(
                detail.contains(&socket.display().to_string()),
                "the error must name {}: {detail}",
                socket.display()
            ),
            other => panic!("{} must be DOWN, got {other:?}", socket.display()),
        }
        let status = d.status(MPM).expect("the mpm daemon is declared");
        assert!(!status.running, "a failed probe is not a running daemon");
        assert_eq!(http.calls(), 0, "no HTTP fallback for {}", socket.display());
    }
}

/// Why: a user manifest an older `tm services init` wrote probes 7880 over
/// HTTP; it is mapped on read to the socket probe, with one WARN naming the
/// file, and the file is never written.
/// Test: this test.
#[test]
fn legacy_mpm_daemon_entry_maps_to_its_socket() {
    const LEGACY: &str = r#"version: 1
services:
  trusty-mpm-daemon:
    description: "MPM background orchestrator"
    default_port: 7880
    port_discovery: static
    health_url: "http://localhost:{port}/health"
    process_match: "trusty-mpm"
    start_cmd: "tm daemon"
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("services.yaml");
    std::fs::write(&path, LEGACY).expect("write manifest");

    let mut warn = Vec::new();
    let m = load_user_manifest(&path, &mut warn).expect("load");

    let mpm = &m.services[MPM];
    assert_eq!(mpm.health_probe, HealthProbe::UdsMpm);
    assert_eq!(mpm.default_port, None);
    assert_eq!(mpm.health_url, None);
    let warn = String::from_utf8(warn).expect("utf8");
    assert_eq!(warn.lines().count(), 1, "exactly one WARN line: {warn:?}");
    assert!(warn.contains("uds_mpm") && warn.contains("7880"), "{warn}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread"),
        LEGACY,
        "the file is never written"
    );
}
