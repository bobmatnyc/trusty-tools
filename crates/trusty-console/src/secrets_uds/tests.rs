//! Tests for the secrets bridge (#9066, plan T1-T21).
//!
//! Why: every refusal and every downgrade in this module is a place a secret
//! could leak or an agent could pass as a person, so each is driven through
//! the real router against a stub socket that records what reached it.
//! What: a stub trusty-secrets socket, a fake process table that counts its
//! reads, and one test per behaviour.
//! Test: this file IS the test.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_secrets::ancestry::{ProcessError, ProcessTable, StartTime};
use trusty_secrets::api::BridgeAssertion;

use super::dial::{self, SecretsDial};
use super::errors::{Refusal, UPSTREAM_KINDS};
use super::verdict::{AcceptedPeer, PeerLookup, PidResolver, VERDICT_BOUND, judge};
use super::{CSRF_HEADER, MAX_BODY_BYTES, SecretsBridge, TailnetRequest, router};

pub(super) const OWN: &str = "127.0.0.1:7788";
const SPECIMEN: &str = "SPECIMEN-VALUE-1234567890";

// ─── fakes ───────────────────────────────────────────────────────────────────

/// A process table the test writes; counts every read.
#[derive(Default)]
pub(super) struct FakeTable {
    /// pid -> (parent, start, is_agent)
    procs: HashMap<u32, (u32, u64, bool)>,
    /// pids whose parent read fails although their start time reads.
    broken_chain: HashSet<u32>,
    reads: AtomicUsize,
}

impl FakeTable {
    fn with(mut self, pid: u32, parent: u32, start: u64, agent: bool) -> Self {
        self.procs.insert(pid, (parent, start, agent));
        self
    }

    fn entry(&self, pid: u32) -> Result<(u32, u64, bool), ProcessError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.procs
            .get(&pid)
            .copied()
            .ok_or(ProcessError::Unreadable { pid })
    }
}

impl ProcessTable for FakeTable {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        if self.broken_chain.contains(&pid) {
            return Err(ProcessError::Unreadable { pid });
        }
        self.entry(pid).map(|e| e.0)
    }
    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError> {
        self.entry(pid).map(|e| StartTime::from_raw(e.1))
    }
    fn is_agent(&self, pid: u32) -> Result<bool, ProcessError> {
        self.entry(pid).map(|e| e.2)
    }
}

/// A shell (500) under Claude (400), and a shell (600) under launchd (1).
pub(super) fn family() -> FakeTable {
    FakeTable::default()
        .with(400, 1, 40, true)
        .with(500, 400, 50, false)
        .with(600, 1, 60, false)
}

/// Every framed request a stub socket received.
pub(super) type Seen = Arc<Mutex<Vec<Value>>>;

/// Bind a stub secrets socket that answers each request with `respond`.
pub(super) fn stub<F>(dir: &Path, respond: F) -> (PathBuf, Seen)
where
    F: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
{
    let socket = dir.join("secrets.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let seen: Seen = Arc::default();
    let (record, respond) = (Arc::clone(&seen), Arc::new(respond));
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let (record, respond) = (Arc::clone(&record), Arc::clone(&respond));
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut raw = Vec::new();
                let _ = conn.read_to_end(&mut raw).await;
                let request: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                record.lock().expect("lock").push(request.clone());
                if let Some(frame) = respond(&request) {
                    let _ = conn.write_all(format!("{frame}\n").as_bytes()).await;
                }
            });
        }
    });
    (socket, seen)
}

pub(super) fn ok_frame(result: Value) -> Option<Value> {
    Some(json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
}

pub(super) fn error_frame(kind: Option<&str>) -> Option<Value> {
    let data = kind.map_or(Value::Null, |k| json!({ "kind": k }));
    Some(json!({ "jsonrpc": "2.0", "id": 1, "error": {
        "code": -32000, "message": "UPSTREAM-MESSAGE-TEXT", "data": data } }))
}

pub(super) fn bridge_at(socket: &Path, table: Arc<dyn ProcessTable>) -> Arc<SecretsBridge> {
    let bridge = SecretsBridge::new(SecretsDial::new(socket.to_path_buf()).with_external(true))
        .expect("mint")
        .with_table(table);
    bridge.set_authorities(&[OWN.parse().expect("addr")]);
    Arc::new(bridge)
}

pub(super) fn token(bridge: &SecretsBridge) -> String {
    bridge.token().expect("enabled").to_hex()
}

/// A request from the console's own page: Host, Origin and the token.
pub(super) fn own_request(bridge: &SecretsBridge, path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("host", OWN)
        .header("origin", format!("http://{OWN}"))
        .header(CSRF_HEADER, token(bridge))
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("request")
}

pub(super) async fn send(
    bridge: &Arc<SecretsBridge>,
    request: Request<Body>,
) -> (StatusCode, String) {
    let response = router(Arc::clone(bridge))
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn text(refusal: Refusal) -> &'static str {
    refusal.status_and_text().1
}

pub(super) const LIST_BODY: &str = r#"{"project":"/p","vault":"trusty/acme/web"}"#;

/// A tempdir and a stub that answers every request with `{"ok":true}`.
pub(super) fn ok_stub() -> (tempfile::TempDir, PathBuf, Seen) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = stub(tmp.path(), |_| ok_frame(json!({ "ok": true })));
    (tmp, socket, seen)
}

// ─── T1, A4, A5: one call per request, each with an assertion ────────────────

#[tokio::test]
async fn each_route_dials_its_bridged_method_with_an_assertion() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let cases = [
        ("/api/secrets/list", LIST_BODY, "secrets.bridge.list"),
        (
            "/api/secrets/set",
            r#"{"project":"/p","vault":"trusty/acme/web","key":"API_KEY","value":"v"}"#,
            "secrets.bridge.set",
        ),
        (
            "/api/secrets/delete",
            r#"{"project":"/p","vault":"trusty/acme/web","key":"API_KEY"}"#,
            "secrets.bridge.delete",
        ),
        (
            "/api/secrets/copy",
            r#"{"project":"/p","from_backend":"keychain","to_backend":"file"}"#,
            "secrets.bridge.copy",
        ),
        (
            "/api/secrets/agents_may_use",
            r#"{"project":"/p","vault":"trusty/acme/web","key":"API_KEY","allowed":true}"#,
            "secrets.bridge.set_agents_may_use",
        ),
    ];
    for (path, body, _) in cases {
        let mut request = own_request(&bridge, path, body);
        request
            .extensions_mut()
            .insert(ConnectInfo(PeerLookup::ready(AcceptedPeer::Unfound)));
        let (status, answer) = send(&bridge, request).await;
        assert_eq!(status, StatusCode::OK, "{path}: {answer}");
        assert_eq!(answer, r#"{"ok":true}"#, "{path}");
    }
    let seen = seen.lock().expect("lock");
    let methods: Vec<&str> = seen.iter().filter_map(|r| r["method"].as_str()).collect();
    let expected: Vec<&str> = cases.iter().map(|c| c.2).collect();
    assert_eq!(methods, expected, "exactly one call per request, in order");
    for request in seen.iter() {
        let bridge = &request["params"]["bridge"];
        // #9067 Architect Q5: an unmappable client is asserted an agent.
        assert_eq!(
            bridge,
            &json!({ "agent_parent": true, "source": "unmappable" })
        );
        assert!(!request.to_string().contains("pid"), "no pid on the wire");
    }
}

// ─── T2: resolve and unknown paths never reach the socket ────────────────────

#[tokio::test]
async fn unknown_secrets_paths_never_reach_the_socket() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let cases = [
        (
            "/api/secrets/resolve",
            StatusCode::NOT_IMPLEMENTED,
            Refusal::ResolveUnsupported,
        ),
        (
            "/api/secrets/x",
            StatusCode::NOT_FOUND,
            Refusal::UnknownRoute,
        ),
        (
            "/api/secrets/list/extra",
            StatusCode::NOT_FOUND,
            Refusal::UnknownRoute,
        ),
        ("/api/secrets", StatusCode::NOT_FOUND, Refusal::UnknownRoute),
        // #9067: the picker routes match exactly; a longer path is unknown.
        (
            "/api/secrets/scopes/extra",
            StatusCode::NOT_FOUND,
            Refusal::UnknownRoute,
        ),
        (
            "/api/secrets/projects/x",
            StatusCode::NOT_FOUND,
            Refusal::UnknownRoute,
        ),
        (
            "/api/secrets/machine_scopes",
            StatusCode::NOT_FOUND,
            Refusal::UnknownRoute,
        ),
    ];
    for (path, status, refusal) in cases {
        let (got, body) = send(&bridge, own_request(&bridge, path, LIST_BODY)).await;
        assert_eq!(got, status, "{path}");
        assert!(body.contains(text(refusal)), "{path}: {body}");
    }
    assert!(seen.lock().expect("lock").is_empty());
}

// ─── T3: no CORS header on the secrets router ────────────────────────────────

#[tokio::test]
async fn secrets_responses_carry_no_cors_header() {
    let (_tmp, socket, _seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let mut refused = own_request(&bridge, "/api/secrets/list", LIST_BODY);
    refused.headers_mut().remove("origin");
    for request in [
        own_request(&bridge, "/api/secrets/list", LIST_BODY),
        refused,
    ] {
        let response = router(Arc::clone(&bridge))
            .oneshot(request)
            .await
            .expect("response");
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-origin")
        );
    }
}

// ─── T4, T5: Origin and Host ─────────────────────────────────────────────────

#[tokio::test]
async fn origin_absent_or_foreign_is_refused_on_every_method() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    for method in ["GET", "POST", "OPTIONS"] {
        for origin in [
            None,
            Some("http://127.0.0.1:9999"),
            Some("http://localhost:7788"),
            Some("null"),
            Some("https://127.0.0.1:7788"),
        ] {
            let mut request = own_request(&bridge, "/api/secrets/list", LIST_BODY);
            *request.method_mut() = method.parse().expect("method");
            request.headers_mut().remove("origin");
            if let Some(origin) = origin {
                request
                    .headers_mut()
                    .insert("origin", origin.parse().expect("header"));
            }
            let (status, body) = send(&bridge, request).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {origin:?}");
            assert!(body.contains(text(Refusal::Origin)), "{method} {origin:?}");
        }
    }
    // Past the Origin check: a preflight is 403 and a GET is 405 (Q3).
    for (method, status) in [
        ("OPTIONS", StatusCode::FORBIDDEN),
        ("GET", StatusCode::METHOD_NOT_ALLOWED),
    ] {
        let mut request = own_request(&bridge, "/api/secrets/session", "");
        *request.method_mut() = method.parse().expect("method");
        assert_eq!(send(&bridge, request).await.0, status, "{method}");
    }
    assert!(seen.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn host_not_this_listener_is_refused() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    for host in ["evil.test:7788", "localhost:7788", "127.0.0.1:9999"] {
        let mut request = own_request(&bridge, "/api/secrets/list", LIST_BODY);
        let headers = request.headers_mut();
        headers.insert("host", host.parse().expect("header"));
        headers.insert("origin", format!("http://{host}").parse().expect("header"));
        assert_eq!(
            send(&bridge, request).await.0,
            StatusCode::FORBIDDEN,
            "{host}"
        );
    }
    assert!(seen.lock().expect("lock").is_empty());
}

// ─── T6, T7, A12: the per-launch token ───────────────────────────────────────

#[tokio::test]
async fn csrf_must_match_this_launch() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let earlier = bridge_at(&socket, Arc::new(family()));
    let good = token(&bridge);
    let mut wrong = good.clone().into_bytes();
    wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
    let wrong = String::from_utf8(wrong).expect("hex");
    for sent in [
        None,
        Some(wrong),
        Some(format!("{good}0")),
        Some(good[..32].to_owned()),
        Some(token(&earlier)),
    ] {
        let mut request = own_request(&bridge, "/api/secrets/list", LIST_BODY);
        request.headers_mut().remove(CSRF_HEADER);
        if let Some(sent) = &sent {
            request
                .headers_mut()
                .insert(CSRF_HEADER, sent.parse().expect("header"));
        }
        let (status, body) = send(&bridge, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains(text(Refusal::Csrf)));
    }
    assert!(seen.lock().expect("lock").is_empty());
    assert_eq!(good.len(), 64, "256 bits as hex");
    assert!(
        !format!("{:?}", bridge.token()).contains(&good),
        "Debug redacts"
    );
}

#[tokio::test]
async fn session_returns_the_token_to_the_own_origin_only() {
    let (_tmp, socket, _seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let mut request = own_request(&bridge, "/api/secrets/session", "");
    request.headers_mut().remove(CSRF_HEADER);
    let (status, body) = send(&bridge, request).await;
    assert_eq!(status, StatusCode::OK);
    let answer: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(answer["csrf"], token(&bridge));

    let mut foreign = own_request(&bridge, "/api/secrets/session", "");
    foreign
        .headers_mut()
        .insert("origin", "http://evil.test".parse().expect("header"));
    let (status, body) = send(&bridge, foreign).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!body.contains(&token(&bridge)));
}

// ─── T8, T10, A7: no echo, body cap ──────────────────────────────────────────

/// Every formatted tracing line this test binary emits once [`capture_logs`] ran.
static LOGS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct LogSink;

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOGS.lock()
            .map_err(|_| std::io::Error::other("log capture poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Install a global TRACE subscriber writing into [`LOGS`]. A scoped
/// subscriber misses callsites another test thread registered first.
fn capture_logs() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(|| LogSink)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

#[tokio::test]
async fn malformed_bodies_never_echo_the_value() {
    capture_logs();
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let bodies = [
        format!(r#"{{"project":"/p","vault":"trusty/acme/web","key":"K","value":"{SPECIMEN}""#),
        format!(
            r#"{{"project":"/p","vault":"trusty/acme/web","key":"K","value":"{SPECIMEN}","extra":1}}"#
        ),
        format!(r#"{{"project":"/p","vault":"{SPECIMEN}","key":"K","value":"v"}}"#),
        format!(r#"{{"project":"/p","vault":"trusty/acme/web","key":"K","value":["{SPECIMEN}"]}}"#),
    ];
    for body in &bodies {
        let (status, answer) = send(&bridge, own_request(&bridge, "/api/secrets/set", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert!(!answer.contains(SPECIMEN));
        assert_eq!(
            answer,
            json!({ "error": text(Refusal::BadBody) }).to_string()
        );
    }
    let mut refused = own_request(&bridge, "/api/secrets/set", &bodies[1]);
    refused.headers_mut().remove("origin");
    assert_eq!(send(&bridge, refused).await.0, StatusCode::FORBIDDEN);
    assert!(
        seen.lock().expect("lock").is_empty(),
        "nothing reached the socket"
    );
    let logs = String::from_utf8_lossy(&LOGS.lock().expect("lock")).into_owned();
    assert!(
        logs.contains("secrets bridge refused"),
        "capture works: {logs}"
    );
    assert!(!logs.contains(SPECIMEN), "{logs}");
}

#[tokio::test]
async fn oversized_body_is_refused_before_the_socket() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let big = format!(
        r#"{{"project":"/p","vault":"trusty/acme/web","key":"K","value":"{}"}}"#,
        "x".repeat(MAX_BODY_BYTES)
    );
    let declared = {
        let mut r = own_request(&bridge, "/api/secrets/set", &big);
        let len = big.len().to_string();
        r.headers_mut()
            .insert("content-length", len.parse().expect("header"));
        r
    };
    for request in [declared, own_request(&bridge, "/api/secrets/set", &big)] {
        let (status, body) = send(&bridge, request).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(body.contains(text(Refusal::TooLarge)));
    }
    assert!(seen.lock().expect("lock").is_empty());
}

// ─── T9, T17: the closed upstream table ──────────────────────────────────────

/// A stub whose error kind is whatever the request's `project` names.
fn kind_echo_stub(dir: &Path) -> (PathBuf, Seen) {
    stub(dir, |request| {
        let kind = request["params"]["project"].as_str().unwrap_or_default();
        error_frame((kind != "/none").then_some(kind))
    })
}

fn list_for(project: &str) -> String {
    json!({ "project": project, "vault": "trusty/acme/web" }).to_string()
}

#[tokio::test]
async fn upstream_errors_relay_only_the_kind() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, _seen) = kind_echo_stub(tmp.path());
    let bridge = bridge_at(&socket, Arc::new(family()));
    for (kind, status, fixed) in UPSTREAM_KINDS {
        let request = own_request(&bridge, "/api/secrets/list", &list_for(kind));
        let (got, body) = send(&bridge, request).await;
        assert_eq!(got, status, "{kind}");
        assert_eq!(body, json!({ "error": fixed, "kind": kind }).to_string());
    }
    for project in ["brand_new_kind", "/none"] {
        let request = own_request(&bridge, "/api/secrets/list", &list_for(project));
        let (got, body) = send(&bridge, request).await;
        assert_eq!(got, StatusCode::BAD_GATEWAY, "{project}");
        assert_eq!(
            body,
            json!({ "error": text(Refusal::UnknownUpstream) }).to_string()
        );
    }
}

#[tokio::test]
async fn audit_unavailable_is_relayed_without_retry() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = kind_echo_stub(tmp.path());
    let bridge = bridge_at(&socket, Arc::new(family()));
    let request = own_request(&bridge, "/api/secrets/list", &list_for("audit_unavailable"));
    let (status, body) = send(&bridge, request).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains(r#""kind":"audit_unavailable""#));
    assert_eq!(
        seen.lock().expect("lock").len(),
        1,
        "no retry, no downgrade"
    );
}

// ─── T11-T14, T21: the verdict ───────────────────────────────────────────────

fn found(pid: u32, start: u64) -> AcceptedPeer {
    AcceptedPeer::Found {
        pid,
        start: Some(StartTime::from_raw(start)),
    }
}

/// Why: #9067 Architect Q5, a Fail-Open Check — a client the console cannot
/// map to a pid (every client off macOS and Linux) must not pass as a
/// person, so it is asserted an agent and toggle-ON is refused for it.
/// Red while `unmappable` carries `agent_parent: false`.
/// Test: itself.
#[test]
fn verdict_without_a_pid_is_unmappable_and_an_agent() {
    let verdict = judge(&family(), AcceptedPeer::Unfound);
    assert_eq!(
        serde_json::to_value(verdict).expect("json"),
        json!({ "agent_parent": true, "source": "unmappable" })
    );
    assert!(verdict.agent_parent);
    assert!(verdict.is_consistent());
}

#[test]
fn verdict_with_an_unreadable_chain_is_unreadable() {
    let mut table = family();
    table.broken_chain.insert(600);
    assert_eq!(judge(&table, found(600, 60)), BridgeAssertion::unreadable());
    let unknown_start = AcceptedPeer::Found {
        pid: 600,
        start: None,
    };
    assert_eq!(
        judge(&family(), unknown_start),
        BridgeAssertion::unreadable()
    );
}

#[test]
fn verdict_for_a_pid_reused_after_accept_is_unreadable() {
    assert_eq!(
        judge(&family(), found(600, 61)),
        BridgeAssertion::unreadable()
    );
    // Gone since accept: a later read error is still `unreadable`.
    assert_eq!(
        judge(&family(), found(700, 70)),
        BridgeAssertion::unreadable()
    );
}

#[test]
fn verdict_reads_the_agent_ancestor() {
    assert_eq!(
        judge(&family(), found(500, 50)),
        BridgeAssertion::mapped(true)
    );
    assert_eq!(
        judge(&family(), found(600, 60)),
        BridgeAssertion::mapped(false)
    );
    let init = FakeTable::default().with(1, 0, 1, false);
    assert_eq!(judge(&init, found(1, 1)), BridgeAssertion::mapped(false));
    let selfish = FakeTable::default().with(9, 9, 9, false);
    assert_eq!(judge(&selfish, found(9, 9)), BridgeAssertion::mapped(false));
}

struct FixedPid(Option<u32>);

impl PidResolver for FixedPid {
    fn pid_of(&self, _local: SocketAddr, _peer: SocketAddr) -> Option<u32> {
        self.0
    }
}

#[test]
fn accepted_peer_records_the_start_time_at_accept() {
    let (local, peer) = (
        OWN.parse().expect("addr"),
        "127.0.0.1:50000".parse().expect("addr"),
    );
    let table = family();
    assert_eq!(
        AcceptedPeer::resolve(&FixedPid(Some(500)), &table, local, peer),
        found(500, 50)
    );
    assert_eq!(
        AcceptedPeer::resolve(&FixedPid(Some(700)), &table, local, peer),
        AcceptedPeer::Found {
            pid: 700,
            start: None
        }
    );
    assert_eq!(
        AcceptedPeer::resolve(&FixedPid(None), &table, local, peer),
        AcceptedPeer::Unfound
    );
}

/// Send a list as the client `peer`; return the assertion the socket saw.
async fn wire_assertion(table: FakeTable, peer: AcceptedPeer) -> (StatusCode, Value) {
    wire_assertion_for(Arc::new(table), Some(PeerLookup::ready(peer))).await
}

/// Send a list on a connection whose connect info is `lookup`, if any.
async fn wire_assertion_for(
    table: Arc<dyn ProcessTable>,
    lookup: Option<PeerLookup>,
) -> (StatusCode, Value) {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, table);
    let mut request = own_request(&bridge, "/api/secrets/list", LIST_BODY);
    if let Some(lookup) = lookup {
        request.extensions_mut().insert(ConnectInfo(lookup));
    }
    let (status, _) = send(&bridge, request).await;
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "forwarded exactly once");
    (status, seen[0]["params"]["bridge"].clone())
}

#[tokio::test]
async fn verdicts_reach_the_wire_and_are_forwarded() {
    let unreadable = json!({ "agent_parent": true, "source": "unreadable" });
    let mut broken = family();
    broken.broken_chain.insert(600);
    let cases = [
        (
            family(),
            AcceptedPeer::Unfound,
            json!({ "agent_parent": true, "source": "unmappable" }),
        ),
        (broken, found(600, 60), unreadable.clone()),
        (family(), found(600, 61), unreadable),
        (
            family(),
            found(500, 50),
            json!({ "agent_parent": true, "source": "mapped" }),
        ),
        (
            family(),
            found(600, 60),
            json!({ "agent_parent": false, "source": "mapped" }),
        ),
    ];
    for (table, peer, expected) in cases {
        let (status, sent) = wire_assertion(table, peer).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{peer:?} is served; the server decides"
        );
        assert_eq!(sent, expected, "{peer:?}");
    }
}

#[tokio::test]
async fn unreadable_verdict_is_forwarded_identically_every_time() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    for _ in 0..2 {
        let mut request = own_request(&bridge, "/api/secrets/list", LIST_BODY);
        request
            .extensions_mut()
            .insert(ConnectInfo(PeerLookup::ready(found(600, 99))));
        assert_eq!(send(&bridge, request).await.0, StatusCode::OK);
    }
    let seen = seen.lock().expect("lock");
    assert_eq!(seen[0], seen[1], "the console reads no audit setting");
}

// ─── #9066 F1, F2: the wait for the accept-time lookup ───────────────────────

/// Finds the agent pid 500, but only after the verdict bound has passed.
struct Stalled;

impl PidResolver for Stalled {
    fn pid_of(&self, _local: SocketAddr, _peer: SocketAddr) -> Option<u32> {
        std::thread::sleep(VERDICT_BOUND + std::time::Duration::from_secs(1));
        Some(500)
    }
}

#[tokio::test]
async fn a_lookup_slower_than_the_bound_is_unreadable() {
    let table: Arc<dyn ProcessTable> = Arc::new(family());
    let lookup = PeerLookup::start(
        Stalled,
        Arc::clone(&table),
        OWN.parse().expect("addr"),
        "127.0.0.1:50000".parse().expect("addr"),
    );
    let started = std::time::Instant::now();
    let (status, sent) = wire_assertion_for(table, Some(lookup)).await;
    let took = started.elapsed();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sent,
        json!({ "agent_parent": true, "source": "unreadable" }),
        "a timed-out lookup never yields the late `mapped` answer"
    );
    assert!(
        took < VERDICT_BOUND + std::time::Duration::from_millis(500),
        "the request waited {took:?}, past the {VERDICT_BOUND:?} bound"
    );
}

#[tokio::test]
async fn a_request_without_a_lookup_is_unreadable() {
    let (status, sent) = wire_assertion_for(Arc::new(family()), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sent,
        json!({ "agent_parent": true, "source": "unreadable" }),
        "missing connect info is a wiring fault, never `unmappable`"
    );
}

#[tokio::test]
async fn a_getsockname_failure_at_accept_is_unreadable() {
    let peer: SocketAddr = "127.0.0.1:50000".parse().expect("addr");
    let local = Err(std::io::Error::other("getsockname failed"));
    let lookup = super::pid_map::lookup_at_accept(local, peer);
    let (status, sent) = wire_assertion_for(Arc::new(family()), Some(lookup)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sent,
        json!({ "agent_parent": true, "source": "unreadable" }),
        "an unreadable local address is a wiring fault, never `unmappable`"
    );
}

// ─── T15, T16, A11: reachability ─────────────────────────────────────────────

#[tokio::test]
async fn dial_in_external_mode_with_no_socket_is_unavailable() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let bridge = bridge_at(&tmp.path().join("absent.sock"), Arc::new(family()));
    let (status, body) = send(
        &bridge,
        own_request(&bridge, "/api/secrets/list", LIST_BODY),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({ "error": text(Refusal::Unavailable) }).to_string()
    );
}

#[tokio::test]
async fn dial_reports_a_missing_binary_as_unavailable() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dial = SecretsDial::new(tmp.path().join("s").join("secrets.sock"))
        .with_external(false)
        .with_program(tmp.path().join("no-such-trusty-secrets"));
    let bridge = SecretsBridge::new(dial).expect("mint");
    bridge.set_authorities(&[OWN.parse().expect("addr")]);
    let bridge = Arc::new(bridge);
    let (status, body) = send(
        &bridge,
        own_request(&bridge, "/api/secrets/list", LIST_BODY),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.contains("no-such"), "no cause: {body}");
}

#[tokio::test]
async fn dial_reports_a_hangup_as_transport() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = stub(tmp.path(), |_| None);
    let bridge = bridge_at(&socket, Arc::new(family()));
    let (status, body) = send(
        &bridge,
        own_request(&bridge, "/api/secrets/list", LIST_BODY),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body,
        json!({ "error": text(Refusal::Transport) }).to_string()
    );
    assert_eq!(seen.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn unknown_own_origin_refuses_every_route() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = SecretsBridge::new(SecretsDial::new(socket).with_external(true)).expect("mint");
    let bridge = Arc::new(bridge);
    for path in [
        "/api/secrets/session",
        "/api/secrets/list",
        "/api/secrets/resolve",
    ] {
        let (status, body) = send(&bridge, own_request(&bridge, path, LIST_BODY)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert!(body.contains(text(Refusal::OriginUnknown)));
    }
    let disabled = Arc::new(SecretsBridge::disabled());
    disabled.set_authorities(&[OWN.parse().expect("addr")]);
    let mut request = own_request(&bridge, "/api/secrets/list", LIST_BODY);
    request.headers_mut().remove(CSRF_HEADER);
    assert_eq!(send(&disabled, request).await.0, StatusCode::FORBIDDEN);
    assert!(seen.lock().expect("lock").is_empty());
}

// ─── T19, A2: the tailnet listener ───────────────────────────────────────────

#[tokio::test]
async fn tailnet_requests_are_refused_before_any_lookup() {
    let (_tmp, socket, seen) = ok_stub();
    let table = Arc::new(family());
    let bridge = bridge_at(&socket, Arc::clone(&table) as Arc<dyn ProcessTable>);
    for path in [
        "/api/secrets/session",
        "/api/secrets/list",
        "/api/secrets/set",
        "/api/secrets/delete",
        "/api/secrets/copy",
        "/api/secrets/agents_may_use",
        "/api/secrets/resolve",
        "/api/secrets/x",
    ] {
        let mut request = own_request(&bridge, path, LIST_BODY);
        request.extensions_mut().insert(TailnetRequest);
        request
            .extensions_mut()
            .insert(ConnectInfo(PeerLookup::ready(found(500, 50))));
        let (status, body) = send(&bridge, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert!(body.contains(text(Refusal::Tailnet)), "{path}");
    }
    assert!(seen.lock().expect("lock").is_empty());
    assert_eq!(table.reads.load(Ordering::SeqCst), 0, "no process lookup");
}

// ─── T20: drift guard against the server's own constants ─────────────────────

fn secrets_source(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../trusty-secrets/src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn console_kind_table_matches_the_server_error_kinds() {
    let errors = secrets_source("server/errors.rs");
    let start = errors.find("pub fn as_str(self)").expect("as_str");
    let end = start + errors[start..].find("pub fn text(self)").expect("text");
    let server: HashSet<&str> = errors[start..end]
        .lines()
        .filter_map(|line| line.split("=> \"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    let console: HashSet<&str> = UPSTREAM_KINDS.iter().map(|(k, _, _)| *k).collect();
    assert_eq!(server.len(), 37);
    assert_eq!(console, server);

    let deadline = secrets_source("server/deadline.rs");
    for (name, value) in [
        ("WRITE_DEADLINE", dial::WRITE_DEADLINE),
        ("READ_DEADLINE", dial::READ_DEADLINE),
        ("CLIENT_MARGIN", dial::CLIENT_MARGIN),
    ] {
        let line = format!(
            "{name}: Duration = Duration::from_secs({});",
            value.as_secs()
        );
        assert!(deadline.contains(&line), "{line}");
    }
    let settings = secrets_source("server/settings.rs");
    for line in [
        format!(
            "pub const SOCKET_SUBPATH: &str = \"{}\";",
            dial::SOCKET_SUBPATH
        ),
        format!(
            "pub const SERVE_SUBCOMMAND: &str = \"{}\";",
            dial::SERVE_SUBCOMMAND
        ),
    ] {
        assert!(settings.contains(&line), "{line}");
    }
    let client = secrets_source("server/client.rs");
    let line = format!(
        "pub const SECRETS_EXTERNAL_ENV: &str = \"{}\";",
        dial::SECRETS_EXTERNAL_ENV
    );
    assert!(client.contains(&line), "{line}");
}

#[test]
fn client_wait_matches_the_direct_method_deadline() {
    use trusty_secrets::api::bridge::method;
    let write = dial::WRITE_DEADLINE + dial::CLIENT_MARGIN;
    assert_eq!(
        dial::client_wait(method::LIST),
        dial::READ_DEADLINE + dial::CLIENT_MARGIN
    );
    for name in [
        method::SET,
        method::DELETE,
        method::COPY,
        method::SET_AGENTS_MAY_USE,
    ] {
        assert_eq!(dial::client_wait(name), write, "{name}");
    }
}
