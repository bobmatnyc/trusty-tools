//! The shared client and this daemon, proven against one real socket (#6286).
//!
//! Why: pass A moved the daemon onto a Unix socket and pass B moved every
//! consumer onto `trusty_common::memory_rpc`. Each half has its own unit tests,
//! and both can pass while the two disagree — a renamed method, a params shape
//! only one side knows, a frame budget one end refuses. Three literals in
//! particular are DUPLICATED across the dependency edge, because `trusty-common`
//! sits below this crate and cannot import them:
//!
//! - `memory.health`, which `trusty-console`'s connector and `tctl`'s probe dial
//!   by literal.
//! - `MAX_FRAME_BYTES`, which is symmetric: a client with a smaller budget
//!   refuses frames the daemon considers legal.
//! - `CODE_NOT_FOUND`, which trusty-agents reads to tell "no such palace" from a
//!   real failure.
//!
//! What: binds a daemon on a temp socket, then drives it through the SHARED
//! client — never through this crate's own — and pins the three literals.
//!
//! Test: this file.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;
use trusty_common::memory_rpc::{
    call_memory_tool_at, call_memory_tool_at_with_timeout, MemoryRpcError,
};
use trusty_memory::AppState;

/// Generous enough for a loaded machine; a local socket answers in microseconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// A running daemon on a temp socket, and the handle that stops it.
struct Daemon {
    socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
}

impl Daemon {
    /// Bind a temp socket, serve on it, and wait until it answers.
    async fn start() -> Self {
        // Seed the process-wide embedder cell with the mock so no test reaches
        // for the real ONNX model (#4413).
        trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();

        let data = tempfile::tempdir().expect("tempdir");
        let root = data.path().to_path_buf();
        std::mem::forget(data);
        // #88: bypass the project-slug gate so a test can create a palace with
        // no real project root on disk.
        // SAFETY: every test in this process wants the same idempotent "1".
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(root);
        // #911: flip past the warming preflight so handlers run.
        state.set_ready();

        let sockets = tempfile::tempdir().expect("tempdir");
        let socket = sockets.path().join("trusty-memory.sock");
        std::mem::forget(sockets);

        let (stop, shutdown) = oneshot::channel::<()>();
        let serve_socket = socket.clone();
        tokio::spawn(async move {
            let _ =
                trusty_memory::transport::uds::serve_with_shutdown(state, &serve_socket, async {
                    let _ = shutdown.await;
                })
                .await;
        });

        // Poll rather than sleep: the bind is fast but a loaded machine is not.
        for _ in 0..200 {
            if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(50)).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        Self {
            socket,
            stop: Some(stop),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Why: `memory.health` is dialled by literal from `trusty-console`'s connector
/// and `tctl`'s `uds_health_method`, in crates with no Cargo edge on this one.
/// A rename there is invisible to `cargo check` and shows up as every dashboard
/// reporting the daemon absent. This is what fails instead.
/// What: calls the literal through the shared client and asserts the daemon
/// answers a health envelope.
/// Test: this is the test.
#[tokio::test]
async fn shared_client_reaches_the_health_method_consumers_dial_by_literal() {
    let daemon = Daemon::start().await;

    let result =
        call_memory_tool_at_with_timeout(&daemon.socket, "memory.health", json!({}), CALL_TIMEOUT)
            .await
            .expect("the daemon answers memory.health");

    assert!(
        result.get("status").and_then(|v| v.as_str()).is_some(),
        "a health answer carries a status: {result}"
    );
}

/// Why: the dispatcher's ~75 methods reach the socket through the fallback, and
/// nothing in `FOLDED_METHODS` names them — so a consumer calling `palace_list`
/// or `memory_remember` is exercising a seam no unit test on either side spans.
/// What: creates a palace and lists it back, both through the shared client.
/// Test: this is the test.
#[tokio::test]
async fn shared_client_reaches_a_dispatcher_method_through_the_fallback() {
    let daemon = Daemon::start().await;

    call_memory_tool_at(
        &daemon.socket,
        "palace_create",
        json!({ "name": "contract-palace", "force": true }),
    )
    .await
    .expect("palace_create answers through the fallback");

    let listed = call_memory_tool_at(&daemon.socket, "palace_list", json!({}))
        .await
        .expect("palace_list answers");

    let palaces = listed["palaces"]
        .as_array()
        .expect("palace_list answers a palaces array");
    assert!(
        palaces
            .iter()
            .any(|p| p.as_str() == Some("contract-palace")),
        "the created palace comes back: {listed}"
    );
}

/// Why: the folded methods are the half `trusty-common`'s monitor client and
/// trusty-agents' backend call by name, and their params were flattened out of
/// path segments and query strings — a shape only this crate knows. A write
/// followed by a tagged read is the narrowest thing that proves both ends agree
/// on it.
/// What: creates a palace, writes a tagged drawer, and reads it back by tag —
/// exactly the `ensure_palace` → `insert` → `find_by_tag` sequence
/// `TrustyMemoryClient` performs.
/// Test: this is the test.
#[tokio::test]
async fn shared_client_round_trips_a_tagged_drawer_through_the_folded_methods() {
    let daemon = Daemon::start().await;

    call_memory_tool_at(
        &daemon.socket,
        "palace_create",
        json!({ "name": "drawer-palace", "force": true }),
    )
    .await
    .expect("palace_create");

    call_memory_tool_at(
        &daemon.socket,
        "memory.drawer_create",
        json!({
            "palace_id": "drawer-palace",
            "content": "the shared client wrote this",
            "tags": ["mem:brief:contract"],
            "force": true,
        }),
    )
    .await
    .expect("memory.drawer_create");

    let listed = call_memory_tool_at(
        &daemon.socket,
        "memory.drawers_list",
        json!({ "palace_id": "drawer-palace", "tag": "mem:brief:contract", "limit": 1 }),
    )
    .await
    .expect("memory.drawers_list");

    let rows = listed
        .as_array()
        .expect("memory.drawers_list answers an array");
    assert_eq!(rows.len(), 1, "exactly the tagged drawer: {listed}");
    assert_eq!(
        rows[0]["content"].as_str(),
        Some("the shared client wrote this")
    );
}

/// Why: trusty-agents' `get` and `delete` against a palace it never created
/// must be clean empty results, and they read that off
/// `MemoryRpcError::is_not_found`. That reads a code `trusty-common` duplicates
/// as a literal because it cannot import
/// `trusty_memory::transport::api_error::CODE_NOT_FOUND`. If the two drift, a
/// never-created palace starts surfacing as a hard error on every read.
/// What: asks for a palace that does not exist and asserts the shared client's
/// typed error says not-found.
/// Test: this is the test.
#[tokio::test]
async fn memory_rpc_not_found_code_matches_the_daemon() {
    let daemon = Daemon::start().await;

    let err = call_memory_tool_at(
        &daemon.socket,
        "memory.palace_get",
        json!({ "palace_id": "no-such-palace" }),
    )
    .await
    .expect_err("an absent palace is refused");

    let typed = err
        .downcast_ref::<MemoryRpcError>()
        .expect("the daemon's refusal arrives as a typed error");
    assert!(
        typed.is_not_found(),
        "expected not-found, got code {}: {}",
        typed.code,
        typed.message
    );
    assert_eq!(
        trusty_common::memory_rpc::CODE_NOT_FOUND,
        trusty_memory::transport::api_error::CODE_NOT_FOUND,
        "the shared client's copy of the code must equal the daemon's"
    );
}

/// Why: the frame budget is symmetric and duplicated. The daemon reads and
/// writes up to `transport::uds::MAX_FRAME_BYTES`; a client carrying
/// `trusty_common::uds::MAX_FRAME_BYTES` (8 MiB) instead would refuse frames the
/// daemon considers legal, which only moves which end reports the failure.
/// What: asserts the two constants are equal.
/// Test: this is the test.
#[test]
fn memory_rpc_frame_budget_matches_the_daemon() {
    assert_eq!(
        trusty_common::memory_rpc::MAX_FRAME_BYTES,
        trusty_memory::transport::uds::MAX_FRAME_BYTES,
        "the client's budget and the daemon's must be the same figure"
    );
}
