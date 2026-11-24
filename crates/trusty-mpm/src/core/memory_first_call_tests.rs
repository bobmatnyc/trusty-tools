//! #10648: a palace verb survives a daemon that stalls its first exchange.
//!
//! Why: the daemon re-opens an idle-evicted palace inside the first request,
//! and that open outran the client's budget.
//! What: a stub daemon that stalls chosen calls past a short budget; the
//! tests pin the read retry, the write warm-up, the never-resent write, and
//! that a non-timeout failure is not retried.
//! Test: this file.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::memory_first_call::{WARM_METHOD, call_verb};
use crate::uds_mock::RpcError;

const BUDGET: Duration = Duration::from_millis(300);
const STALL: Duration = Duration::from_millis(900);

type Calls = Arc<Mutex<Vec<String>>>;

/// A stub that stalls the first `stalls` calls of `stalled_method` past
/// [`BUDGET`], answers everything else at once, and records every method.
async fn stalling_daemon(
    stalled_method: &'static str,
    stalls: usize,
) -> (crate::uds_mock::MockUdsDaemon, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let count = Arc::new(AtomicUsize::new(0));
    let daemon = crate::uds_mock::spawn(move |method: &str, _params: Value| {
        if let Ok(mut seen) = seen.lock() {
            seen.push(method.to_string());
        }
        let stall = method == stalled_method && count.fetch_add(1, Ordering::SeqCst) < stalls;
        Box::pin(async move {
            if stall {
                tokio::time::sleep(STALL).await;
            }
            Ok(json!({ "ok": true }))
        })
    })
    .await;
    (daemon, calls)
}

fn methods(calls: &Calls) -> Vec<String> {
    calls.lock().expect("calls").clone()
}

#[tokio::test]
async fn a_read_retries_once_after_a_stalled_first_exchange() {
    let (daemon, calls) = stalling_daemon("memory_recall", 1).await;
    let out = call_verb(
        daemon.socket(),
        "memory_recall",
        json!({ "palace": "p", "query": "q" }),
        false,
        BUDGET,
    )
    .await
    .expect("the retry must succeed");
    assert_eq!(out, json!({ "ok": true }));
    assert_eq!(methods(&calls), vec!["memory_recall", "memory_recall"]);
}

/// The repro: `run_verb`'s own door, a 900 ms stall against a 300 ms budget.
#[tokio::test]
async fn call_with_timeout_retries_a_stalled_first_recall() {
    let (daemon, calls) = stalling_daemon("memory_recall", 1).await;
    let out = super::memory_checked::call_with_timeout(
        daemon.socket(),
        "memory_recall",
        json!({ "palace": "p", "query": "q" }),
        BUDGET,
    )
    .await
    .expect("the retry must succeed");
    assert_eq!(out, json!({ "ok": true }));
    let recalls = methods(&calls)
        .iter()
        .filter(|m| *m == "memory_recall")
        .count();
    assert_eq!(recalls, 2, "one stalled attempt, one retry");
}

#[tokio::test]
async fn a_read_stalled_twice_is_an_error() {
    let (daemon, calls) = stalling_daemon("memory_recall", 2).await;
    let err = call_verb(
        daemon.socket(),
        "memory_recall",
        json!({ "palace": "p", "query": "q" }),
        false,
        BUDGET,
    )
    .await
    .expect_err("a read that stalls twice must fail, not succeed");
    assert!(format!("{err:#}").contains("did not complete the exchange"));
    assert_eq!(methods(&calls), vec!["memory_recall", "memory_recall"]);
}

#[tokio::test]
async fn a_write_warms_its_palace_then_sends_once() {
    // The cold open stalls the warm-up, not the write.
    let (daemon, calls) = stalling_daemon(WARM_METHOD, 1).await;
    call_verb(
        daemon.socket(),
        "memory_remember",
        json!({ "palace": "p", "content": "c" }),
        true,
        BUDGET,
    )
    .await
    .expect("the write must go through after the warm-up retry");
    assert_eq!(
        methods(&calls),
        vec![WARM_METHOD, WARM_METHOD, "memory_remember"]
    );
}

#[tokio::test]
async fn a_write_is_not_sent_when_the_warm_up_times_out_twice() {
    let (daemon, calls) = stalling_daemon(WARM_METHOD, 2).await;
    let err = call_verb(
        daemon.socket(),
        "memory_remember",
        json!({ "palace": "p", "content": "c" }),
        true,
        BUDGET,
    )
    .await
    .expect_err("a palace that never opens must fail the verb");
    assert!(format!("{err:#}").contains("did not complete the exchange"));
    assert!(
        !methods(&calls).iter().any(|m| m == "memory_remember"),
        "the write must never be sent: {:?}",
        methods(&calls)
    );
}

#[tokio::test]
async fn a_stalled_write_is_not_resent() {
    let (daemon, calls) = stalling_daemon("memory_remember", 1).await;
    call_verb(
        daemon.socket(),
        "memory_remember",
        json!({ "palace": "p", "content": "c" }),
        true,
        BUDGET,
    )
    .await
    .expect_err("a timed-out write is reported, never re-sent");
    let sent = methods(&calls)
        .iter()
        .filter(|m| *m == "memory_remember")
        .count();
    assert_eq!(sent, 1, "a write may be sent once only");
}

#[tokio::test]
async fn a_refusal_is_not_retried() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let daemon = crate::uds_mock::spawn(move |method: &str, _params: Value| {
        if let Ok(mut seen) = seen.lock() {
            seen.push(method.to_string());
        }
        Box::pin(async move { Err(RpcError::internal("refused")) })
    })
    .await;
    call_verb(
        daemon.socket(),
        "memory_recall",
        json!({ "palace": "p", "query": "q" }),
        false,
        BUDGET,
    )
    .await
    .expect_err("a refusal is an error");
    assert_eq!(methods(&calls), vec!["memory_recall"]);
}
