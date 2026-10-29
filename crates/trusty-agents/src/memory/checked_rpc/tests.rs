//! Tests for the protocol-checked memory call door (#9538).
//!
//! Test: this *is* the test module.

use super::*;
use crate::uds_mock::{self, MockFuture};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use trusty_common::memory_rpc::{MemoryProtocolError, SUPPORTED_MEMORY_PROTOCOLS};

/// A handler that counts the application calls it receives.
fn counting(hits: &Arc<AtomicUsize>) -> impl Fn(&str, Value) -> MockFuture + Send + Sync + 'static {
    let hits = Arc::clone(hits);
    move |_method: &str, _params: Value| {
        hits.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!({"ok": true})) })
    }
}

/// Why: a daemon from another release must be refused by name before any
/// application call reaches it.
/// What: a daemon reporting a protocol past the supported range; `call` must
/// fail with `MemoryProtocolError::Unsupported` and the handler must see no call.
/// Test: this test.
#[tokio::test]
async fn an_unsupported_daemon_is_refused_before_the_call() {
    let hits = Arc::new(AtomicUsize::new(0));
    let beyond = SUPPORTED_MEMORY_PROTOCOLS.end() + 1;
    let daemon = uds_mock::spawn_with_protocol(beyond, counting(&hits)).await;

    let err = call(daemon.socket(), "memory.health", json!({}))
        .await
        .expect_err("an incompatible daemon must be refused");

    let named = err.downcast_ref::<MemoryProtocolError>();
    assert!(
        matches!(named, Some(MemoryProtocolError::Unsupported { daemon, .. }) if *daemon == beyond),
        "expected Unsupported, got {err:#}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the call must not be sent");
}

/// Why: the check must not break a compatible daemon.
/// Test: this test.
#[tokio::test]
async fn a_supported_daemon_is_called_through() {
    let hits = Arc::new(AtomicUsize::new(0));
    let daemon = uds_mock::spawn(counting(&hits)).await;

    let value = call(daemon.socket(), "memory.health", json!({}))
        .await
        .expect("a compatible daemon is called");

    assert_eq!(value, json!({"ok": true}));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
