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
//! #6555 adds one arm that does NOT go through the shared client: `tctl`'s
//! `probe_daemon_http` builds its own frame, and hand-supplying `json!({})` is
//! exactly what hid a probe that sent no `params` at all. That arm binds on the
//! DERIVED path rather than a temp one, because `tctl` takes no socket argument.
//!
//! Test: this file.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;
use trusty_common::memory_rpc::{
    call_memory_tool_at, call_memory_tool_at_with_timeout, MemoryRpcError,
};
use trusty_installer::commands::probe_http::{probe_daemon_http, ProbeOutcome};
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

/// Why: the `chat_*` tools are the ONE consumer path that wraps its call in an
/// MCP `tools/call` envelope, and nothing spanned that seam against the real
/// dispatcher. `persona_memory::call_memory_tool` passes method `"tools/call"`
/// with `{name, arguments}` into the shared client, and a reader who assumes the
/// client adds an envelope of its own reads that as a double-wrap that would
/// lose every persona chat turn silently. It does not add one —
/// `call_memory_tool_at_with_timeout` puts `method` and `params` straight into
/// the JSON-RPC request — but the only proof that held was trusty-agents' own
/// mock, which is a copy of the assumption under test (#6286 review, finding 1).
///
/// What: proves both halves of the asymmetry against a running daemon. Calling
/// `chat_session_create` DIRECTLY is refused, because `chat_*` is absent from
/// `transport::rpc::TOOL_METHODS` — which is why the envelope is there at all —
/// and the same tool through `tools/call` persists a turn `chat_session_get`
/// reads back. A double-wrapped request would fail the second half: the
/// dispatcher would hand `dispatch_tool` the name `"tools/call"`.
/// Test: this is the test.
#[tokio::test]
async fn shared_client_reaches_a_chat_tool_through_the_tools_call_envelope() {
    let daemon = Daemon::start().await;

    call_memory_tool_at(
        &daemon.socket,
        "palace_create",
        json!({ "name": "chat-palace", "force": true }),
    )
    .await
    .expect("palace_create");

    // Half one: no direct dispatch. `chat_*` is not in `TOOL_METHODS`.
    let direct = call_memory_tool_at(
        &daemon.socket,
        "chat_session_create",
        json!({ "palace": "chat-palace", "session_id": "persona-izzie" }),
    )
    .await
    .expect_err("chat_session_create is not directly dispatchable");
    let typed = direct
        .downcast_ref::<MemoryRpcError>()
        .expect("the daemon's refusal arrives as a typed error");
    assert_eq!(
        typed.code,
        i64::from(trusty_memory::transport::rpc::error_codes::METHOD_NOT_FOUND),
        "expected method-not-found, got: {typed}"
    );

    // Half two: the envelope the persona path actually sends.
    for (tool, arguments) in [
        (
            "chat_session_create",
            json!({ "palace": "chat-palace", "session_id": "persona-izzie" }),
        ),
        (
            "chat_turn_append",
            json!({
                "palace": "chat-palace",
                "session_id": "persona-izzie",
                "prompt": "what do you remember about me?",
                "response": "You live in Hastings-on-Hudson.",
            }),
        ),
    ] {
        call_memory_tool_at(
            &daemon.socket,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
        .await
        .unwrap_or_else(|e| panic!("{tool} through the tools/call envelope: {e:#}"));
    }

    let read_back = call_memory_tool_at(
        &daemon.socket,
        "tools/call",
        json!({
            "name": "chat_session_get",
            "arguments": { "palace": "chat-palace", "session_id": "persona-izzie" },
        }),
    )
    .await
    .expect("chat_session_get through the tools/call envelope");

    // `tools/call` answers the MCP content block, with the tool's own JSON
    // rendered as text — the shape any MCP client renders.
    let text = read_back["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a tools/call result carries a text block: {read_back}"));
    let session: serde_json::Value =
        serde_json::from_str(text).unwrap_or_else(|e| panic!("session JSON in {text:?}: {e}"));
    let history = session["history"]
        .as_array()
        .unwrap_or_else(|| panic!("the session carries a history array: {session}"));
    assert_eq!(
        history
            .iter()
            .map(|m| (m["role"].as_str(), m["content"].as_str()))
            .collect::<Vec<_>>(),
        vec![
            (Some("user"), Some("what do you remember about me?")),
            (Some("assistant"), Some("You live in Hastings-on-Hudson.")),
        ],
        "the turn persisted through a SINGLE envelope: {session}"
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

/// Why: `serve_stdio_bridge::STREAMING_METHODS` is a SECOND copy of the
/// daemon's `transport::uds::STREAM_METHODS`, and it has to be — the bridge
/// refuses a streaming method before it dials, so it cannot ask the router what
/// was registered. The two drift silently in the dangerous direction: a method
/// the daemon streams and the bridge does not know is one the bridge forwards
/// as an ordinary call, leaving an MCP client waiting for a single response
/// frame that never comes. #6286 added `memory.activity_stream` to the daemon
/// and not to the bridge, which is that exact case.
///
/// What: compares the two lists as sets. The bridge is not required to keep the
/// daemon's declaration order — only to know the same names.
/// Test: this is the test.
#[test]
fn bridge_streaming_methods_match_the_daemon() {
    let mut daemon = trusty_memory::transport::uds::STREAM_METHODS.to_vec();
    let mut bridge = trusty_memory::commands::serve_stdio_bridge::STREAMING_METHODS.to_vec();
    daemon.sort_unstable();
    bridge.sort_unstable();
    assert_eq!(
        bridge, daemon,
        "the bridge must refuse exactly the methods the daemon streams — a name \
         only the daemon knows is one the bridge forwards as a unary call, and \
         the client waits forever"
    );
}

/// Points `resolve_data_dir` at a temp root, and clears the override on `Drop`.
///
/// Why (#6555): `probe_daemon_http` takes no socket path — it derives one
/// through `trusty_common::daemon_socket_path`, the entry point the daemon
/// binds through. The [`Daemon`] harness above binds an arbitrary tempdir
/// socket, which `tctl` therefore cannot find, so the tctl arm below binds
/// where `tctl` actually looks. `Drop` rather than cleanup at the end of the
/// body, because a panicking assertion would otherwise strand the override
/// pointing at a deleted directory for the next `#[serial]` sibling.
/// What: sets and removes `TRUSTY_DATA_DIR_OVERRIDE`.
/// Test: `tctl_probe_sees_a_live_uds_daemon_as_serving`.
struct DataDirGuard;

impl DataDirGuard {
    fn point_at(root: &std::path::Path) -> Self {
        // SAFETY: process-global, and the only caller is `#[serial]`.
        unsafe { std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, root) };
        Self
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        // SAFETY: process-global, and the only caller is `#[serial]`.
        unsafe { std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV) };
    }
}

/// REGRESSION (#6555): `tctl`'s own probe must read a live daemon as `Serving`.
///
/// Why: every other test here hands the daemon `json!({})` by hand, so none of
/// them builds the frame `tctl` sends. `probe_socket` sent no `params` at all,
/// which decodes to `Value::Null`, which `memory.health`'s `HealthQuery`
/// refuses with `-32602`. `tctl status` then rendered a healthy daemon as
/// `down`, exited 2, and failed `tctl install`'s verify tail. This is the arm
/// that fails on that frame, mirroring trusty-analyze's Consumer 2.
/// What: binds the real router on the derived path and calls `tctl`'s public
/// entry point, which builds its own request end to end.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn tctl_probe_sees_a_live_uds_daemon_as_serving() {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();

    let data = tempfile::tempdir().expect("tempdir");
    let root = data.path().to_path_buf();
    std::mem::forget(data);
    let _data_dir = DataDirGuard::point_at(&root);
    // #88: bypass the project-slug gate, as `Daemon::start` does.
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }

    let state = AppState::new(root);
    // #911: flip past the warming preflight so the health handler runs.
    state.set_ready();

    // The daemon binds, and `tctl` resolves, the SAME path from the SAME entry
    // point — which is what makes this a consumer contract rather than a wire
    // format test.
    let socket = trusty_common::daemon_socket_path("trusty-memory").expect("derive socket path");

    let (stop, shutdown) = oneshot::channel::<()>();
    let serve_socket = socket.clone();
    let serving = tokio::spawn(async move {
        trusty_memory::transport::uds::serve_with_shutdown(state, &serve_socket, async {
            let _ = shutdown.await;
        })
        .await
    });

    let mut up = false;
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(50)).await {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(up, "nothing came up on {}", socket.display());

    let outcome = probe_daemon_http("trusty-memory", "trusty-memory").await;
    assert!(
        matches!(outcome, ProbeOutcome::Serving { .. }),
        "got {outcome:?}"
    );

    let _ = stop.send(());
    let _ = serving.await;
}
