//! Unit tests for the shared trusty-search socket client (#7237).
//!
//! Why: isolated in a sibling file (declared via `#[path =
//! "search_rpc_tests.rs"] mod tests;` in `search_rpc.rs`) so the production
//! module stays well under the 500-SLOC cap. As a child module, `super::`
//! reaches its private items.
//!
//! What: the socket-path override, the dial-failure arm both entry points
//! share, and the blocking wrapper's two answering arms — a `result` and the
//! daemon's own coded refusal.
//!
//! Test: `cargo test -p trusty-common --features uds -- search_rpc::tests`

use super::*;

use crate::uds_mock::{self, RpcError};

/// Long enough that a loaded machine's loopback round trip is not a flake,
/// short enough that a dead socket does not stall the suite.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Why: the override is the only way a probe can be pointed at a daemon a
/// test started without redirecting every other trusty-* client in the
/// process through `TRUSTY_DATA_DIR_OVERRIDE`.
///
/// It holds `crate::data_dir::ENV_LOCK` rather than `#[serial_test::serial]`,
/// even though nothing here reads the data dir: `search_index`'s rigs set this
/// same variable under that lock, and two DIFFERENT mutexes serialise nothing
/// against each other — this test's `remove_var` wiped a running rig's override
/// mid-registration and it reported `DaemonUnreachable`.
/// Test: itself.
#[test]
fn search_socket_honours_the_env_override() {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // SAFETY: guarded by ENV_LOCK; removed below before returning.
    unsafe {
        std::env::set_var(TRUSTY_SEARCH_SOCKET_ENV, "/tmp/example-search.sock");
    }
    let resolved = search_socket();
    unsafe {
        std::env::remove_var(TRUSTY_SEARCH_SOCKET_ENV);
    }
    assert_eq!(
        resolved.expect("an override always resolves"),
        PathBuf::from("/tmp/example-search.sock")
    );
}

/// Why: every probe's failure arm depends on a dead socket FAILING rather
/// than consuming the budget — `tm doctor` must not hang on an absent
/// daemon, and `search_index`'s registration must not stall a session launch.
/// Test: itself.
#[tokio::test]
async fn call_at_reports_a_dead_socket_rather_than_hanging() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let err = call_at(&socket, METHOD_HEALTH, serde_json::json!({}), PROBE_TIMEOUT)
        .await
        .expect_err("nothing is listening");
    assert!(
        err.downcast_ref::<SearchRpcError>().is_none(),
        "a dial failure is a transport error, never a daemon refusal: {err:#}"
    );
}

/// Why: [`call_blocking`] is the entry point `search_index` uses from a
/// synchronous function that is itself frequently called from inside a tokio
/// runtime. Driving it from `spawn_blocking` inside a `#[tokio::test]` is that
/// arrangement exactly — if the wrapper built its runtime on the caller's
/// thread instead of its own, this would panic rather than answer.
/// Test: itself.
#[tokio::test]
async fn call_blocking_round_trips_against_a_listening_daemon() {
    let daemon = uds_mock::spawn(|method, params| {
        let answer = serde_json::json!({ "method": method, "params": params });
        Box::pin(async move { Ok(answer) })
    })
    .await;
    let socket = daemon.socket().to_path_buf();

    let value = tokio::task::spawn_blocking(move || {
        call_blocking(
            &socket,
            METHOD_INDEX_CREATE,
            serde_json::json!({ "id": "api" }),
            PROBE_TIMEOUT,
        )
    })
    .await
    .expect("the blocking worker must not panic")
    .expect("the daemon answered");

    assert_eq!(
        value.get("method").and_then(serde_json::Value::as_str),
        Some(METHOD_INDEX_CREATE),
        "the method name must reach the daemon verbatim: {value}"
    );
    assert_eq!(
        value
            .pointer("/params/id")
            .and_then(serde_json::Value::as_str),
        Some("api"),
        "the params must reach the daemon verbatim: {value}"
    );
}

/// Why: the caller that matters branches on the CODE — a `409` is recoverable
/// (#6864) and a `404` is a definite verdict (#5045). A refusal flattened into
/// a formatted string would make both indistinguishable from a transport
/// failure.
/// Test: itself.
#[tokio::test]
async fn call_blocking_carries_the_daemons_own_error_code() {
    let daemon = uds_mock::spawn(|_method, _params| {
        Box::pin(async move { Err(RpcError::new(CODE_CONFLICT, "root_path is taken")) })
    })
    .await;
    let socket = daemon.socket().to_path_buf();

    let err = tokio::task::spawn_blocking(move || {
        call_blocking(
            &socket,
            METHOD_INDEX_CREATE,
            serde_json::json!({}),
            PROBE_TIMEOUT,
        )
    })
    .await
    .expect("the blocking worker must not panic")
    .expect_err("the daemon refused");

    let refusal = err
        .downcast_ref::<SearchRpcError>()
        .expect("a daemon refusal must arrive typed, not as an opaque string");
    assert!(refusal.is_conflict(), "code was {}", refusal.code);
    assert!(!refusal.is_not_found());
    assert_eq!(refusal.message, "root_path is taken");
}

/// Why: an absent socket is what "trusty-search is not running" looks like, and
/// the registration path treats it as fail-closed rather than falling back to
/// anything. It must come back promptly and as a transport failure.
/// Test: itself.
#[test]
fn call_blocking_reports_a_dead_socket_rather_than_hanging() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let err = call_blocking(&socket, METHOD_HEALTH, serde_json::json!({}), PROBE_TIMEOUT)
        .expect_err("nothing is listening");
    assert!(
        err.downcast_ref::<SearchRpcError>().is_none(),
        "a dial failure is a transport error, never a daemon refusal: {err:#}"
    );
}
