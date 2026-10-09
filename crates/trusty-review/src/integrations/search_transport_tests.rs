//! Tests for the #9214 socket-or-HTTP transport.
//!
//! Why: phase B keeps both legs, so the precedence and the error mapping are
//! the whole risk — a client that silently stayed on HTTP, or a socket error
//! that stopped reading as a 404 or 503, would pass every older test.
//! What: a temp-dir `UnixListener` fake stands in for the daemon (no live
//! daemon is ever reached), and a raw `TcpListener` stub for the HTTP leg.
//! Tests that change the environment run under `#[serial_test::serial]`, the
//! crate's env lock, and point `TRUSTY_DATA_DIR_OVERRIDE` at a temp dir so the
//! default socket path is never the developer's real one.
//! Test: included as `#[cfg(test)] mod tests` from `search_transport.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trusty_common::search_rpc::TRUSTY_SEARCH_DATA_DIR_ENV;

use super::fixture::{EnvGuard, FakeSearchSocket, healthy};
use super::*;
use crate::config::ReviewConfig;
use crate::integrations::search_client::{HttpSearchClient, SearchClient};
#[cfg(feature = "report")]
use crate::report::index_registry::fetch_registered_indexes_via;
#[cfg(feature = "report")]
use crate::report::investigate::trace_client::{HttpTraceSource, TraceError, TraceSource};

const DATA_DIR_OVERRIDE: &str = "TRUSTY_DATA_DIR_OVERRIDE";

/// A temp dir short enough for a Unix socket path on macOS (104 bytes).
fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("b5")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp")
}

/// Isolate the default socket path and clear every other transport env var.
fn isolated(dir: &tempfile::TempDir) -> [EnvGuard; 4] {
    [
        EnvGuard::set(DATA_DIR_OVERRIDE, &dir.path().to_string_lossy()),
        EnvGuard::unset(TRUSTY_SEARCH_SOCKET_ENV),
        EnvGuard::unset(TRUSTY_SEARCH_URL_ENV),
        EnvGuard::unset(TRUSTY_SEARCH_DATA_DIR_ENV),
    ]
}

/// The default socket path under the current `TRUSTY_DATA_DIR_OVERRIDE`.
fn default_socket() -> PathBuf {
    search_rpc::search_socket().expect("resolve the default socket path")
}

/// The config as loaded with `TRUSTY_SEARCH_URL` unset: no explicit URL.
fn default_config() -> ReviewConfig {
    ReviewConfig::load(None)
}

/// A loopback listener on an ephemeral port that counts every TCP connection.
///
/// #9214: the pre-fix fallback reached the address in `<TRUSTY_DATA_DIR>/http_addr`
/// when the socket file was missing; pointing that file here makes any such
/// attempt countable. Each connection is accepted and dropped at once.
async fn tcp_tripwire() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the tripwire");
    let addr = listener.local_addr().expect("addr").to_string();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (addr, hits)
}

/// Let the tripwire's accept loop drain its backlog before it is read.
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

/// A one-route HTTP stub: every request gets `status` and `body`.
async fn http_stub(status: &'static str, body: String) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the HTTP stub");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0_u8; 8192];
            let _ = stream.read(&mut buf).await;
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (url, hits)
}

/// Answer only `search.health`.
fn health_only(method: &str, _params: &Value) -> super::fixture::Reply {
    if method == "search.health" {
        Ok(healthy())
    } else {
        Err((-32601, "method not found".to_string(), None))
    }
}

// ── Precedence ───────────────────────────────────────────────────────────────

/// Rule 3: no env, the default socket exists → the socket leg.
#[serial_test::serial]
#[tokio::test]
async fn socket_is_used_when_present() {
    let dir = short_tempdir();
    let _env = isolated(&dir);
    let fake = FakeSearchSocket::serve(&default_socket(), health_only);

    let client = HttpSearchClient::from_config(&default_config()).expect("client builds");
    client
        .health()
        .await
        .expect("the fake socket answers health");

    assert_eq!(fake.methods(), vec!["search.health".to_string()]);
}

/// #9214 regression: no socket file and no explicit URL → the socket leg,
/// failing as unavailable with the path it looked for. Pre-fix this resolved
/// `Http("http://localhost:7878")`; the transport is asserted before any call,
/// so the red run never dials that port.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn missing_socket_fails_closed_on_the_config_leg() {
    let dir = short_tempdir();
    let _env = isolated(&dir);
    let missing = default_socket();
    assert!(!missing.exists(), "the isolated data dir holds no socket");

    let client = HttpSearchClient::from_config(&default_config()).expect("client builds");
    assert_eq!(
        client.transport(),
        &SearchTransport::Socket(missing.clone()),
        "a missing socket must not fall back to HTTP"
    );
    let err = client
        .health()
        .await
        .expect_err("nothing serves the socket");
    let shown = missing.display().to_string();
    assert!(
        matches!(err, SearchClientError::Unavailable(ref m) if m.contains(&shown)),
        "unavailable, naming {shown}: {err}"
    );
}

/// #9214 regression: the report pass's resolver makes no TCP attempt when the
/// socket file is missing. Pre-fix it resolved the `DaemonAddrLayout` address,
/// which reads `<TRUSTY_DATA_DIR>/http_addr` — here a tripwire listener.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn missing_socket_fails_closed_without_tcp_on_the_advertised_leg() {
    let dir = short_tempdir();
    let _env = isolated(&dir);
    let _iso = EnvGuard::set(TRUSTY_SEARCH_DATA_DIR_ENV, &dir.path().to_string_lossy());
    let (addr, hits) = tcp_tripwire().await;
    std::fs::write(dir.path().join("http_addr"), &addr).expect("write http_addr");
    let missing = dir.path().join("trusty-search.sock");
    assert!(!missing.exists(), "the isolated data dir holds no socket");

    let transport = SearchTransport::resolve_advertised();
    let client = HttpSearchClient::with_transport(transport.clone()).expect("client builds");
    let err = client
        .health()
        .await
        .expect_err("nothing serves the socket");
    settle().await;

    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "a missing socket reached TCP {addr}; transport was {transport:?}"
    );
    assert_eq!(transport, SearchTransport::Socket(missing.clone()));
    let shown = missing.display().to_string();
    assert!(
        matches!(err, SearchClientError::Unavailable(ref m) if m.contains(&shown)),
        "unavailable, naming {shown}: {err}"
    );
}

/// #9214: a socket path that cannot be derived fails closed too — a relative
/// `TRUSTY_DATA_DIR`, which the daemon refuses, is unavailable with the
/// reason, and no TCP attempt is made.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn unresolvable_socket_path_fails_closed() {
    let dir = short_tempdir();
    let _env = isolated(&dir);
    let _iso = EnvGuard::set(TRUSTY_SEARCH_DATA_DIR_ENV, "relative-data-dir");

    for transport in [
        SearchTransport::resolve(&default_config()),
        SearchTransport::resolve_advertised(),
    ] {
        let Some(path) = transport.socket_path() else {
            panic!("an unresolvable socket must not fall back to HTTP: {transport:?}");
        };
        assert!(
            path.display()
                .to_string()
                .contains("must be an absolute path"),
            "the path names the reason: {}",
            path.display()
        );
        let client = HttpSearchClient::with_transport(transport.clone()).expect("client builds");
        let err = client.health().await.expect_err("nothing serves it");
        assert!(
            matches!(err, SearchClientError::Unavailable(ref m) if m.contains("must be an absolute path")),
            "unavailable, naming the reason: {err}"
        );
    }
}

/// #9214: a deliberately set `TRUSTY_SEARCH_URL` keeps the HTTP leg, on that
/// URL, for both resolvers.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn explicit_url_env_keeps_the_http_leg() {
    let dir = short_tempdir();
    let _env = isolated(&dir);
    let (url, hits) = http_stub("200 OK", healthy().to_string()).await;
    let _url = EnvGuard::set(TRUSTY_SEARCH_URL_ENV, &format!("{url}/"));

    assert_eq!(
        SearchTransport::resolve(&default_config()),
        SearchTransport::Http(url.clone())
    );
    assert_eq!(
        SearchTransport::resolve_advertised(),
        SearchTransport::Http(url.clone())
    );
    let client = HttpSearchClient::from_config(&default_config()).expect("client builds");
    client.health().await.expect("the HTTP stub answers health");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the call went over HTTP");
}

/// Rule 1 beats rule 2: `TRUSTY_SEARCH_SOCKET` wins over `TRUSTY_SEARCH_URL`.
#[serial_test::serial]
#[tokio::test]
async fn explicit_socket_env_beats_explicit_url() {
    let dir = short_tempdir();
    let _env = isolated(&dir);
    let socket = dir.path().join("pinned.sock");
    let fake = FakeSearchSocket::serve(&socket, health_only);
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &socket.to_string_lossy());
    let _url = EnvGuard::set(TRUSTY_SEARCH_URL_ENV, "http://127.0.0.1:1");

    let config = ReviewConfig::load(None);
    assert_eq!(config.search_url, "http://127.0.0.1:1");
    let client = HttpSearchClient::from_config(&config).expect("client builds");
    client.health().await.expect("the pinned socket answers");
    assert_eq!(fake.methods(), vec!["search.health".to_string()]);
}

/// A socket file with no daemon is a dial error, never a fallback to HTTP.
#[serial_test::serial]
#[tokio::test]
async fn dead_socket_file_does_not_fall_back_to_http() {
    let dir = short_tempdir();
    let _env = isolated(&dir);
    let dead = dir.path().join("dead.sock");
    // Hardened, so the dial fails on the dead daemon, not the mode check.
    drop(trusty_common::uds::bind_hardened(&dead).expect("bind, then drop"));
    let (url, hits) = http_stub("200 OK", healthy().to_string()).await;
    let pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &dead.to_string_lossy());

    let mut config = default_config();
    config.search_url = url;
    let client = HttpSearchClient::from_config(&config).expect("client builds");
    let err = client
        .health()
        .await
        .expect_err("a dead daemon cannot answer");
    assert!(
        matches!(err, SearchClientError::Unavailable(ref m) if m.contains("dead.sock")),
        "the error names the socket leg: {err}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no request reached HTTP");

    // Rule 3 with a stale default socket file: still the socket leg.
    drop(pin);
    let stale = default_socket();
    drop(std::os::unix::net::UnixListener::bind(&stale).expect("bind, then drop"));
    assert_eq!(
        SearchTransport::resolve(&default_config()),
        SearchTransport::Socket(stale)
    );
}

/// A unit test that set nothing never reaches the operator's real socket.
///
/// Why: a test that dials the live daemon passes or fails on whether it is up,
/// and loads it. Most tests load the default config and set no env at all.
/// What: with every transport env var and the data-dir override cleared, both
/// resolvers must land on a socket path that does not exist. Reaching the real
/// path fails here: it is `Socket(<real path>)`, not the hermetic one.
/// Test: this test.
#[serial_test::serial]
#[test]
fn unit_tests_never_resolve_the_real_default_socket() {
    let _env = [
        EnvGuard::unset(DATA_DIR_OVERRIDE),
        EnvGuard::unset(TRUSTY_SEARCH_SOCKET_ENV),
        EnvGuard::unset(TRUSTY_SEARCH_URL_ENV),
        EnvGuard::unset(TRUSTY_SEARCH_DATA_DIR_ENV),
    ];
    for resolved in [
        SearchTransport::resolve(&default_config()),
        SearchTransport::resolve_advertised(),
    ] {
        match resolved {
            SearchTransport::Socket(path) => {
                assert!(
                    !path.exists(),
                    "a test resolved a live socket: {}",
                    path.display()
                );
                assert_eq!(path, hermetic_socket());
            }
            other => panic!("expected the hermetic socket, got {other:?}"),
        }
    }
}

/// `TRUSTY_DATA_DIR` isolates the default socket, as the daemon's own rule does.
///
/// Why: the daemon of an instance started with `TRUSTY_DATA_DIR` binds
/// `<TRUSTY_DATA_DIR>/trusty-search.sock`, and rule 3 takes its path from
/// `search_rpc::search_socket()` alone (#9214), so an isolated run reaches
/// its own daemon only while that function honours the var.
/// What: a fake "shared" daemon at the default path with `TRUSTY_DATA_DIR`
/// unset (moved into a temp dir by `TRUSTY_DATA_DIR_OVERRIDE`), then
/// `TRUSTY_DATA_DIR` set and the isolated instance's own socket; both
/// resolvers must pick `<TRUSTY_DATA_DIR>/trusty-search.sock`. A
/// `search_socket()` that ignored the var would answer the shared path.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn trusty_data_dir_isolates_the_default_socket() {
    let shared_dir = short_tempdir();
    let isolated_dir = short_tempdir();
    let _env = isolated(&shared_dir);
    let shared = FakeSearchSocket::serve(&default_socket(), health_only);
    let _iso = EnvGuard::set(
        TRUSTY_SEARCH_DATA_DIR_ENV,
        &isolated_dir.path().to_string_lossy(),
    );
    let own = FakeSearchSocket::serve(&isolated_dir.path().join("trusty-search.sock"), health_only);
    assert!(
        shared.path.starts_with(shared_dir.path()),
        "the shared fake sits under TRUSTY_DATA_DIR_OVERRIDE: {}",
        shared.path.display()
    );

    for resolved in [
        SearchTransport::resolve(&default_config()),
        SearchTransport::resolve_advertised(),
    ] {
        assert_ne!(resolved, SearchTransport::Socket(shared.path.clone()));
        assert_eq!(resolved, SearchTransport::Socket(own.path.clone()));
    }
}

// ── Error mapping ────────────────────────────────────────────────────────────

/// An `index_not_resident` refusal body, as the daemon's 503 carries it.
fn refusal(retryable: bool) -> Value {
    json!({"error": "index_not_resident", "index_id": "cold", "retryable": retryable})
}

/// The socket and HTTP 503s for one refusal body, as `(status, body)` pairs.
async fn both_503s(code: i64, data: Value) -> [(u16, Value); 2] {
    let dir = short_tempdir();
    let sent = data.clone();
    let fake = FakeSearchSocket::serve(&dir.path().join("s.sock"), move |_, _| {
        Err((code, "index_not_resident".to_string(), Some(sent.clone())))
    });
    let socket = HttpSearchClient::with_transport(SearchTransport::Socket(fake.path.clone()))
        .expect("client builds");
    let (url, _hits) = http_stub("503 Service Unavailable", data.to_string()).await;
    let http = HttpSearchClient::new(url).expect("client builds");

    let mut out = Vec::new();
    for client in [&socket, &http] {
        match client.index_status("cold").await {
            Err(SearchClientError::Api { status, body }) => {
                out.push((status, serde_json::from_str(&body).expect("JSON body")));
            }
            other => panic!("expected Api 503, got {other:?}"),
        }
    }
    [out.remove(0), out.remove(0)]
}

/// `-32002` reads exactly as the HTTP 503 does, body included.
#[tokio::test]
async fn rpc_32002_maps_to_the_http_503_error() {
    let [socket, http] = both_503s(CODE_UNAVAILABLE, refusal(true)).await;
    assert_eq!(socket, (503, refusal(true)));
    assert_eq!(socket, http);
}

/// `-32012`, the permanent refusal, is a 503 too.
#[tokio::test]
async fn rpc_32012_maps_to_503_too() {
    let [socket, http] = both_503s(CODE_UNAVAILABLE_PERMANENT, refusal(false)).await;
    assert_eq!(socket, (503, refusal(false)));
    assert_eq!(socket, http);
}

/// `-32004` is the HTTP 404: `is_unknown_index` holds for status and search.
#[tokio::test]
async fn rpc_32004_maps_to_the_http_404_error() {
    let dir = short_tempdir();
    let fake = FakeSearchSocket::serve(&dir.path().join("s.sock"), |_, _| {
        Err((CODE_NOT_FOUND, "unknown index: ghost".to_string(), None))
    });
    let client = HttpSearchClient::with_transport(SearchTransport::Socket(fake.path.clone()))
        .expect("client builds");

    let status = client.index_status("ghost").await.expect_err("404");
    assert!(status.is_unknown_index(), "index_status: {status}");
    let search = client.search("ghost", "q", Some(3)).await.expect_err("404");
    assert!(search.is_unknown_index(), "search: {search}");
    assert_eq!(
        fake.calls()[1],
        (
            "search.query".to_string(),
            json!({"index_id": "ghost", "body": {"text": "q", "top_k": 3}})
        )
    );

    let (url, _hits) = http_stub(
        "404 Not Found",
        r#"{"error":"unknown index: ghost"}"#.into(),
    )
    .await;
    let http = HttpSearchClient::new(url).expect("client builds");
    let http_err = http.index_status("ghost").await.expect_err("404");
    assert!(http_err.is_unknown_index(), "the HTTP leg agrees");
}

// ── The report pass ──────────────────────────────────────────────────────────

/// The trace reads go over the socket with the HTTP routes' parameters.
#[cfg(feature = "report")]
#[tokio::test]
async fn trace_entry_node_and_usages_go_over_the_socket() {
    let dir = short_tempdir();
    let fake = FakeSearchSocket::serve(&dir.path().join("s.sock"), |method, params| {
        match (method, params["index_id"].as_str()) {
            ("search.health", _) => Ok(healthy()),
            ("search.call_chain", Some("gone")) => {
                Err((CODE_NOT_FOUND, "unknown index: gone".to_string(), None))
            }
            ("search.call_chain", _) => Ok(Value::String(
                "## `run` [ENTRY]  src/lib.rs:7\nSignature: pub fn run()\n".to_string(),
            )),
            ("search.query", _) => Ok(json!({"results": [
                {"path": "src/lib.rs", "start_line": 9, "compact_snippet": "run();"}
            ]})),
            _ => Err((-32601, "method not found".to_string(), None)),
        }
    });
    let source = HttpTraceSource::with_transport(SearchTransport::Socket(fake.path.clone()))
        .expect("source builds");

    assert!(source.reachable().await);
    let entry = source.entry_node("idx", "run").await.expect("entry node");
    assert_eq!((entry.file.as_str(), entry.line), ("src/lib.rs", 7));
    let usages = source
        .usages("idx", "run", "src/", 5, 100)
        .await
        .expect("usages");
    assert_eq!(usages.len(), 1);
    assert_eq!(
        source.entry_node("gone", "run").await,
        Err(TraceError::IndexAbsent("gone".to_string()))
    );

    let calls = fake.calls();
    assert_eq!(
        calls[1],
        (
            "search.call_chain".to_string(),
            json!({"index_id": "idx", "entry_point": "run", "direction": "outgoing",
                   "max_depth": 1, "include_source": false})
        )
    );
    assert_eq!(
        calls[2],
        (
            "search.query".to_string(),
            json!({"index_id": "idx",
                   "body": {"text": "run", "top_k": 5, "path_prefix": "src/"}})
        )
    );
}

/// A `call_chain` result that is not a string is an error, never a definite
/// "symbol absent" (#9214 critic).
#[cfg(feature = "report")]
#[tokio::test]
async fn a_non_string_call_chain_result_is_an_error() {
    let dir = short_tempdir();
    let fake = FakeSearchSocket::serve(&dir.path().join("s.sock"), |_, _| {
        Ok(json!({"report": "## `run` [ENTRY]  src/lib.rs:7"}))
    });
    let source = HttpTraceSource::with_transport(SearchTransport::Socket(fake.path.clone()))
        .expect("source builds");
    assert_eq!(
        source.entry_node("idx", "run").await,
        Err(TraceError::Api {
            status: 200,
            body: "non-string call_chain result".to_string(),
        })
    );
}

/// The registry read stays fail-open on the socket leg, and reads when it can.
#[cfg(feature = "report")]
#[tokio::test]
async fn fetch_registered_indexes_over_the_socket_is_fail_open() {
    let dir = short_tempdir();
    let missing = SearchTransport::Socket(dir.path().join("missing.sock"));
    assert!(fetch_registered_indexes_via(missing).await.is_empty());

    let fake = FakeSearchSocket::serve(&dir.path().join("s.sock"), |_, _| {
        Ok(json!({"indexes": [{"id": "a", "root_path": "/w/a"}]}))
    });
    let indexes = fetch_registered_indexes_via(SearchTransport::Socket(fake.path.clone())).await;
    assert_eq!(indexes.len(), 1);
    assert_eq!(
        fake.calls()[0],
        ("search.indexes.list".to_string(), json!({"details": true}))
    );
}

/// #9431: the HTTP leg's name, which every gate message and the transport
/// log line print, hides the userinfo password and the token value.
#[test]
fn describe_masks_url_credentials() {
    let url = "http://user:fake123fake@127.0.0.1:9/p?access_token=fake123fake";
    let shown = SearchTransport::Http(url.to_string()).describe();
    assert!(!shown.contains("fake123fake"), "secret survived: {shown}");
    assert!(shown.contains("127.0.0.1:9/p"), "host lost: {shown}");
    // A username-only userinfo is the credential itself.
    let shown = SearchTransport::Http("http://tok123@127.0.0.1:9/p".to_string()).describe();
    assert_eq!(shown, "http://[redacted]@127.0.0.1:9/p");
}

// ── The call-chain read (#9196) ──────────────────────────────────────────────

/// #9196: `call_chain` takes the socket leg with the HTTP route's parameters,
/// returns the bare-string report, maps a not-ready `-32002` onto the HTTP
/// 503 and refuses a result that is not a string.
#[tokio::test]
async fn call_chain_goes_over_the_socket_with_its_params() {
    let dir = short_tempdir();
    let fake =
        FakeSearchSocket::serve(
            &dir.path().join("s.sock"),
            |_, params| match params["entry_point"].as_str() {
                Some("src/a.rs::cold") => Err((
                    CODE_UNAVAILABLE,
                    "index_not_resident".to_string(),
                    Some(json!({"error": "index_not_resident", "retryable": true})),
                )),
                Some("src/a.rs::odd") => Ok(json!({"report": "x"})),
                _ => Ok(Value::String("## `f` [ENTRY]  src/a.rs:3\n".to_string())),
            },
        );
    let client = HttpSearchClient::with_transport(SearchTransport::Socket(fake.path.clone()))
        .expect("client builds");

    let text = client
        .call_chain("idx", "src/a.rs::f", "both", 1, false)
        .await
        .expect("report");
    assert_eq!(text, "## `f` [ENTRY]  src/a.rs:3\n");
    assert_eq!(
        fake.calls()[0],
        (
            "search.call_chain".to_string(),
            json!({"index_id": "idx", "entry_point": "src/a.rs::f", "direction": "both",
                   "max_depth": 1, "include_source": false})
        )
    );
    let cold = client
        .call_chain("idx", "src/a.rs::cold", "both", 1, false)
        .await
        .expect_err("not ready");
    assert!(
        matches!(cold, SearchClientError::Api { status: 503, .. }),
        "{cold:?}"
    );
    let odd = client
        .call_chain("idx", "src/a.rs::odd", "both", 1, false)
        .await
        .expect_err("non-string");
    assert!(matches!(odd, SearchClientError::Parse(_)), "{odd:?}");
}

/// A one-request HTTP stub that answers `status` with a `text/plain` `body`
/// and hands back the request it read.
async fn text_stub(
    status: &'static str,
    body: &'static str,
) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the HTTP stub");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = vec![0_u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (url, rx)
}

/// #9196, #9214 phase B: the HTTP leg reads `GET /indexes/{id}/call_chain`
/// with the same parameters and returns the `text/plain` body; a 404 keeps
/// its body so "entry point not found" and "unknown index" stay apart.
#[tokio::test]
async fn call_chain_over_http_returns_the_text_body() {
    let (url, request) = text_stub("200 OK", "## `f` [ENTRY]  src/a.rs:3\n").await;
    let client = HttpSearchClient::new(url).expect("client builds");
    let text = client
        .call_chain("idx", "src/a.rs::f", "both", 1, false)
        .await
        .expect("report");
    assert_eq!(text, "## `f` [ENTRY]  src/a.rs:3\n");
    let line = request.await.expect("request seen");
    assert!(
        line.starts_with(
            "GET /indexes/idx/call_chain?entry_point=src%2Fa.rs%3A%3Af&direction=both\
             &max_depth=1&include_source=false "
        ),
        "{line}"
    );

    let (url, _request) =
        text_stub("404 Not Found", r#"{"error":"entry point not found: g"}"#).await;
    let client = HttpSearchClient::new(url).expect("client builds");
    match client.call_chain("idx", "g", "both", 1, false).await {
        Err(SearchClientError::Api { status: 404, body }) => {
            assert!(body.contains("entry point not found"), "{body}");
        }
        other => panic!("expected Api 404, got {other:?}"),
    }
}
