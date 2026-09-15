//! Tests for the `SubagentStop` POST's retry-plus-log delivery (#6556).
//!
//! Why: the whole fix is "a transient failure is retried, a permanent one is
//! recorded, and neither overruns the hook's budget". Each of those three is a
//! separate way to reintroduce the six-hour leak, so each gets its own case
//! against a real loopback daemon stub — the failure being fixed is a transport
//! failure, and a mocked client would assert the mock.
//! Test: this *is* the test module.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// A `POST /hooks` stub that fails its first `fail_first` requests.
///
/// Why: "fails N times then succeeds" is the exact shape of the daemon blip the
/// retry exists to cross, and the counter is how "N+1 attempts" is measured at
/// the wire rather than inferred from the caller.
struct HooksStub {
    seen: Arc<AtomicU32>,
    fail_first: u32,
    fail_status: axum::http::StatusCode,
}

impl HooksStub {
    fn new(fail_first: u32, fail_status: axum::http::StatusCode) -> Self {
        Self {
            seen: Arc::new(AtomicU32::new(0)),
            fail_first,
            fail_status,
        }
    }

    /// Serve this stub on a fresh loopback port; returns its base URL.
    async fn serve(&self) -> String {
        use axum::routing::post;

        let seen = Arc::clone(&self.seen);
        let fail_first = self.fail_first;
        let fail_status = self.fail_status;
        let router = axum::Router::new().route(
            "/hooks",
            post(move || {
                let seen = Arc::clone(&seen);
                async move {
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    if n < fail_first {
                        (fail_status, "stub refusal")
                    } else {
                        (axum::http::StatusCode::OK, "{}")
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback port");
        let addr: SocketAddr = listener.local_addr().expect("resolve bound addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{addr}")
    }

    fn requests(&self) -> u32 {
        self.seen.load(Ordering::SeqCst)
    }
}

/// One `POST /hooks` body in the shape `commands::misc::hook` builds.
fn stop_body() -> serde_json::Value {
    serde_json::json!({
        "session_id": "11111111-1111-1111-1111-111111111111",
        "event": "SubagentStop",
        "payload": {"agent_id": "a403cdbc078b5c474"},
    })
}

/// How many attempt lines the outcome carries, ignoring the verdict line.
fn attempt_lines(outcome: &HookPostOutcome) -> Vec<&String> {
    outcome
        .lines
        .iter()
        .filter(|l| l.contains("attempt "))
        .collect()
}

/// 🔴 REGRESSION (#6556): a daemon blip must not lose the stop.
///
/// Why: this is the fix. Two failures then a success is a daemon mid-restart —
/// the case that left the delegation `Running` for six hours because the single
/// fire-and-forget attempt landed in the window. Exactly one stop reaches the
/// daemon (the retry must not double-record), and every attempt is logged.
/// Fails at 9727aa358, where the POST is `let _ = ….send().await`: one request,
/// no retry, nothing logged.
#[tokio::test]
async fn a_stop_delivered_on_the_third_attempt_logs_every_one() {
    let stub = HooksStub::new(2, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    let url = stub.serve().await;

    let outcome = post_hook_with_retry(&url, &stop_body()).await;

    assert!(outcome.delivered, "lines: {:?}", outcome.lines);
    assert_eq!(outcome.attempts, 3, "two failures then a success");
    assert_eq!(
        stub.requests(),
        3,
        "the daemon saw exactly one successful stop, after two refusals"
    );
    assert_eq!(
        attempt_lines(&outcome).len(),
        3,
        "every attempt is logged, not only the last: {:?}",
        outcome.lines
    );
    assert!(
        outcome
            .lines
            .last()
            .is_some_and(|l| l.contains("delivered after 3 attempt(s)")),
        "the verdict names the outcome and the count: {:?}",
        outcome.lines
    );
}

/// A healthy daemon costs exactly one request — the retry is not a tax.
#[tokio::test]
async fn a_stop_delivered_first_try_makes_one_attempt() {
    let stub = HooksStub::new(0, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    let url = stub.serve().await;

    let outcome = post_hook_with_retry(&url, &stop_body()).await;

    assert!(outcome.delivered);
    assert_eq!(outcome.attempts, 1);
    assert_eq!(stub.requests(), 1);
}

/// 🔴 REGRESSION (#6556): a daemon that never answers exhausts the attempts,
/// reports UNDELIVERED, and returns inside the hook's budget.
///
/// Why: the permanent-failure arm. The caller keys the disk record off
/// `delivered == false`, so a run that silently reported success here would park
/// nothing and the leak would be unchanged.
#[tokio::test]
async fn a_daemon_that_never_answers_exhausts_the_attempts() {
    let stub = HooksStub::new(u32::MAX, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let url = stub.serve().await;

    let started = std::time::Instant::now();
    let outcome = post_hook_with_retry(&url, &stop_body()).await;
    let elapsed = started.elapsed();

    assert!(!outcome.delivered, "lines: {:?}", outcome.lines);
    assert_eq!(outcome.attempts, HOOK_POST_ATTEMPTS);
    assert_eq!(stub.requests(), HOOK_POST_ATTEMPTS);
    assert!(
        elapsed <= HOOK_POST_BUDGET + std::time::Duration::from_millis(500),
        "the loop must return inside its budget; took {elapsed:?}"
    );
    assert!(
        outcome
            .lines
            .last()
            .is_some_and(|l| l.contains("UNDELIVERED after 3 attempt(s)")),
        "the verdict is what an operator greps for: {:?}",
        outcome.lines
    );
}

/// A body the daemon refuses is not retried — no retry fixes a malformed
/// payload, and spending the budget on one delays the spool write.
#[tokio::test]
async fn a_refused_body_is_not_retried() {
    let stub = HooksStub::new(u32::MAX, axum::http::StatusCode::BAD_REQUEST);
    let url = stub.serve().await;

    let outcome = post_hook_with_retry(&url, &stop_body()).await;

    assert!(!outcome.delivered);
    assert_eq!(outcome.attempts, 1, "a 400 is permanent");
    assert_eq!(stub.requests(), 1);
    assert!(
        outcome.lines.iter().any(|l| l.contains("permanent")),
        "the line says why it stopped: {:?}",
        outcome.lines
    );
}

/// An unreachable port is a transport error, not a status — still transient.
#[tokio::test]
async fn an_unreachable_daemon_retries_and_reports_undelivered() {
    // Bind and drop, so the port is almost certainly free and refuses.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback port");
    let addr: SocketAddr = listener.local_addr().expect("resolve bound addr");
    drop(listener);

    let outcome = post_hook_with_retry(&format!("http://{addr}"), &stop_body()).await;

    assert!(!outcome.delivered);
    assert_eq!(outcome.attempts, HOOK_POST_ATTEMPTS);
}

/// 🔴 The budget gate: every bound a `SubagentStop` invocation pays in series
/// has to fit inside the timeout the hook is registered with.
///
/// Why (#6556, the #7975 pattern): the retry is only safe because of this
/// arithmetic, and the arithmetic is only checked if it is written down. Summing
/// all four means lowering the registered timeout — or raising any one budget —
/// fails here rather than in production, where the symptom is a killed hook and
/// a lost stop, the very defect being fixed.
#[test]
fn the_stop_post_budget_stays_inside_the_registered_hook_timeout() {
    let registered =
        registered_subagent_stop_timeout().expect("the SubagentStop group carries a timeout");
    let worst_case = trusty_mpm::core::discovery::GATEWAY_PROBE_TIMEOUT
        + crate::commands::hook_stdin::HOOK_STDIN_TIMEOUT
        + crate::commands::misc::IDLE_PARK_DETECT_TIMEOUT
        + HOOK_POST_BUDGET;
    let headroom = registered.checked_sub(worst_case).unwrap_or_default();
    assert!(
        headroom >= std::time::Duration::from_secs(1),
        "worst case {worst_case:?} of the registered {registered:?} leaves only {headroom:?} for \
         exec, the spool write and teardown"
    );
}

/// The reader itself, so a shape change in the hook block fails HERE rather
/// than silently turning the budget gate above into a no-op.
#[test]
fn the_registered_subagent_stop_timeout_is_readable() {
    assert_eq!(
        registered_subagent_stop_timeout(),
        Some(std::time::Duration::from_secs(5)),
        "the SubagentStop hook is registered with a 5 s timeout"
    );
}

/// The per-attempt bounds must at least be capable of fitting the budget, or
/// the budget would be the only thing ending the loop and the attempt count
/// would be a fiction.
#[test]
fn the_attempt_bounds_fit_inside_the_budget() {
    let backoffs: std::time::Duration =
        (1..HOOK_POST_ATTEMPTS).map(|n| HOOK_POST_BACKOFF * n).sum();
    let nominal = HOOK_POST_ATTEMPT_TIMEOUT * HOOK_POST_ATTEMPTS + backoffs;
    assert!(
        nominal <= HOOK_POST_BUDGET + HOOK_POST_ATTEMPT_TIMEOUT,
        "nominal worst case {nominal:?} overruns the {HOOK_POST_BUDGET:?} budget by more than \
         one attempt, so the budget would routinely truncate the retry"
    );
    assert!(HOOK_POST_CONNECT_TIMEOUT <= HOOK_POST_ATTEMPT_TIMEOUT);
}
