use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::services::manifest::{PortDiscovery, ServiceDecl, ServicesManifest};

// ── Mock implementations ──

struct MockProcessProber {
    pid: Option<u32>,
    /// Count how many times pgrep was called.
    call_count: Arc<Mutex<u32>>,
}

impl MockProcessProber {
    fn new(pid: Option<u32>) -> Self {
        Self {
            pid,
            call_count: Arc::new(Mutex::new(0)),
        }
    }
    fn call_count_arc(&self) -> Arc<Mutex<u32>> {
        Arc::clone(&self.call_count)
    }
}

impl ProcessProber for MockProcessProber {
    fn pgrep(&self, _pattern: &str) -> Option<u32> {
        *self.call_count.lock().unwrap() += 1;
        self.pid
    }
}

struct MockPortProber {
    port: Option<u16>,
}

impl PortProber for MockPortProber {
    fn read_port_file(&self, _path: &std::path::Path) -> Option<u16> {
        self.port
    }
}

struct MockHttpProber {
    state: HealthState,
    call_count: Arc<Mutex<u32>>,
}

impl MockHttpProber {
    fn new(state: HealthState) -> Self {
        Self {
            state,
            call_count: Arc::new(Mutex::new(0)),
        }
    }
    fn call_count_arc(&self) -> Arc<Mutex<u32>> {
        Arc::clone(&self.call_count)
    }
}

impl HttpProber for MockHttpProber {
    fn get_health(&self, _url: &str, _timeout: Duration) -> HealthState {
        *self.call_count.lock().unwrap() += 1;
        self.state.clone()
    }
}

struct MockVersionRunner {
    version: Option<String>,
}

impl VersionRunner for MockVersionRunner {
    fn run(&self, _cmd: &str) -> Option<String> {
        self.version.clone()
    }
}

// ── Helper manifest builders ──

fn static_manifest_with_port(port: u16) -> ServicesManifest {
    let mut services = BTreeMap::new();
    services.insert(
        "test-svc".to_string(),
        ServiceDecl {
            description: "test service".to_string(),
            default_port: Some(port),
            port_discovery: PortDiscovery::Static,
            port_file: None,
            health_url: Some("http://localhost:{port}/health".to_string()),
            log_path: None,
            version_cmd: Some("echo 1.0.0".to_string()),
            process_match: Some("test-svc".to_string()),
            start_cmd: None,
            stop_cmd: None,
            restart_cmd: None,
        },
    );
    ServicesManifest {
        version: 1,
        services,
    }
}

fn file_manifest(port_file: &str) -> ServicesManifest {
    let mut services = BTreeMap::new();
    services.insert(
        "dyn-svc".to_string(),
        ServiceDecl {
            description: "dynamic port service".to_string(),
            default_port: Some(7070),
            port_discovery: PortDiscovery::File,
            port_file: Some(port_file.to_string()),
            health_url: Some("http://localhost:{port}/health".to_string()),
            log_path: None,
            version_cmd: None,
            process_match: Some("dyn-svc".to_string()),
            start_cmd: None,
            stop_cmd: None,
            restart_cmd: None,
        },
    );
    ServicesManifest {
        version: 1,
        services,
    }
}

fn sidecar_manifest() -> ServicesManifest {
    let mut services = BTreeMap::new();
    services.insert(
        "sidecar".to_string(),
        ServiceDecl {
            description: "UDS sidecar".to_string(),
            default_port: None,
            port_discovery: PortDiscovery::Static,
            port_file: None,
            health_url: None,
            log_path: None,
            version_cmd: None,
            process_match: Some("sidecar-proc".to_string()),
            start_cmd: None,
            stop_cmd: None,
            restart_cmd: None,
        },
    );
    ServicesManifest {
        version: 1,
        services,
    }
}

// ── Tests ──

#[test]
fn status_returns_none_for_unknown_service() {
    let m = static_manifest_with_port(7878);
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(None)),
        Box::new(MockPortProber { port: None }),
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner { version: None }),
    );
    assert!(d.status("no-such-service").is_none());
}

#[test]
fn status_uses_cache_within_ttl() {
    let m = static_manifest_with_port(7878);
    let prober = MockProcessProber::new(Some(1234));
    let count = prober.call_count_arc();
    let mut d = Discoverer::with_probers(
        m,
        Box::new(prober),
        Box::new(MockPortProber { port: Some(7878) }),
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner {
            version: Some("1.0".into()),
        }),
    );
    d.status("test-svc");
    d.status("test-svc");
    // pgrep must be called only once (second call uses cache).
    assert_eq!(*count.lock().unwrap(), 1);
}

#[test]
fn status_probes_fresh_after_ttl() {
    let m = static_manifest_with_port(7878);
    let prober = MockProcessProber::new(Some(1234));
    let count = prober.call_count_arc();
    let mut d = Discoverer::with_probers(
        m,
        Box::new(prober),
        Box::new(MockPortProber { port: Some(7878) }),
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner { version: None }),
    );
    // Manually expire the cache by inserting with an old timestamp.
    d.status("test-svc"); // inserts into cache
    // Force cache expiry.
    let old = Instant::now() - Duration::from_secs(10);
    if let Some(entry) = d.cache.get_mut("test-svc") {
        entry.0 = old;
    }
    d.status("test-svc"); // must re-probe
    assert_eq!(*count.lock().unwrap(), 2);
}

#[test]
fn probe_process_returns_pid_when_running() {
    let m = static_manifest_with_port(9000);
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(5678))),
        Box::new(MockPortProber { port: Some(9000) }),
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner {
            version: Some("2.0".into()),
        }),
    );
    let status = d.status("test-svc").unwrap();
    assert_eq!(status.pid, Some(5678));
    assert!(status.running);
}

#[test]
fn probe_process_returns_none_when_not_running() {
    let m = static_manifest_with_port(9000);
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(None)),
        Box::new(MockPortProber { port: None }),
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner { version: None }),
    );
    let status = d.status("test-svc").unwrap();
    assert_eq!(status.pid, None);
    assert!(!status.running);
}

#[test]
fn probe_port_reads_port_file() {
    let m = file_manifest("/tmp/test-port-file");
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(999))),
        Box::new(MockPortProber { port: Some(7073) }),
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner { version: None }),
    );
    let status = d.status("dyn-svc").unwrap();
    assert_eq!(status.port, Some(7073));
}

#[test]
fn probe_port_returns_default_port() {
    let m = static_manifest_with_port(7878);
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(100))),
        Box::new(MockPortProber { port: None }), // not consulted for Static
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner { version: None }),
    );
    let status = d.status("test-svc").unwrap();
    assert_eq!(status.port, Some(7878));
}

#[test]
fn probe_health_ok_on_2xx() {
    let m = static_manifest_with_port(7878);
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(1))),
        Box::new(MockPortProber { port: Some(7878) }),
        Box::new(MockHttpProber::new(HealthState::Ok)),
        Box::new(MockVersionRunner { version: None }),
    );
    let status = d.status("test-svc").unwrap();
    assert_eq!(status.health, HealthState::Ok);
}

#[test]
fn probe_health_fail_on_503() {
    let m = static_manifest_with_port(7878);
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(1))),
        Box::new(MockPortProber { port: Some(7878) }),
        Box::new(MockHttpProber::new(HealthState::Fail {
            detail: "HTTP 503 Service Unavailable".into(),
        })),
        Box::new(MockVersionRunner { version: None }),
    );
    let status = d.status("test-svc").unwrap();
    assert!(matches!(status.health, HealthState::Fail { .. }));
}

#[test]
fn probe_health_fail_on_connection_refused() {
    let m = static_manifest_with_port(7878);
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(1))),
        Box::new(MockPortProber { port: Some(7878) }),
        Box::new(MockHttpProber::new(HealthState::Fail {
            detail: "connection refused".into(),
        })),
        Box::new(MockVersionRunner { version: None }),
    );
    let status = d.status("test-svc").unwrap();
    assert!(
        matches!(status.health, HealthState::Fail { ref detail } if detail.contains("refused"))
    );
}

#[test]
fn health_bypasses_cache() {
    let m = static_manifest_with_port(7878);
    let http_prober = MockHttpProber::new(HealthState::Ok);
    let count = http_prober.call_count_arc();
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(1))),
        Box::new(MockPortProber { port: Some(7878) }),
        Box::new(http_prober),
        Box::new(MockVersionRunner { version: None }),
    );
    d.status("test-svc"); // primes cache (1 http call)
    d.health("test-svc"); // must re-probe (1 more http call)
    // status() triggered 1 probe, health() triggered 1 more = 2 total.
    assert_eq!(*count.lock().unwrap(), 2);
}

#[test]
fn list_returns_all_manifest_services() {
    let m = ServicesManifest::default_manifest();
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(None)),
        Box::new(MockPortProber { port: None }),
        Box::new(MockHttpProber::new(HealthState::Fail {
            detail: "not running".into(),
        })),
        Box::new(MockVersionRunner { version: None }),
    );
    let list = d.list();
    assert_eq!(list.len(), 6);
    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"trusty-search"));
    assert!(names.contains(&"trusty-memory"));
    assert!(names.contains(&"trusty-embedderd"));
}

#[test]
fn service_status_serialises_to_json() {
    let status = ServiceStatus {
        name: "trusty-search".to_string(),
        declared: true,
        running: true,
        pid: Some(12345),
        port: Some(7878),
        url: Some("http://localhost:7878".to_string()),
        version: Some("trusty-search 0.13.2".to_string()),
        health: HealthState::Ok,
        log_path: None,
        uptime_secs: Some(3600),
    };
    let json = serde_json::to_string(&status).expect("serialise");
    let rt: serde_json::Value = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(rt["name"], "trusty-search");
    assert_eq!(rt["pid"], 12345);
    assert_eq!(rt["port"], 7878);
    assert_eq!(rt["health"], "ok");
    assert_eq!(rt["uptime_secs"], 3600);
}

#[test]
fn sidecar_shows_unknown_health_when_running() {
    let m = sidecar_manifest();
    let mut d = Discoverer::with_probers(
        m,
        Box::new(MockProcessProber::new(Some(42))),
        Box::new(MockPortProber { port: None }),
        Box::new(MockHttpProber::new(HealthState::Ok)), // should not be called
        Box::new(MockVersionRunner { version: None }),
    );
    let status = d.status("sidecar").unwrap();
    assert!(status.running);
    assert_eq!(status.health, HealthState::Unknown);
    assert!(status.port.is_none());
}
