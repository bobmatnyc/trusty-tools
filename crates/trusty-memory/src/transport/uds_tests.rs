//! The daemon's wire behaviour, over a real socket (#6286).
//!
//! Why a real socket rather than `build_router(…).dispatch(frame)` alone: the
//! router answers the question "what does this method return", and the socket
//! answers "does a client that dials this path get it" — including the bind,
//! the peer check, the frame budget, the concurrency and the unlink. The web
//! tests this file replaces drove an in-process axum router and could not ask
//! the second question at all.
//!
//! What is deliberately gone with the HTTP surface, and not replaced: the
//! same-origin write guard (nothing to guard on a 0600 socket in a 0700
//! directory), the `/sse` broadcast, the embedded-asset fallback route, the
//! `…/memories` path alias, and every assertion of the form "is route X
//! registered" — a listener with no paths cannot be asked.
//!
//! Test naming: `rpc_*` for the wire, `stream_*` for `memory.chat`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::oneshot;
use trusty_common::uds::server::{RpcResponse, CODE_METHOD_NOT_FOUND};
use trusty_common::uds::{send_framed_request_capped, send_framed_stream_request_capped};

use super::{
    build_router, dispatcher_method_count, serve_with_shutdown, socket_path, FOLDED_METHODS,
    MAX_FRAME_BYTES, METHOD_HEALTH, STREAM_METHODS,
};
use crate::AppState;

/// How long a test waits on one call. Generous for a loaded machine; a local
/// socket answers in microseconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// Build a fresh `AppState` rooted in an ephemeral tempdir.
///
/// The tempdir is leaked so the directory outlives the borrow without the test
/// holding it; tests are short and the process reaps it.
fn test_state() -> AppState {
    // Seed the process-wide `retrieval::shared_embedder()` cell with the mock.
    // Under `cargo nextest run` each test gets a virgin cell and would
    // otherwise reach for the real ONNX model and fail on the HuggingFace
    // download — the #4413 defect class. Idempotent, so calling it from the one
    // fixture every test uses is free and order-independent.
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    // #88: bypass the project-slug enforcement gate so a test can create a
    // palace without a real project root on disk.
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root);
    // #911: flip past the warming preflight so handlers run.
    state.set_ready();
    state
}

/// One request frame for `method`.
fn frame(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// A running daemon on a temp socket, and the handle that stops it.
struct Daemon {
    socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    joined: Option<tokio::task::JoinHandle<()>>,
}

impl Daemon {
    /// Bind a temp socket, serve `state` on it, and wait until it answers.
    async fn start(state: AppState) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("sockets").join("trusty-memory.sock");
        std::mem::forget(tmp);
        let (stop, shutdown) = oneshot::channel::<()>();
        let serve_socket = socket.clone();
        let joined = tokio::spawn(async move {
            let _ = serve_with_shutdown(state, &serve_socket, async {
                let _ = shutdown.await;
            })
            .await;
        });

        // Poll rather than sleep a fixed interval: the bind is fast but a
        // loaded machine is not, and a fixed wait is either flaky or slow.
        for _ in 0..200 {
            if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(200)).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        Self {
            socket,
            stop: Some(stop),
            joined: Some(joined),
        }
    }

    fn socket(&self) -> &Path {
        &self.socket
    }

    /// Dial, send one frame, read one back.
    async fn call(&self, method: &str, params: Value) -> RpcResponse {
        send_framed_request_capped(
            &self.socket,
            &frame(1, method, params),
            CALL_TIMEOUT,
            MAX_FRAME_BYTES,
        )
        .await
        .unwrap_or_else(|e| panic!("call {method}: {e}"))
    }

    /// Call and unwrap the success half, naming the error if there is one.
    async fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.call(method, params).await;
        match (response.result, response.error) {
            (Some(result), _) => result,
            (None, Some(e)) => panic!("{method} failed: {} ({})", e.message, e.code),
            (None, None) => panic!("{method} answered neither result nor error"),
        }
    }

    /// Stop the daemon and wait for its task to finish.
    async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(joined) = self.joined.take() {
            let _ = tokio::time::timeout(Duration::from_secs(10), joined).await;
        }
    }
}

// ---------------------------------------------------------------------------
// The method table
// ---------------------------------------------------------------------------

/// Why: `FOLDED_METHODS` is what a consumer outside this crate reads to learn
/// the surface, and a name that is documented but not registered fails at the
/// consumer rather than here. The reverse — registered but undocumented —
/// leaves a method nothing can discover.
/// Test: itself.
#[tokio::test]
async fn rpc_router_registers_every_documented_method() {
    let router = build_router(test_state());
    let registered: Vec<&str> = router.method_names().collect();
    let mut documented = FOLDED_METHODS.to_vec();
    documented.sort_unstable();
    assert_eq!(
        registered, documented,
        "FOLDED_METHODS must equal what build_router registers"
    );
    // Sorted for the same reason `documented` is: the router keeps its names in
    // a `BTreeMap`, so it reports them in name order rather than registration
    // order, and pinning the constant's declaration order would only make the
    // next addition fail on where it was written.
    let streams: Vec<&str> = router.stream_names().collect();
    let mut documented_streams = STREAM_METHODS.to_vec();
    documented_streams.sort_unstable();
    assert_eq!(
        streams, documented_streams,
        "STREAM_METHODS must equal what build_router registers"
    );
}

/// Why: a folded name that collided with a dispatcher name would shadow it —
/// registered methods win over the fallback — and the shadowing would be
/// silent. The `memory.` prefix keeps the two sets disjoint by construction;
/// this is what stops a later addition breaking that by accident.
/// Test: itself.
#[tokio::test]
async fn rpc_folded_names_do_not_collide_with_dispatcher_names() {
    let dispatcher = crate::transport::rpc::method_names();
    for folded in FOLDED_METHODS.iter().chain(STREAM_METHODS) {
        assert!(
            !dispatcher.contains(folded),
            "{folded} is registered AND routed by the dispatcher; the folded \
             registration would silently shadow it"
        );
    }
}

/// Why: nothing else reports how large the fallback's half of the surface is —
/// `RpcRouter::method_names` cannot see it, which is the point of the seam. A
/// count that silently fell to zero would mean the dispatcher was mounted and
/// empty, and every tool call would answer `method_not_found` at runtime.
/// Test: itself.
#[test]
fn rpc_reports_the_dispatcher_surface_size() {
    assert!(
        dispatcher_method_count() >= 30,
        "the dispatcher routes the whole tool surface; got {}",
        dispatcher_method_count()
    );
}

/// Why: the dispatcher's own names must be reachable THROUGH the fallback, not
/// merely present in a list. `palace_list` against a fresh state is the
/// cheapest arm that proves a real call landed — it returns an empty array
/// rather than an error.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_dispatcher_method_answers_through_the_fallback() {
    let daemon = Daemon::start(test_state()).await;
    let result = daemon.ok("palace_list", json!({})).await;
    assert!(
        result["palaces"]
            .as_array()
            .expect("palace_list returns {palaces: []}")
            .is_empty(),
        "a fresh state lists zero palaces, got {result}"
    );
    daemon.shutdown().await;
}

/// Why: a name neither half serves must be refused with the dispatcher's own
/// code rather than the router's, because the fallback is consulted first and
/// its refusal is what crosses the wire.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_reports_method_not_found_for_an_unknown_method() {
    let daemon = Daemon::start(test_state()).await;
    let response = daemon.call("definitely_not_a_method", json!({})).await;
    let error = response.error.expect("an unknown method must be refused");
    assert_eq!(error.code, CODE_METHOD_NOT_FOUND);
    assert!(
        error.message.contains("definitely_not_a_method"),
        "the refusal must name what was asked for: {}",
        error.message
    );
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// Folded methods
// ---------------------------------------------------------------------------

/// Why: `memory.status` takes no arguments, so a caller legitimately sends no
/// `params` at all — which arrives as `null`. A plain unit struct refuses
/// `null`, which is why `NoParams` exists; without it every console poll would
/// answer `invalid_params`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_status_answers_with_no_params() {
    let daemon = Daemon::start(test_state()).await;
    let bare = send_framed_request_capped::<_, RpcResponse>(
        daemon.socket(),
        &json!({"jsonrpc": "2.0", "id": 1, "method": "memory.status"}),
        CALL_TIMEOUT,
        MAX_FRAME_BYTES,
    )
    .await
    .expect("status with absent params");
    let result = bare.result.expect("status must answer");
    assert!(result["version"].is_string());
    assert_eq!(result["palace_count"], 0);
    daemon.shutdown().await;
}

/// Why: `memory.health` is the method the console connector and `tctl`'s probe
/// dial by literal. It must answer without `?probe=true`'s expensive ONNX
/// round-trip — the cheap path is what a 1-second poll uses (#1101) — and it
/// must report the socket rather than the TCP address it used to (#6286).
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_health_answers_over_a_real_socket() {
    let daemon = Daemon::start(test_state()).await;
    let result = daemon.ok(METHOD_HEALTH, json!({})).await;
    assert_eq!(result["status"], "ok");
    assert!(result["version"].is_string());
    assert!(
        result.get("addr").is_none(),
        "the TCP address field retired with the listener: {result}"
    );
    assert!(
        result["socket"]
            .as_str()
            .is_some_and(|s| s.ends_with(".sock")),
        "health must name the socket it serves: {result}"
    );
    daemon.shutdown().await;
}

/// Why: `memory.config` is what the settings panel pre-fills from, and the one
/// thing it must never do is echo the OpenRouter key.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_config_reports_whether_a_key_is_set_without_echoing_it() {
    let daemon = Daemon::start(test_state()).await;
    let result = daemon.ok("memory.config", json!({})).await;
    assert!(result["openrouter_configured"].is_boolean());
    assert!(result["model"].is_string());
    assert!(
        result.get("openrouter_api_key").is_none(),
        "the key must never cross the wire: {result}"
    );
    daemon.shutdown().await;
}

/// Why: the drawer trio is the folded surface's only write path, and a create
/// that did not round-trip through list would leave the migration untested at
/// exactly the point it matters. This also covers the params reshape: the
/// palace id used to be a path segment and is now a field.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_drawer_create_list_and_delete_round_trip() {
    let state = test_state();
    let palace = seed_palace(&state, "alpha");
    let daemon = Daemon::start(state).await;

    let created = daemon
        .ok(
            "memory.drawer_create",
            json!({"palace_id": palace, "content": "a folded drawer for the round trip"}),
        )
        .await;
    let drawer_id = created["id"]
        .as_str()
        .expect("create returns an id")
        .to_string();

    let listed = daemon
        .ok("memory.drawers_list", json!({"palace_id": palace}))
        .await;
    assert!(
        listed.to_string().contains(&drawer_id),
        "the created drawer must be listed: {listed}"
    );

    let deleted = daemon
        .ok(
            "memory.drawer_delete",
            json!({"palace_id": palace, "drawer_id": drawer_id}),
        )
        .await;
    assert_eq!(
        deleted["deleted"], true,
        "delete answers a body, not the former 204: {deleted}"
    );
    daemon.shutdown().await;
}

/// Why: attribution used to arrive in `X-Trusty-Client-*` headers, and a
/// JSON-RPC frame has no header channel. If the fields did not move into
/// `params`, every write from the console would be attributed to the daemon's
/// own environment — the exact thing DOC-53 §4.3 forbids.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_drawer_create_attributes_the_caller_it_was_given() {
    let state = test_state();
    let palace = seed_palace(&state, "attributed");
    let daemon = Daemon::start(state).await;

    daemon
        .ok(
            "memory.drawer_create",
            json!({
                "palace_id": palace,
                "content": "attributed to a caller that named itself",
                "client": "trusty-console",
                "workstream": "feat-6286",
            }),
        )
        .await;

    let listed = daemon
        .ok("memory.drawers_list", json!({"palace_id": palace}))
        .await
        .to_string();
    assert!(
        listed.contains("feat-6286"),
        "the caller's workstream must reach the drawer's tags: {listed}"
    );
    daemon.shutdown().await;
}

/// Why: #5231 — deleting a drawer id nobody has must be a refusal a caller can
/// act on, not a silent success. `-32004` is that refusal on this wire.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_drawer_delete_reports_not_found_for_an_unknown_id() {
    let state = test_state();
    let palace = seed_palace(&state, "missing-drawer");
    let daemon = Daemon::start(state).await;
    let response = daemon
        .call(
            "memory.drawer_delete",
            json!({"palace_id": palace, "drawer_id": uuid::Uuid::new_v4().to_string()}),
        )
        .await;
    let error = response.error.expect("an absent drawer must be refused");
    assert_eq!(error.code, crate::transport::CODE_NOT_FOUND);
    daemon.shutdown().await;
}

/// Why: `memory.palace_get` names a palace by id, and #5549 (ADR-0045) turns on
/// telling a genuine absence apart from an open that failed. An id nobody has
/// is the absence half.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_palace_get_reports_not_found_for_an_unknown_id() {
    let daemon = Daemon::start(test_state()).await;
    let response = daemon
        .call("memory.palace_get", json!({"palace_id": "no-such-palace"}))
        .await;
    let error = response.error.expect("an absent palace must be refused");
    assert_eq!(error.code, crate::transport::CODE_NOT_FOUND);
    daemon.shutdown().await;
}

/// Why: `memory.palaces_list` is what replaces the monitor's N-call
/// `memory.palace_get` fan-out, so it has to answer one row per palace WITH
/// real counts — not the placeholder zeros the retired `GET /api/v1/palaces`
/// reported for any palace that was not already resident (#4640). A row whose
/// counts were peeked rather than measured is the defect this method exists to
/// close.
/// What: seeds two palaces, writes a drawer into one through the folded create,
/// and asserts both appear with the write reflected in the counts.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_palaces_list_reports_counts_per_palace() {
    let state = test_state();
    let written = seed_palace(&state, "roster-one");
    let empty = seed_palace(&state, "roster-two");
    let daemon = Daemon::start(state).await;

    daemon
        .ok(
            "memory.drawer_create",
            json!({
                "palace_id": written,
                "content": "a drawer the roster has to count",
                "force": true,
            }),
        )
        .await;

    let result = daemon.ok("memory.palaces_list", json!({})).await;
    let rows = result["palaces"]
        .as_array()
        .expect("palaces_list answers a palaces array");
    assert_eq!(rows.len(), 2, "one row per palace: {result}");

    let row = rows
        .iter()
        .find(|r| r["id"] == written.as_str())
        .expect("the written palace is listed");
    assert!(row["error"].is_null(), "a readable palace carries no error");
    assert_eq!(
        row["palace"]["drawer_count"], 1,
        "the count must be a measurement, not a peeked zero: {row}"
    );

    let row = rows
        .iter()
        .find(|r| r["id"] == empty.as_str())
        .expect("the empty palace is listed");
    assert!(row["error"].is_null());
    assert_eq!(row["palace"]["drawer_count"], 0);
}

/// Why: the fan-out this replaces dropped a palace whose call failed at
/// `debug!`, so the panel could report a palace COUNT above fewer rows than
/// that with nothing saying which were missing. The row type carries an error
/// field so that cannot recur, and this is what proves the field is reachable
/// rather than decorative.
/// What: seeds a palace, then makes its data directory unopenable by replacing
/// it with a regular file, and asserts the palace still appears — with `palace`
/// absent and `error` set.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_palaces_list_reports_an_unreadable_palace_rather_than_dropping_it() {
    let state = test_state();
    let good = seed_palace(&state, "roster-good");

    // Created through a SEPARATE registry so `state.registry` never caches its
    // handle: `create_palace` registers the handle it just opened, and a
    // resident handle is returned without touching disk, so a palace seeded the
    // usual way cannot be made to fail an open at all.
    let broken = seed_palace_off_registry(&state, "roster-broken");

    // The registry lists the palace from its `palace.json`, then reads
    // `identity.txt` on the way to opening its stores. Leaving the metadata
    // intact and putting a DIRECTORY where that file belongs makes the read
    // fail with `EISDIR` — the palace is listed and cannot be opened, which is
    // the shape a permissions problem or a jammed redb lock produces, without
    // needing either.
    let dir = state.data_root.join(&broken);
    std::fs::create_dir_all(dir.join("identity.txt")).expect("wedge the palace identity");

    let daemon = Daemon::start(state).await;
    let result = daemon.ok("memory.palaces_list", json!({})).await;
    let rows = result["palaces"]
        .as_array()
        .expect("palaces_list answers a palaces array");
    assert_eq!(
        rows.len(),
        2,
        "an unreadable palace must still be a row: {result}"
    );

    let row = rows
        .iter()
        .find(|r| r["id"] == broken.as_str())
        .expect("the wedged palace is listed");
    assert!(
        row["error"].is_string(),
        "the row must carry why it could not be read: {row}"
    );
    assert!(
        row["palace"].is_null(),
        "a failed read must not become a row of zeros: {row}"
    );

    let row = rows
        .iter()
        .find(|r| r["id"] == good.as_str())
        .expect("the healthy palace is unaffected");
    assert!(row["error"].is_null());
}

/// Why: the clamp is what stops a caller pulling a whole graph by asking for
/// one page, and it used to live in the axum extractor layer that is gone. A
/// clamp that did not survive the move would be invisible until a palace was
/// large enough to matter.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_kg_all_clamps_an_absurd_limit() {
    let state = test_state();
    let palace = seed_palace(&state, "kg-clamp");
    let daemon = Daemon::start(state).await;
    // The assertion is that the call is ANSWERED rather than refused or
    // unbounded: a fresh palace has no triples, so the page is empty either
    // way, and what is under test is that `usize::MAX` does not reach the
    // store.
    let result = daemon
        .ok(
            "memory.kg_all",
            json!({"palace_id": palace, "limit": 1_000_000_000_u64}),
        )
        .await;
    assert!(result.is_array(), "kg_all answers an array: {result}");
    daemon.shutdown().await;
}

/// Why: `direction` is the one KG param with a closed vocabulary, and a value
/// outside it used to be a 400. Silently defaulting to `both` would answer a
/// question the caller did not ask.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_kg_graph_neighbors_refuses_a_bad_direction() {
    let state = test_state();
    let palace = seed_palace(&state, "kg-direction");
    let daemon = Daemon::start(state).await;
    let response = daemon
        .call(
            "memory.kg_graph_neighbors",
            json!({"palace_id": palace, "node": "n", "direction": "sideways"}),
        )
        .await;
    let error = response.error.expect("a bad direction must be refused");
    assert_eq!(error.code, trusty_common::uds::server::CODE_INVALID_PARAMS);
    assert!(
        error.message.contains("sideways"),
        "the refusal must name what was sent: {}",
        error.message
    );
    daemon.shutdown().await;
}

/// Why: #466 — content the background worker would drop must be refused
/// synchronously, or the caller is told the memory was stored when it was not.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_remember_async_rejects_short_content() {
    let daemon = Daemon::start(test_state()).await;
    let response = daemon
        .call("memory.remember_async", json!({"content": "too short"}))
        .await;
    let error = response.error.expect("short content must be refused");
    assert_eq!(error.code, trusty_common::uds::server::CODE_INVALID_PARAMS);
    assert!(
        error.message.contains("too short"),
        "the refusal must say why: {}",
        error.message
    );
    daemon.shutdown().await;
}

/// Why: the contract is one-way — the method answers `queued` before the write
/// has happened — so the only thing that proves the queue is real is observing
/// the drawer afterwards. A method that validated its input and then dropped it
/// would pass every other assertion in this file.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_remember_async_queues_and_persists() {
    let state = test_state();
    let palace = seed_palace(&state, "queued");
    let daemon = Daemon::start(state).await;

    let queued = daemon
        .ok(
            "memory.remember_async",
            json!({
                "palace": palace,
                "content": "the fire and forget path really does persist this",
            }),
        )
        .await;
    assert_eq!(queued["status"], "queued");

    // The write runs on a detached task, so poll rather than assert once. The
    // budget is generous because the dispatch goes through the embedder.
    let mut listed = String::new();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        listed = daemon
            .ok("memory.drawers_list", json!({"palace_id": palace}))
            .await
            .to_string();
        if listed.contains("really does persist") {
            break;
        }
    }
    assert!(
        listed.contains("really does persist"),
        "the queued write must reach the palace: {listed}"
    );
    daemon.shutdown().await;
}

/// Why: the tail is what an operator reads instead of SSHing to the box, and
/// the clamp is what stops a request asking for more lines than the ring holds.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_logs_tail_answers_a_bounded_page() {
    let daemon = Daemon::start(test_state()).await;
    let result = daemon.ok("memory.logs_tail", json!({"n": 5})).await;
    assert!(
        result["lines"].is_array(),
        "logs_tail returns lines: {result}"
    );
    assert!(result["total"].is_number());
    daemon.shutdown().await;
}

/// Why: the activity page seeds the console feed on mount, and its filters are
/// caller-supplied. An unreadable `since` must be refused rather than dropped,
/// because a dropped filter returns a correct-looking page filtered by
/// something other than what was asked for.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_activity_refuses_an_unparseable_since() {
    let daemon = Daemon::start(test_state()).await;
    let ok = daemon.ok("memory.activity", json!({})).await;
    assert!(ok["entries"].is_array(), "activity returns entries: {ok}");

    let response = daemon
        .call("memory.activity", json!({"since": "not-a-timestamp"}))
        .await;
    let error = response.error.expect("a bad timestamp must be refused");
    assert_eq!(error.code, trusty_common::uds::server::CODE_INVALID_PARAMS);
    daemon.shutdown().await;
}

/// Why: the dream trio was three separate routes and is now three methods; the
/// aggregate read is the one the console dashboard polls.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_dream_status_answers_an_aggregate() {
    let daemon = Daemon::start(test_state()).await;
    let result = daemon.ok("memory.dream_status", json!({})).await;
    assert!(
        result.is_object(),
        "dream_status returns an object: {result}"
    );
    daemon.shutdown().await;
}

/// Why: the provider probe is what tells the chat panel whether anything is
/// reachable, and it must answer both entries whether or not either upstream is
/// up — a probe that errored when Ollama was absent would read as a broken
/// daemon.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_chat_providers_answers_both_upstreams() {
    let daemon = Daemon::start(test_state()).await;
    let result = daemon.ok("memory.chat_providers", json!({})).await;
    let names: Vec<&str> = result["providers"]
        .as_array()
        .expect("providers is an array")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(names, vec!["ollama", "openrouter"]);
    daemon.shutdown().await;
}

/// Why: the three message endpoints are the SessionStart hook's whole
/// interface, and `mark_read`'s idempotence is what stops two concurrent
/// sessions double-delivering. A send that did not become a listable message
/// would break the hook silently.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_messages_send_list_and_mark_read_round_trip() {
    let state = test_state();
    let inbox = seed_palace(&state, "inbox");
    let daemon = Daemon::start(state).await;

    let sent = daemon
        .ok(
            "memory.message_send",
            json!({
                "to_palace": inbox,
                "purpose": "handoff",
                "content": "the folded message path still delivers",
                "from_palace": "sender",
            }),
        )
        .await;
    assert_eq!(sent["status"], "sent");
    let drawer_id = sent["drawer_id"].as_str().expect("a drawer id").to_string();

    let listed = daemon
        .ok(
            "memory.messages_list",
            json!({"palace": inbox, "unread_only": true}),
        )
        .await;
    assert_eq!(
        listed.as_array().expect("an array").len(),
        1,
        "the sent message must be unread in the inbox: {listed}"
    );

    let first = daemon
        .ok(
            "memory.message_mark_read",
            json!({"palace": inbox, "drawer_id": drawer_id.clone()}),
        )
        .await;
    assert_eq!(first["flipped"], true, "the first ack flips the flag");

    let second = daemon
        .ok(
            "memory.message_mark_read",
            json!({"palace": inbox, "drawer_id": drawer_id}),
        )
        .await;
    assert_eq!(
        second["flipped"], false,
        "a second ack is a success that flipped nothing, not an error"
    );
    daemon.shutdown().await;
}

/// Why (DOC-53 §4.3, the code-critic BLOCK round): the bug this exists to catch
/// is daemon-vs-caller mis-attribution. ONE shared daemon serves every
/// concurrently-attached session, so it must stamp each write with the identity
/// THAT REQUEST carried — never a value derived from the daemon process, which
/// would be identical for every caller and silently collapse two sessions'
/// attribution into one.
///
/// An in-process `dispatch_tool` call cannot show this: it never crosses the
/// shared surface a bridge-mediated caller uses. This used to drive `POST /rpc`
/// for that reason; #6286 makes the socket that surface, so it dials twice with
/// two `tools/call` envelopes carrying different `arguments.workstream` — the
/// exact shape the stdio bridge forwards.
///
/// `force: true` bypasses the rolling near-duplicate gate (#220): without it the
/// two writes, differing only in a marker word, would be similar enough for the
/// SECOND to be skipped — which would make this pass for the wrong reason.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn mcp_writes_carry_distinct_ws_tags_per_caller_over_rpc() {
    let state = test_state();
    let palace = seed_palace(&state, "cred-multi-caller");
    let daemon = Daemon::start(state).await;

    for (workstream, marker) in [("ws-alpha", "alpha"), ("ws-beta", "beta")] {
        let answered = daemon
            .ok(
                "tools/call",
                json!({
                    "name": "memory_remember",
                    "arguments": {
                        "palace": palace,
                        "text": format!(
                            "distinct-caller regression content marker {marker} with enough tokens"
                        ),
                        "room": "General",
                        "workstream": workstream,
                        "force": true,
                    }
                }),
            )
            .await;
        // A skipped write would leave one drawer and fail the count below with a
        // misleading message, so the envelope is checked where it is produced.
        let text = answered["content"][0]["text"].as_str().unwrap_or_default();
        let inner: Value = serde_json::from_str(text).unwrap_or_default();
        assert_eq!(
            inner["status"], "stored",
            "expected a stored (not skipped) envelope for {workstream}; got {inner:?}"
        );
    }

    let listed = daemon
        .ok(
            "memory.drawers_list",
            json!({ "palace_id": palace, "limit": 10 }),
        )
        .await;
    let drawers = listed.as_array().expect("drawers array");
    assert_eq!(drawers.len(), 2, "expected two drawers, got {drawers:?}");

    for drawer in drawers {
        let content = drawer["content"].as_str().unwrap_or_default();
        let tags: Vec<&str> = drawer["tags"]
            .as_array()
            .expect("tags array")
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        let (own, other) = if content.contains("marker alpha") {
            ("ws-alpha", "ws-beta")
        } else if content.contains("marker beta") {
            ("ws-beta", "ws-alpha")
        } else {
            panic!("drawer content did not match either marker: {content:?}");
        };
        assert!(
            tags.contains(&format!("ws:{own}").as_str())
                && tags.contains(&format!("creator:workstream={own}").as_str()),
            "the {own} drawer must carry its own ws tags; got {tags:?}"
        );
        assert!(
            !tags.contains(&format!("ws:{other}").as_str())
                && !tags.contains(&format!("creator:workstream={other}").as_str()),
            "the {own} drawer must NOT leak the {other} caller's ws tags \
             (daemon-vs-caller mis-attribution); got {tags:?}"
        );
    }
    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Why: `memory.chat` is registered with `typed_stream`, and a caller that
/// forgets `"stream": true` must be told so in the one frame it is reading
/// rather than left waiting for a shape it cannot parse. This is the half of
/// the streaming contract that is testable without an LLM.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_chat_refuses_a_unary_call_naming_the_stream_requirement() {
    let daemon = Daemon::start(test_state()).await;
    let response = daemon
        .call("memory.chat", json!({"message": "hello"}))
        .await;
    let error = response
        .error
        .expect("a unary call to a streaming method must be refused");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_STREAM_REQUIRED,
        "the refusal must be the stream-required code, got {}",
        error.message
    );
    daemon.shutdown().await;
}

/// Why: this is the Fail-Open branch the whole streaming extension exists to
/// close. `memory.chat` opens no stream when no provider is configured — which
/// is the state of a test machine — and that failure must reach the client as
/// the terminal ERROR frame. The SSE version it replaces wrote
/// `data: {"error": …}` and then ended normally, which a reader could not tell
/// from a completed answer.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_chat_reports_a_provider_failure_as_the_terminal_error_frame() {
    let daemon = Daemon::start(test_state()).await;
    let mut stream = send_framed_stream_request_capped::<_, Value>(
        daemon.socket(),
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory.chat",
            "stream": true,
            "params": {"message": "hello"},
        }),
        CALL_TIMEOUT,
        MAX_FRAME_BYTES,
    )
    .await
    .expect("the stream must open even when the handler will fail");

    let first = stream
        .next_frame()
        .await
        .expect("a failed open is a terminal frame, never an empty end");
    let error = first.expect_err("no provider is configured on a test machine");
    assert!(
        matches!(error, trusty_common::uds::UdsRpcError::Stream { .. }),
        "expected the server's terminal error frame, got {error:?}"
    );
    assert!(
        stream.next_frame().await.is_none(),
        "the failure is reported once, then the stream is done"
    );
    daemon.shutdown().await;
}

/// Why (#6286): `memory.activity_stream` is what `/sse` was, and the whole
/// point is that a reader learns of an event WITHOUT asking. A test that polled
/// would pass against the 2-second `memory.activity` tick this replaces and
/// prove nothing.
/// What: opens the stream, then triggers an event through a separate call, and
/// asserts the frame arrives on the already-open stream with the same
/// `type`-tagged body the SSE `data:` lines carried.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_activity_stream_delivers_an_event_without_polling() {
    let state = test_state();
    let palace = seed_palace(&state, "stream-palace");
    let daemon = Daemon::start(state).await;

    let mut stream = send_framed_stream_request_capped::<_, Value>(
        daemon.socket(),
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory.activity_stream",
            "stream": true,
        }),
        CALL_TIMEOUT,
        MAX_FRAME_BYTES,
    )
    .await
    .expect("the stream opens");

    // A second connection, so the event is genuinely pushed onto the first
    // rather than being a reply to anything it sent.
    daemon
        .ok(
            "memory.drawer_create",
            json!({
                "palace_id": palace,
                "content": "an event the stream has to carry",
                "force": true,
            }),
        )
        .await;

    let frame = tokio::time::timeout(CALL_TIMEOUT, stream.next_frame())
        .await
        .expect("an emitted event must reach an open stream promptly")
        .expect("the stream carries the event rather than ending")
        .expect("the frame is an item, not a terminal error");
    assert_eq!(
        frame["type"], "drawer_added",
        "the frame body is the `type`-tagged DaemonEvent: {frame}"
    );
    assert_eq!(frame["palace_id"], palace.as_str());

    daemon.shutdown().await;
}

/// Why (#6286): `/sse` began at the subscription and this does too, which is a
/// contract a reader has to be able to rely on. A stream that replayed history
/// would make the monitor's log show the whole activity table every time the
/// TUI opened; one that a reader WRONGLY assumes replays shows an empty pane
/// and reads as an idle daemon. Either way the answer has to be pinned.
/// What: emits an event BEFORE opening the stream, then opens it and asserts
/// nothing arrives — while a later event does.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_activity_stream_does_not_replay_history() {
    let state = test_state();
    let palace = seed_palace(&state, "replay-palace");
    let daemon = Daemon::start(state).await;

    daemon
        .ok(
            "memory.drawer_create",
            json!({
                "palace_id": palace,
                "content": "an event from before the stream opened",
                "force": true,
            }),
        )
        .await;

    let mut stream = send_framed_stream_request_capped::<_, Value>(
        daemon.socket(),
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory.activity_stream",
            "stream": true,
        }),
        CALL_TIMEOUT,
        MAX_FRAME_BYTES,
    )
    .await
    .expect("the stream opens");

    // Nothing to read: the prior event is history, and history is what
    // `memory.activity` is for.
    let quiet = tokio::time::timeout(Duration::from_millis(300), stream.next_frame()).await;
    assert!(
        quiet.is_err(),
        "a freshly opened stream must not replay: {quiet:?}"
    );

    // And the stream is live rather than merely silent.
    daemon
        .ok(
            "memory.drawer_create",
            json!({
                "palace_id": palace,
                "content": "an event from after the stream opened",
                "force": true,
            }),
        )
        .await;
    let frame = tokio::time::timeout(CALL_TIMEOUT, stream.next_frame())
        .await
        .expect("a later event still arrives")
        .expect("the stream is open")
        .expect("the frame is an item");
    assert_eq!(frame["type"], "drawer_added");

    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// The socket itself
// ---------------------------------------------------------------------------

/// Why: trusty-memory is the multi-client case ADR-0031/ADR-0032 cite as the
/// reason a resident daemon exists — many stdio bridges forward through one
/// process. An accept loop that served inline would be read as dead under
/// exactly the load it was handling, because a saturated accept queue answers
/// ECONNREFUSED on macOS.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_serves_concurrent_connections() {
    const CLIENTS: usize = 24;
    let daemon = Daemon::start(test_state()).await;
    let socket = daemon.socket().to_path_buf();

    let calls = (0..CLIENTS).map(|i| {
        let socket = socket.clone();
        tokio::spawn(async move {
            send_framed_request_capped::<_, RpcResponse>(
                &socket,
                &frame(i as i64, "memory.status", json!({})),
                CALL_TIMEOUT,
                MAX_FRAME_BYTES,
            )
            .await
        })
    });

    let answers = futures::future::join_all(calls).await;
    for (i, answer) in answers.into_iter().enumerate() {
        let response = answer
            .unwrap_or_else(|e| panic!("client {i} task panicked: {e}"))
            .unwrap_or_else(|e| panic!("client {i} call failed: {e}"));
        assert!(
            response.result.is_some(),
            "client {i} got an error: {:?}",
            response.error
        );
        assert_eq!(response.id, json!(i as i64), "ids must not cross wires");
    }
    daemon.shutdown().await;
}

/// Why: a frame past the shared 8 MiB default must be ACCEPTED, because this
/// service raises its own budget to 32 MiB and a request that the client sends
/// happily and the server refuses is the failure mode the two matching figures
/// exist to prevent.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_accepts_a_request_larger_than_the_shared_default() {
    let daemon = Daemon::start(test_state()).await;
    // 12 MiB of padding: past `trusty_common::uds::MAX_FRAME_BYTES` (8 MiB),
    // inside this service's 32 MiB. `memory.status` ignores its params, so what
    // is under test is the read, not the handler.
    let padding = "x".repeat(12 * 1024 * 1024);
    let response = send_framed_request_capped::<_, RpcResponse>(
        daemon.socket(),
        &frame(1, "memory.status", json!({"pad": padding})),
        CALL_TIMEOUT,
        MAX_FRAME_BYTES,
    )
    .await
    .expect("a 12 MiB frame is inside this service's budget");
    assert!(response.result.is_some(), "{:?}", response.error);
    daemon.shutdown().await;
}

/// Why: 32 MiB is a bound, not the absence of one. A frame past it must be
/// REFUSED rather than buffered, and the refusal must not take the accept loop
/// down with it — a peer that floods the socket would otherwise be a way to
/// stop the daemon answering anyone.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_refuses_a_request_past_its_own_budget() {
    let daemon = Daemon::start(test_state()).await;

    // One byte past the budget, so what is under test is the boundary rather
    // than a wildly oversized payload.
    let padding = "x".repeat(MAX_FRAME_BYTES as usize);
    let refused = send_framed_request_capped::<_, RpcResponse>(
        daemon.socket(),
        &frame(1, "memory.status", json!({"pad": padding})),
        CALL_TIMEOUT,
        // The client's own budget is raised past the server's so the refusal
        // under test is the SERVER's. With both at 32 MiB the client would
        // refuse to write and the server would never see the frame.
        MAX_FRAME_BYTES * 2,
    )
    .await;
    assert!(
        refused.is_err(),
        "a frame past the server's budget must not be answered"
    );

    // The loop is still serving: this is the half that matters, because a
    // refusal that killed the accept loop would look identical on the first
    // request.
    let after = daemon.ok("memory.status", json!({})).await;
    assert!(
        after["version"].is_string(),
        "the accept loop must survive an oversized frame: {after}"
    );
    daemon.shutdown().await;
}

/// Why: `bind_hardened` and `UnixListener`'s `Drop` both leave the socket file
/// behind, so a server that just returned would leave a path the next start
/// fails to bind. The unlink is what stops that, and a test that deleted the
/// file itself would pass whether or not the unlink existed — which is why this
/// drives the real shutdown path.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_unlinks_its_socket_on_shutdown() {
    let daemon = Daemon::start(test_state()).await;
    let socket = daemon.socket().to_path_buf();
    assert!(socket.exists(), "the socket must exist while serving");
    daemon.shutdown().await;
    assert!(
        !socket.exists(),
        "the socket file must be unlinked on shutdown: {}",
        socket.display()
    );
}

/// Why: the path is derived rather than published, so every consumer computes
/// the same one. A name that drifted would leave each side dialling its own.
/// Test: itself.
#[test]
fn socket_path_is_named_for_this_daemon() {
    let path = socket_path().expect("the data directory must resolve");
    assert!(
        path.ends_with("trusty-memory.sock"),
        "unexpected socket path: {}",
        path.display()
    );
}

/// Why: `remove_retired_discovery_files` resolves the real `$HOME`, so it is
/// never driven from a test. Its removal helper is, against a temp path — an
/// absent file must be tolerated because that is the fresh-install case.
/// Test: itself.
#[test]
fn remove_if_present_deletes_a_stale_file_and_tolerates_an_absent_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stale = tmp.path().join("http_addr");
    std::fs::write(&stale, "127.0.0.1:7070\n").expect("write");
    super::remove_if_present(&stale);
    assert!(!stale.exists(), "a stale discovery file must be removed");
    // Second call: the file is gone and this must not panic or log an error.
    super::remove_if_present(&stale);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a real palace on disk and return its id.
fn seed_palace(state: &AppState, name: &str) -> String {
    use trusty_common::memory_core::palace::{Palace, PalaceId};
    let id = PalaceId::new(name);
    let palace = Palace {
        id: id.clone(),
        name: name.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(name),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create the test palace");
    id.0
}

/// Create a real palace on disk WITHOUT registering it in `state`'s registry.
///
/// Why: `create_palace` registers the handle it opened, and `open_palace`
/// returns a resident handle without touching disk — so a palace seeded the
/// usual way opens no matter what its files look like. A separate registry
/// instance writes the same bytes and leaves `state.registry` cold, which is
/// the only way to exercise an open that fails.
fn seed_palace_off_registry(state: &AppState, name: &str) -> String {
    use trusty_common::memory_core::palace::{Palace, PalaceId};
    use trusty_common::memory_core::PalaceRegistry;
    let id = PalaceId::new(name);
    let palace = Palace {
        id: id.clone(),
        name: name.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(name),
    };
    PalaceRegistry::new()
        .create_palace(&state.data_root, palace)
        .expect("create the test palace off-registry");
    id.0
}
