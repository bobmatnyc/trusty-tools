//! The protocol handshake's client half, against fake daemons (#9288).
//!
//! Why: the real daemon only ever answers its own version, so the refusal arms
//! — a newer or older daemon, a broken answer, a daemon that predates the
//! handshake — need a socket that answers what the test chooses.
//! What: each test serves an [`RpcRouter`] on a temp socket and drives the
//! shared client against it.
//! Test: this file.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::*;
use crate::uds::server::{RpcError, RpcRouter, RpcServeOptions, serve_until};

const BUDGET: Duration = Duration::from_secs(10);

/// A router served on a temp socket; dropping it stops the accept loop.
struct Fake {
    socket: PathBuf,
    _dir: tempfile::TempDir,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn serve(router: RpcRouter) -> Fake {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("fake-memory.sock");
    let listener = crate::uds::bind_hardened(&socket).expect("bind the fake daemon");
    let (stop, shutdown) = oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(
            &listener,
            Arc::new(router),
            RpcServeOptions::default(),
            async {
                let _ = shutdown.await;
            },
        )
        .await;
    });
    Fake {
        socket,
        _dir: dir,
        stop: Some(stop),
    }
}

/// A router whose `memory.protocol` answers `body`.
fn answering(body: Value) -> RpcRouter {
    RpcRouter::new().typed::<Value, Value, _, _>(METHOD_PROTOCOL, move |_| {
        let body = body.clone();
        async move { Ok(body) }
    })
}

/// A router whose `memory.protocol` refuses with `error`.
fn refusing(error: RpcError) -> RpcRouter {
    RpcRouter::new().typed::<Value, Value, _, _>(METHOD_PROTOCOL, move |_| {
        let error = error.clone();
        async move { Err(error) }
    })
}

/// Why (#9288): a daemon in range must be callable, and the info it reported
/// must reach the caller unchanged.
/// Test: itself.
#[tokio::test]
async fn protocol_check_accepts_a_daemon_in_the_supported_range() {
    let version = *SUPPORTED_MEMORY_PROTOCOLS.end();
    let fake = serve(answering(
        json!({ "protocol_version": version, "daemon_version": "9.9.9" }),
    ));

    let verdict = check_memory_protocol_at(&fake.socket, BUDGET)
        .await
        .expect("an in-range daemon is callable");

    assert_eq!(
        verdict,
        MemoryProtocol::Supported(MemoryProtocolInfo {
            protocol_version: version,
            daemon_version: Some("9.9.9".to_string()),
        })
    );
}

/// Why (#9288): an old client meeting a newer daemon — or a new client meeting
/// an older one — must get the named error, not a parse failure later.
/// Test: itself.
#[tokio::test]
async fn protocol_check_refuses_an_out_of_range_daemon_with_a_named_error() {
    let below = SUPPORTED_MEMORY_PROTOCOLS.start().saturating_sub(1);
    let above = SUPPORTED_MEMORY_PROTOCOLS.end() + 1;
    for reported in [below, above] {
        let fake = serve(answering(
            json!({ "protocol_version": reported, "daemon_version": "2.0.0" }),
        ));

        let err = check_memory_protocol_at(&fake.socket, BUDGET)
            .await
            .expect_err("an out-of-range daemon is refused");

        match &err {
            MemoryProtocolError::Unsupported {
                daemon,
                daemon_version,
                min,
                max,
                ..
            } => {
                assert_eq!(*daemon, reported);
                assert_eq!(daemon_version, "2.0.0");
                assert_eq!(*min, *SUPPORTED_MEMORY_PROTOCOLS.start());
                assert_eq!(*max, *SUPPORTED_MEMORY_PROTOCOLS.end());
            }
            other => panic!("protocol {reported}: expected Unsupported, got {other:?}"),
        }
        assert!(
            err.to_string()
                .starts_with("unsupported trusty-memory protocol"),
            "the refusal names itself: {err}"
        );
    }
}

/// Why (#9288): a daemon that predates the handshake answers method-not-found.
/// That one answer reads as `PreHandshake` — callable during a rolling upgrade,
/// and distinct from `Supported` so no caller mistakes it for a checked daemon.
/// Test: itself.
#[tokio::test]
async fn protocol_check_reads_a_pre_handshake_daemon_as_pre_handshake() {
    let fake = serve(
        RpcRouter::new().typed::<Value, Value, _, _>("memory.health", |_| async {
            Ok(json!({ "status": "ok" }))
        }),
    );

    let verdict = check_memory_protocol_at(&fake.socket, BUDGET)
        .await
        .expect("a pre-handshake daemon is callable by rule");

    assert_eq!(verdict, MemoryProtocol::PreHandshake);
}

/// Why (#9288, Fail-Open Check): a version query that fails must never fall
/// through to a callable verdict. Only method-not-found reads as
/// pre-handshake; every other refusal, an unreadable body, and a dead socket
/// are errors.
/// Test: itself.
#[tokio::test]
async fn protocol_check_fails_closed_when_the_query_fails() {
    let refusals = [
        RpcError::internal("handshake handler failed"),
        RpcError::new(CODE_NOT_FOUND, "not found"),
        RpcError::invalid_params("bad params"),
    ];
    for error in refusals {
        let code = error.code;
        let fake = serve(refusing(error));
        let err = check_memory_protocol_at(&fake.socket, BUDGET)
            .await
            .expect_err("a refused query is not callable");
        assert!(
            matches!(err, MemoryProtocolError::HandshakeFailed { .. }),
            "code {code}: expected HandshakeFailed, got {err:?}"
        );
    }

    let bodies = [
        json!({ "protocol_version": "1" }),
        json!({ "protocol_version": -1 }),
        json!({}),
        json!("ok"),
    ];
    for body in bodies {
        let fake = serve(answering(body.clone()));
        let err = check_memory_protocol_at(&fake.socket, BUDGET)
            .await
            .expect_err("an unreadable answer is not callable");
        assert!(
            matches!(err, MemoryProtocolError::MalformedHandshake { .. }),
            "body {body}: expected MalformedHandshake, got {err:?}"
        );
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let err = check_memory_protocol_at(&dir.path().join("absent.sock"), BUDGET)
        .await
        .expect_err("a dead socket is not callable");
    assert!(
        matches!(err, MemoryProtocolError::HandshakeFailed { .. }),
        "dead socket: expected HandshakeFailed, got {err:?}"
    );
}

/// Why (#9288): a refusal must not be remembered, or a daemon restarted into
/// the installed release stays refused; a callable verdict is reused inside
/// the recheck interval.
/// Test: itself.
#[tokio::test]
async fn an_unsupported_verdict_is_not_cached() {
    let reported = Arc::new(AtomicU64::new(SUPPORTED_MEMORY_PROTOCOLS.end() + 1));
    let answer = Arc::clone(&reported);
    let fake = serve(
        RpcRouter::new().typed::<Value, Value, _, _>(METHOD_PROTOCOL, move |_| {
            let version = answer.load(Ordering::SeqCst);
            async move { Ok(json!({ "protocol_version": version })) }
        }),
    );

    let first = ensure_memory_protocol_at(&fake.socket, BUDGET).await;
    assert!(
        matches!(first, Err(MemoryProtocolError::Unsupported { .. })),
        "{first:?}"
    );

    reported.store(*SUPPORTED_MEMORY_PROTOCOLS.end(), Ordering::SeqCst);
    let healed = ensure_memory_protocol_at(&fake.socket, BUDGET).await;
    assert!(
        matches!(healed, Ok(MemoryProtocol::Supported(_))),
        "the refusal was cached: {healed:?}"
    );

    reported.store(SUPPORTED_MEMORY_PROTOCOLS.end() + 1, Ordering::SeqCst);
    let reused = ensure_memory_protocol_at(&fake.socket, BUDGET).await;
    assert!(
        matches!(reused, Ok(MemoryProtocol::Supported(_))),
        "a callable verdict is reused inside the interval: {reused:?}"
    );
}

/// Why (#9288): `MemoryRpcError` used to drop the error object's `data`.
/// Test: itself.
#[tokio::test]
async fn memory_rpc_error_keeps_the_data_member() {
    let detail = json!({ "palace": "p", "retryable": false });
    let fake = serve(refusing(
        RpcError::internal("refused with detail").with_data(detail.clone()),
    ));

    let err = call_memory_tool_at(&fake.socket, METHOD_PROTOCOL, json!({}))
        .await
        .expect_err("the fake refuses");
    let typed = err
        .downcast_ref::<MemoryRpcError>()
        .expect("a typed refusal");

    assert_eq!(typed.data.as_ref(), Some(&detail));
}
