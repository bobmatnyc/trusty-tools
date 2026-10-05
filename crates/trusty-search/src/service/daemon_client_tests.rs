//! Tests for `service::daemon_client` (#6285).
//!
//! Why: the client is the seam every CLI subcommand (and, next, the MCP bridge)
//! crosses to reach the daemon, so its three failure kinds and the code each
//! refusal carries are pinned here once rather than per subcommand.
//! What: a mock daemon on a scratch socket under a `TempDir` answers every
//! method through a closure. No test resolves or dials the live daemon's
//! socket.
//! Test: this file.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use trusty_common::uds::server::{
    serve_until, RpcError, RpcFallback, RpcRouter, RpcServeOptions, CODE_INTERNAL_ERROR,
    CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND,
};

use super::*;

/// The closure a mock daemon answers every call with.
type Handler = Arc<dyn Fn(&str, Value) -> Result<Value, RpcError> + Send + Sync>;

/// Routes every method to the test's closure.
struct Fallback(Handler);

#[async_trait]
impl RpcFallback for Fallback {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        (self.0)(method, params)
    }
}

/// A mock daemon on a scratch socket. Dropping it stops the accept loop.
pub(crate) struct MockDaemon {
    pub(crate) client: DaemonClient,
    _dir: tempfile::TempDir,
    _stop: tokio::sync::oneshot::Sender<()>,
}

/// Bind a scratch socket and answer every call through `handler`.
pub(crate) async fn mock_daemon(
    handler: impl Fn(&str, Value) -> Result<Value, RpcError> + Send + Sync + 'static,
) -> MockDaemon {
    mock_daemon_with(Fallback(Arc::new(handler))).await
}

/// Bind a scratch socket and answer every call through `fallback`.
///
/// #9168: the async form, so a bridge test can answer after a delay.
pub(crate) async fn mock_daemon_with(fallback: impl RpcFallback) -> MockDaemon {
    let dir = tempfile::tempdir().expect("scratch socket dir");
    let socket = dir.path().join("ts.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the scratch socket");
    let router = Arc::new(RpcRouter::new().fallback(fallback));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = stopped.await;
        })
        .await;
    });
    MockDaemon {
        client: DaemonClient::at(socket),
        _dir: dir,
        _stop: stop,
    }
}

/// A call the daemon answers returns its `result`, and carries its params.
#[tokio::test]
async fn a_call_returns_the_daemons_result() {
    let daemon =
        mock_daemon(|method, params| Ok(json!({ "method": method, "params": params }))).await;
    let got = daemon
        .client
        .call("search.index.status", json!({ "index_id": "x" }))
        .await
        .expect("the mock answers");
    assert_eq!(got["method"], "search.index.status");
    assert_eq!(got["params"]["index_id"], "x");
}

/// Every refusal class the daemon projects from an HTTP status arrives with its
/// own code, its own message, and the predicate a caller branches on.
///
/// Why a table: this is the error-mapping contract the CLI (and next the MCP
/// bridge) relies on. A predicate that matched the wrong code would turn a
/// "no such index" into a retry loop, or an unavailability into a hard miss.
#[tokio::test]
async fn a_refusal_carries_the_daemons_code_and_message() {
    type Pred = fn(&DaemonCallError) -> bool;
    let cases: &[(i64, &str, Pred)] = &[
        (CODE_NOT_FOUND, "not found", DaemonCallError::is_not_found),
        (CODE_CONFLICT, "conflict", DaemonCallError::is_conflict),
        (
            CODE_INVALID_PARAMS,
            "invalid params",
            DaemonCallError::is_invalid_params,
        ),
        (
            CODE_UNAVAILABLE,
            "unavailable",
            DaemonCallError::is_unavailable,
        ),
        (
            CODE_UNAVAILABLE_PERMANENT,
            "permanently unavailable",
            DaemonCallError::is_permanently_unavailable,
        ),
    ];
    for &(code, label, predicate) in cases {
        let daemon =
            mock_daemon(move |_, _| Err(RpcError::new(code, "index_not_resident: wt-1"))).await;
        let err = daemon
            .client
            .call("search.query", json!({}))
            .await
            .expect_err("the mock refuses");
        assert_eq!(err.code(), Some(code));
        assert_eq!(err.message(), Some("index_not_resident: wt-1"));
        assert!(predicate(&err), "code {code} must satisfy its predicate");
        assert!(
            !err.is_unreachable(),
            "a refusal is an answer, not an outage"
        );
        let text = err.to_string();
        assert!(
            text.contains(label) && text.contains("index_not_resident"),
            "the operator line must name the class and the daemon's reason: {text}"
        );
    }
    // The two 503 classes are both unavailable; only one is permanent.
    let retryable = DaemonCallError::Refused {
        method: "m".into(),
        code: CODE_UNAVAILABLE,
        message: String::new(),
        data: None,
    };
    assert!(retryable.is_unavailable() && !retryable.is_permanently_unavailable());
    // A not-found refusal is never read as an unavailability, or the reverse.
    let missing = DaemonCallError::Refused {
        method: "m".into(),
        code: CODE_NOT_FOUND,
        message: String::new(),
        data: None,
    };
    assert!(!missing.is_unavailable() && !retryable.is_not_found());
}

/// A refusal's `data` member reaches the caller verbatim, and a refusal that
/// sent none reports none.
///
/// Why: #6285 moves the MCP bridge onto this client, and its INDEX_UNAVAILABLE
/// contract relays the daemon's 503 body field for field. A client that kept
/// only the code and message would strand `restore_via`, `stages` and the rest.
#[tokio::test]
async fn a_refusal_carries_the_daemons_error_data() {
    let body = json!({
        "error": "index_not_resident",
        "index_id": "wt-1",
        "retryable": true,
        "restore_via": "POST /indexes/wt-1/search",
        "reason": "cold_parked",
        "transient": true,
        "stages": { "lexical": "ready" },
    });
    let sent = body.clone();
    let daemon = mock_daemon(move |_, _| {
        Err(RpcError::new(CODE_UNAVAILABLE, "index_not_resident").with_data(sent.clone()))
    })
    .await;
    let err = daemon
        .client
        .call("search.index.status", json!({}))
        .await
        .expect_err("the mock refuses");
    assert_eq!(err.data(), Some(&body));

    let bare = mock_daemon(|_, _| Err(RpcError::new(CODE_NOT_FOUND, "unknown index: x"))).await;
    let err = bare
        .client
        .call("search.index.status", json!({}))
        .await
        .expect_err("the mock refuses");
    assert_eq!(err.data(), None, "a refusal without data must report none");
}

/// A socket nothing serves fails closed, names the path, and says how to fix it.
///
/// Why: requirement 5 of #6285's consumer move — there is no TCP fallback, so
/// this error is the whole of what an operator sees when the daemon is down.
#[tokio::test]
async fn a_missing_socket_fails_closed_naming_the_path() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let socket = dir.path().join("nobody-home.sock");
    let client = DaemonClient::at(&socket);

    let err = client
        .call("search.indexes.list", json!({}))
        .await
        .expect_err("nothing is listening");
    assert!(
        err.is_unreachable(),
        "a dead socket is unreachable: {err:?}"
    );
    assert_eq!(err.code(), None);
    let text = err.to_string();
    assert!(
        text.contains(&socket.display().to_string()),
        "the error must name the socket path: {text}"
    );
    assert!(text.contains("trusty-search start"), "{text}");
    assert!(
        !text.contains("http://"),
        "no TCP address may appear: {text}"
    );
    assert!(!client.is_up().await);
}

/// Every code the daemon emits reads as its own class, never the catch-all.
#[test]
fn every_daemon_code_has_a_label() {
    for code in [
        CODE_INVALID_PARAMS,
        CODE_FORBIDDEN,
        CODE_NOT_FOUND,
        CODE_DEADLINE_EXCEEDED,
        CODE_CONFLICT,
        CODE_TOO_MANY_REQUESTS,
        CODE_UNAVAILABLE,
        CODE_UNAVAILABLE_PERMANENT,
        CODE_METHOD_NOT_FOUND,
        CODE_INTERNAL_ERROR,
    ] {
        assert_ne!(code_label(code), "error", "code {code} has no label");
    }
    assert_eq!(code_label(-1), "error");
}

/// `TRUSTY_SEARCH_SOCKET` names the socket outright; blank means unset.
#[test]
fn resolve_honours_the_socket_env_override() {
    let explicit = resolve_socket(Some(std::ffi::OsStr::new(" /tmp/ts-6285/x.sock ")))
        .expect("an explicit path resolves");
    assert_eq!(explicit, PathBuf::from("/tmp/ts-6285/x.sock"));

    // A blank override falls through to the daemon's own derivation, which
    // must end in the product socket name either way.
    let derived = resolve_socket(Some(std::ffi::OsStr::new("  "))).expect("derivation");
    assert_eq!(
        derived.file_name().and_then(|n| n.to_str()),
        Some("trusty-search.sock")
    );
}

/// A streaming call yields each item, then ends; the frame asks for a stream.
#[tokio::test]
async fn a_stream_yields_items_then_ends() {
    use trusty_common::uds::server::RpcStreamItems;

    let dir = tempfile::tempdir().expect("scratch socket dir");
    let socket = dir.path().join("ts.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let router = Arc::new(RpcRouter::new().typed_stream::<Value, _, _>(
        "search.index.reindex.stream",
        |params| async move {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(json!({ "type": "start", "p": params }))).await;
                let _ = tx.send(Ok(json!({ "type": "complete" }))).await;
            });
            Ok::<RpcStreamItems, RpcError>(rx)
        },
    ));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = stopped.await;
        })
        .await;
    });

    let mut frames = DaemonClient::at(&socket)
        .stream("search.index.reindex.stream", json!({ "index_id": "x" }))
        .await
        .expect("the stream opens");
    let first = frames.next_frame().await.expect("one").expect("ok");
    assert_eq!(first["type"], "start");
    assert_eq!(first["p"]["index_id"], "x");
    let second = frames.next_frame().await.expect("two").expect("ok");
    assert_eq!(second["type"], "complete");
    assert!(
        frames.next_frame().await.is_none(),
        "the stream ends cleanly"
    );
    drop(stop);
}
