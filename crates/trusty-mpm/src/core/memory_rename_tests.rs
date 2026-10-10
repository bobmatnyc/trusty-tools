//! Tests for `tm memory rename` (#9544).
//!
//! Why: the two codes an operator acts on — -32601 and -32006 — are lost if
//! the call goes through a path that flattens them, and only a daemon on a
//! socket can show what the client does with each.
//! What: a stub daemon built from `trusty_common::uds::server`, answering the
//! handshake and `palace_rename` per test and recording every method it is
//! sent.
//! Test: this file IS the test module.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_common::memory_rpc::METHOD_PROTOCOL;
use trusty_common::uds::server::{
    CODE_METHOD_NOT_FOUND, RpcError, RpcFallback, RpcRouter, RpcServeOptions, serve_until,
};

use super::*;

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// How the stub answers.
#[derive(Clone, Copy)]
enum Daemon {
    /// Supports the handshake and answers `palace_rename` with success.
    Current,
    /// Supports the handshake and refuses `palace_rename` with -32006.
    Refusing,
    /// Answers -32601 to every method, as a daemon before both did.
    Old,
}

struct Stub {
    daemon: Daemon,
    calls: Calls,
}

#[async_trait]
impl RpcFallback for Stub {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.to_string(), params.clone()));
        }
        match (self.daemon, method) {
            (Daemon::Old, _) => Err(RpcError::new(CODE_METHOD_NOT_FOUND, "Method not found")),
            (_, m) if m == METHOD_PROTOCOL => Ok(json!({"protocol_version": 1})),
            (Daemon::Refusing, _) => Err(RpcError::new(
                CODE_REFUSED,
                "palace \"dst\" already exists and is not empty: it holds drawers",
            )),
            (Daemon::Current, _) => Ok(json!({"old": params["palace_id"], "new": params["new_id"]})),
        }
    }
}

async fn serve(socket: &Path, daemon: Daemon) -> (tokio::sync::oneshot::Sender<()>, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let listener = trusty_common::uds::bind_hardened(socket).expect("bind the stub socket");
    let router = Arc::new(RpcRouter::new().fallback(Stub {
        daemon,
        calls: Arc::clone(&calls),
    }));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = rx.await;
        })
        .await;
    });
    (tx, calls)
}

fn methods(calls: &Calls) -> Vec<String> {
    calls
        .lock()
        .expect("calls")
        .iter()
        .map(|(m, _)| m.clone())
        .collect()
}

/// Why (#9544): the daemon reads `palace_id`, `new_id` and `replace_empty`;
/// any other key is ignored and the rename fails as a missing argument.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rename_arguments_use_the_schema_keys() {
    assert_eq!(
        rename_arguments("src", "dst", true),
        json!({"palace_id": "src", "new_id": "dst", "replace_empty": true})
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(&socket, Daemon::Current).await;
    rename_palace_at(&socket, "src", "dst", false)
        .await
        .expect("rename");
    let sent = calls.lock().expect("calls").last().cloned().expect("a call");
    assert_eq!(sent.0, RENAME_METHOD);
    assert_eq!(sent.1, rename_arguments("src", "dst", false));
}

/// Why (#9544, A11): a daemon outside the supported protocol range must be
/// refused before the write is sent.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rename_calls_ensure_memory_protocol_at_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(&socket, Daemon::Current).await;
    rename_palace_at(&socket, "src", "dst", false)
        .await
        .expect("rename");
    assert_eq!(methods(&calls), vec![METHOD_PROTOCOL, RENAME_METHOD]);
}

/// Why (#9544, A11): a refusal carries the reason the operator acts on; a
/// generic failure would hide it.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rename_surfaces_refusal_32006_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, _calls) = serve(&socket, Daemon::Refusing).await;
    let err = rename_palace_at(&socket, "src", "dst", false)
        .await
        .expect_err("refused");
    assert!(matches!(err, MemoryRenameError::Refused { .. }), "{err:?}");
    assert!(err.to_string().contains("not empty"), "{err}");
}

/// Why (#9544, A11): an older daemon answers -32601; "method not found" alone
/// reads as a typo, so the client must say the daemon predates the method.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rename_against_a_daemon_that_answers_32601_says_it_predates_palace_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let (_stop, calls) = serve(&socket, Daemon::Old).await;
    let err = rename_palace_at(&socket, "src", "dst", false)
        .await
        .expect_err("an old daemon cannot rename");
    assert!(
        err.to_string().contains("predates palace_rename"),
        "{err}"
    );
    assert_eq!(methods(&calls).last().map(String::as_str), Some(RENAME_METHOD));
}
