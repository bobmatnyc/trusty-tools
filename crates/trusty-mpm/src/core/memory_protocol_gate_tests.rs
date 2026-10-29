//! A daemon outside the supported memory protocol is refused by name (#9538).
//!
//! Why: #9288 added `memory.protocol`; the client side only helps if every
//! first-party call site asks before its first memory call. A verb or import
//! must refuse such a daemon; a health, banner or doctor probe must report it
//! as degraded with the reason, never hard-fail.
//! What: one fake daemon on a socket that reports protocol 99 and answers every
//! other method with success, recording what it was sent.
//! Test: this file IS the test module.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use trusty_common::memory_rpc::MemoryProtocolError;

use crate::uds_mock::{MockUdsDaemon, spawn_with_protocol};

/// A daemon speaking protocol 99, and the methods it was asked beyond the handshake.
async fn future_daemon() -> (MockUdsDaemon, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&seen);
    let daemon = spawn_with_protocol(99, move |method: &str, _params: Value| {
        if let Ok(mut rec) = rec.lock() {
            rec.push(method.to_string());
        }
        Box::pin(async { Ok(json!({"status": "ok", "drawers": [], "drawer_id": "d1"})) })
    })
    .await;
    (daemon, seen)
}

fn no_calls(seen: &Arc<Mutex<Vec<String>>>) {
    let seen = seen.lock().expect("recorded");
    assert!(seen.is_empty(), "no memory method may reach it: {seen:?}");
}

#[tokio::test]
async fn memory_verb_refuses_an_incompatible_daemon_before_calling() {
    let (daemon, seen) = future_daemon().await;
    let err = crate::core::memory_verbs::call_method(daemon.socket(), "memory_list", json!({}))
        .await
        .expect_err("an unsupported protocol must be refused");
    assert!(
        err.to_string()
            .contains("unsupported trusty-memory protocol"),
        "named protocol error expected: {err}"
    );
    no_calls(&seen);
}

#[tokio::test]
async fn memory_import_write_refuses_an_incompatible_daemon() {
    let (daemon, seen) = future_daemon().await;
    let err = crate::core::memory_import::remember(daemon.socket(), "p", "t", &[], false)
        .await
        .expect_err("an unsupported protocol must be refused");
    assert!(
        matches!(
            err.downcast_ref::<MemoryProtocolError>(),
            Some(MemoryProtocolError::Unsupported { daemon: 99, .. })
        ),
        "wrapped MemoryProtocolError expected: {err:#}"
    );
    no_calls(&seen);
}

#[tokio::test]
async fn reachability_probe_reports_the_protocol_mismatch() {
    let (daemon, _seen) = future_daemon().await;
    let verdict = crate::core::memory_reachable::probe_socket_reachability(daemon.socket()).await;
    assert!(!verdict.is_reachable(), "{verdict:?}");
    assert!(
        verdict.describe().contains("protocol"),
        "reason expected: {}",
        verdict.describe()
    );
}

#[cfg(feature = "tui")]
#[tokio::test]
async fn banner_probe_reports_the_protocol_mismatch_as_unreachable_with_a_note() {
    let (daemon, seen) = future_daemon().await;
    let outcome = crate::tui::coordinator::banner::probe_memory(Some(daemon.socket())).await;
    assert!(!outcome.active, "{outcome:?}");
    assert!(
        outcome
            .note
            .as_deref()
            .is_some_and(|n| n.contains("protocol")),
        "reason expected: {outcome:?}"
    );
    no_calls(&seen);
}
