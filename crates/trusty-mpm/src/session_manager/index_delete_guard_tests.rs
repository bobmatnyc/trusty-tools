//! Who may acquire the destructive-delete capability, and what it reports when
//! the daemon does not confirm a removal (#4743, #6285).
//!
//! Why these two things together: #4743's guarantee is that a `cargo test`
//! process never reaches a daemon on this path, and #6285 moved the path onto a
//! Unix socket. A transport move is exactly where a guard silently stops
//! applying, and exactly where a failure arm silently starts reading as success
//! — so the acquisition rules and the outcome classification are pinned in one
//! place.
//!
//! Why a real mock daemon rather than a dead path for most of these: a dead path
//! makes "the guard refused" and "nothing was listening" indistinguishable. The
//! daemon here WOULD answer, so a refusal is evidence about this crate's
//! behaviour rather than about the machine.
//!
//! Test: the eight functions below.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::test_support::isolated_daemon_home;
use crate::uds_mock::{self, MockFuture, RpcError};

/// The socket production resolution derives under the active override — the
/// path `acquire` will look at and a mock daemon must bind.
fn resolved_socket() -> PathBuf {
    trusty_common::daemon_socket_path("trusty-search").expect("derive the trusty-search socket")
}

/// Every `(method, params)` one mock daemon was asked for.
type Recorder = Arc<Mutex<Vec<(String, Value)>>>;

/// A handler that records what it was called with and answers `result`.
fn recording(seen: Recorder, result: Value) -> impl Fn(&str, Value) -> MockFuture + Send + Sync {
    move |method, params| {
        seen.lock()
            .expect("recorder lock")
            .push((method.to_string(), params));
        let result = result.clone();
        Box::pin(async move { Ok(result) })
    }
}

/// A handler that refuses every call with `code`.
fn refusing(code: i64, message: &str) -> impl Fn(&str, Value) -> MockFuture + Send + Sync {
    let message = message.to_string();
    move |_method, _params| {
        let error = RpcError::new(code, message.clone());
        Box::pin(async move { Err(error) })
    }
}

// ---- acquisition ---------------------------------------------------------

/// The load-bearing assertion, made from inside the very thing it detects:
/// this test IS a `cargo test` process, so `acquire` must refuse it.
///
/// Why no injection: a decision table with fabricated inputs would prove the
/// precedence rules (`trusty-common` already tests those) but not the only fact
/// that matters here — that a real trusty-mpm test binary is classified as one.
/// Reverting the `running_under_test_harness` branch in `acquire` fails this on
/// any machine with a running trusty-search daemon.
/// Test: this function IS the test.
#[test]
fn acquire_is_refused_under_a_test_harness() {
    assert!(
        DestructiveIndexDelete::acquire().is_none(),
        "a cargo test process must never acquire the destructive-delete capability (#4743)"
    );
}

/// #6285's replacement for the "no `http_addr` discovery file" refusal: with the
/// harness check lifted and nothing bound, there is no daemon to destroy
/// anything with.
///
/// Why it matters: the socket path resolves whether or not a daemon exists —
/// unlike the old discovery file, whose absence WAS the answer. Without this
/// arm the capability would be handed out on every machine, and the first thing
/// that noticed would be a failed request.
/// Test: this function IS the test.
#[serial_test::serial]
#[test]
fn acquire_refuses_when_no_daemon_socket_is_bound() {
    let (_dir, _scope) = isolated_daemon_home(true);
    assert!(
        DestructiveIndexDelete::acquire().is_none(),
        "no bound socket means no daemon to delete through"
    );
}

/// The escape hatch works, and the guard is not over-applied into a permanent
/// disablement of the orphan GC.
///
/// Why: a refusal that could never be lifted would be indistinguishable from
/// having deleted the feature, and nothing would notice if `acquire` started
/// returning `None` unconditionally.
/// What: with `TRUSTY_ALLOW_PRODUCTION_STATE=1` and a mock daemon bound where
/// production resolution looks, `acquire` yields a capability pointed at THAT
/// socket — the isolated one, never the operator's.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn acquire_succeeds_when_production_state_is_explicitly_allowed() {
    let (_dir, _scope) = isolated_daemon_home(true);
    let socket = resolved_socket();
    let _daemon = uds_mock::spawn_at(socket.clone(), uds_mock::always(json!({}))).await;

    let acquired = DestructiveIndexDelete::acquire().expect("the explicit opt-in must yield one");
    assert_eq!(
        acquired.socket, socket,
        "the capability must point at the resolved socket"
    );
}

// ---- what goes on the wire -----------------------------------------------

/// The opt-in that makes the call destructive must survive refactoring — a
/// request that lost `delete_data` would silently leak index data forever
/// (#4123), the failure this whole module is downstream of.
///
/// Why at the wire rather than on the params function: the method NAME is the
/// other half. trusty-mpm has no Cargo edge on trusty-search, so a drifted name
/// answers `method_not_found` and nothing local catches it.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn delete_params_opt_into_data_deletion() {
    let (_dir, _scope) = isolated_daemon_home(true);
    let seen: Recorder = Arc::new(Mutex::new(Vec::new()));
    let _daemon = uds_mock::spawn_at(
        resolved_socket(),
        recording(Arc::clone(&seen), json!({ "removed": true })),
    )
    .await;

    let cap = DestructiveIndexDelete::acquire().expect("capability");
    cap.delete("my-index").await;

    let calls = seen.lock().expect("recorder lock").clone();
    assert_eq!(
        calls,
        vec![(
            "search.index.delete".to_string(),
            json!({ "index_id": "my-index", "delete_data": true })
        )],
        "the destructive call must name trusty-search's own method and carry the opt-in"
    );
}

// ---- failure arms: nothing unconfirmed may read as a removal --------------

/// A socket file that outlived its daemon is a transport failure, never a
/// removal.
///
/// Why this shape: `acquire`'s existence check passes on a stale socket, so the
/// last thing standing between a crashed daemon and a falsely-recorded delete is
/// how `delete` classifies a dial failure. A plain file at the socket path
/// reproduces that exactly — resolution succeeds, connection does not.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn delete_over_a_stale_socket_is_a_transport_failure() {
    let (_dir, _scope) = isolated_daemon_home(true);
    let socket = resolved_socket();
    std::fs::create_dir_all(socket.parent().expect("socket parent"))
        .expect("create the daemon dir");
    std::fs::write(&socket, b"").expect("leave a stale socket file");

    let cap = DestructiveIndexDelete::acquire().expect("a stale socket still resolves");
    let outcome = cap.delete("my-index").await;

    assert!(
        matches!(outcome, DeleteOutcome::Transport(_)),
        "an unanswered delete must report the transport failure, never a removal: {outcome:?}"
    );
}

/// A daemon that refuses reports its own code, and reports nothing removed.
///
/// Why: `search.index.delete` answers not-found for an index the daemon does not
/// hold (#6363). Reading that as "gone, then" would let the sweep record a
/// removal it never performed.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn a_refusal_is_never_reported_as_removed() {
    let (_dir, _scope) = isolated_daemon_home(true);
    let _daemon = uds_mock::spawn_at(
        resolved_socket(),
        refusing(
            crate::daemon::error::CODE_NOT_FOUND,
            "unknown index: my-index",
        ),
    )
    .await;

    let cap = DestructiveIndexDelete::acquire().expect("capability");
    let outcome = cap.delete("my-index").await;

    match outcome {
        DeleteOutcome::Refused { code, ref message } => {
            assert_eq!(code, crate::daemon::error::CODE_NOT_FOUND);
            assert!(message.contains("unknown index"), "message: {message}");
        }
        other => panic!("a refusal must carry the daemon's own code: {other:?}"),
    }
}

/// The #3049 arm: the daemon ANSWERED, and its answer says the index is still
/// there.
///
/// Why: with `delete_data` requested and an in-flight writer that never
/// quiesced, trusty-search abandons the delete and reports `removed: false` in a
/// SUCCESS body. Over HTTP that was a 200, and any-2xx-is-success recorded the
/// index as reclaimed while every byte of it was still on disk. Preserving that
/// bug across the transport move is what this pins against.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn a_result_that_did_not_remove_is_not_reported_as_removed() {
    let (_dir, _scope) = isolated_daemon_home(true);
    let _daemon = uds_mock::spawn_at(
        resolved_socket(),
        uds_mock::always(json!({ "ok": true, "removed": false, "quiesced": false })),
    )
    .await;

    let cap = DestructiveIndexDelete::acquire().expect("capability");
    let outcome = cap.delete("my-index").await;

    match outcome {
        DeleteOutcome::NotRemoved(ref detail) => {
            assert!(detail.contains("quiesced: false"), "detail: {detail}");
        }
        other => panic!("an abandoned delete must not read as a removal: {other:?}"),
    }
}

/// The fixture premise for the two arms above: a daemon that DOES remove the
/// index is reported as a removal.
///
/// Why: without it, `classify_delete_result` returning `NotRemoved`
/// unconditionally would pass every other test in this file.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn a_result_reporting_removal_is_removed() {
    let (_dir, _scope) = isolated_daemon_home(true);
    let _daemon = uds_mock::spawn_at(
        resolved_socket(),
        uds_mock::always(json!({ "ok": true, "removed": true, "data_deleted": true })),
    )
    .await;

    let cap = DestructiveIndexDelete::acquire().expect("capability");
    let outcome = cap.delete("my-index").await;

    assert!(
        matches!(outcome, DeleteOutcome::Removed),
        "a confirmed removal must read as one: {outcome:?}"
    );
}
