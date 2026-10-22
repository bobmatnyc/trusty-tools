//! API-driven end-to-end test for DOC-39 §5.6 Slice D's
//! `session.get_readiness` query RPC.
//!
//! Why: `Event::IndexReadiness` (see `tests/task_e2e.rs`'s module docs for the
//! shared #2056 `task.run` background-indexing context) is emitted exactly
//! ONCE, at task start. A previous PR (#2861) shipped that event-only side;
//! nothing let a client that attaches AFTER the probe fired retrieve the
//! state it never streamed. This test drives the REAL `tcode serve` daemon
//! over its Unix socket (never `trusty_code`'s Rust API directly — the same black-box
//! discipline `tests/task_e2e.rs`/`tests/session_e2e.rs` follow) and proves
//! the query surface actually closes that gap, using `TCODE_MOCK_LLM=echo`
//! for deterministic, offline `task.run` execution.
//! What: [`late_attaching_client_can_query_cached_readiness`] runs a task
//! and polls until its background indexing probe lands, then queries
//! `session.get_readiness` on a fresh socket connection that never called
//! `session.attach` — every socket call is its own connection (#6637), so
//! nothing but the daemon's cache links the two — proving a late attacher sees the exact
//! cached state the original probe recorded, not a re-probe or a drifted
//! copy. [`never_probed_session_returns_never_probed_over_the_wire`] proves
//! the OTHER half: a session that has never run a task (so no probe has ever
//! landed) returns the typed `{"status":"never_probed"}` result — never an
//! error, never a bare `null` — over the real wire.
//! Test: this file IS the test; see `support` for the process/protocol
//! plumbing shared with `session_e2e.rs`/`task_e2e.rs`.

use crate::support;

use std::time::Duration;

use serde_json::{Value, json};
use support::{
    SocketDaemon, project_with_agents, spawn_socket_daemon, spawn_socket_daemon_with_env,
};

/// Bounded polling budget for waiting on the detached background-indexing
/// probe to land: `probe_index_readiness` itself has a 1500ms request
/// timeout plus a 750ms connect timeout (see
/// `trusty_common::search_readiness`), so this generously outlasts even a
/// worst-case fail-open probe against an unreachable daemon.
const READINESS_POLL_ROUNDS: usize = 100;
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Poll `session.get_readiness` on `daemon` until it reports
/// `status: "probed"`, returning that `result` value.
///
/// Why: `task.run`'s background indexing probe (`run_task::
/// ensure_project_indexed_in_background`) runs on a detached thread and
/// records asynchronously — there is no guarantee it has landed by the time
/// `task.run` itself returns, so the test must poll rather than assume
/// immediate availability. Bounded so a genuine regression (the probe never
/// lands, or `session.get_readiness` never flips to `"probed"`) fails fast
/// instead of hanging the suite.
async fn poll_until_probed(daemon: &SocketDaemon, session_id: &str) -> Value {
    for round in 0..READINESS_POLL_ROUNDS {
        let resp = daemon
            .call(
                1000 + round as i64,
                "session.get_readiness",
                json!({"session_id": session_id}),
            )
            .await;
        assert!(resp["error"].is_null(), "get_readiness failed: {resp}");
        if resp["result"]["status"] == "probed" {
            return resp["result"].clone();
        }
        tokio::time::sleep(READINESS_POLL_INTERVAL).await;
    }
    panic!("gave up waiting for an index_readiness probe to land for session {session_id}");
}

/// THE point of this slice: a client that attaches to a session AFTER its
/// one-time `Event::IndexReadiness` probe already fired must still be able
/// to retrieve the current readiness state via `session.get_readiness`.
#[tokio::test]
async fn late_attaching_client_can_query_cached_readiness() {
    let project = project_with_agents();
    let daemon = spawn_socket_daemon_with_env(
        project.path(),
        &[(
            trusty_code::task::mock_llm::MOCK_LLM_ENV,
            trusty_code::task::mock_llm::MOCK_LLM_ECHO,
        )],
    )
    .await;

    // 1. Run a task — this is the run
    // whose background indexing probe fires `Event::IndexReadiness` exactly
    // once, and whose readiness this connection never re-reads via
    // `session.attach` (proving the cache — not a live subscription — is
    // what the late client below actually reads).
    let run_resp = daemon
        .call(1, "task.run", json!({"task_description": "say hi"}))
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    // 2. Poll until the probe has landed,
    // capturing exactly what it recorded so the late query below can be
    // compared against it.
    let probed_by_original = poll_until_probed(&daemon, &session_id).await;
    assert_eq!(probed_by_original["status"], "probed");
    assert!(
        probed_by_original["state"].is_string(),
        "a probed snapshot must carry a state: {probed_by_original}"
    );

    // 3. A FRESH connection — one that made none of the above calls and
    // never `session.attach`ed — must retrieve the SAME
    // cached snapshot from a single `session.get_readiness` call.
    let late_resp = daemon
        .call(
            1,
            "session.get_readiness",
            json!({"session_id": session_id}),
        )
        .await;
    assert!(
        late_resp["error"].is_null(),
        "late get_readiness failed: {late_resp}"
    );
    assert_eq!(
        late_resp["result"], probed_by_original,
        "a late-attaching client must see the exact same cached readiness snapshot \
         the original probe recorded, not a re-probe or a drifted copy"
    );

    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// A session with no `task.run` ever executed against it has had no
/// background indexing probe, so `session.get_readiness` must return the
/// typed `never_probed` result — not an error, not a bare `null`.
#[tokio::test]
async fn never_probed_session_returns_never_probed_over_the_wire() {
    let daemon = spawn_socket_daemon().await;

    let create_resp = daemon
        .call(1, "session.create", json!({"task": "never runs a task"}))
        .await;
    assert!(
        create_resp["error"].is_null(),
        "create failed: {create_resp}"
    );
    let session_id = create_resp["result"]["id"]
        .as_str()
        .expect("session.create must return an id")
        .to_string();

    let readiness_resp = daemon
        .call(
            2,
            "session.get_readiness",
            json!({"session_id": session_id}),
        )
        .await;
    assert!(
        readiness_resp["error"].is_null(),
        "get_readiness failed: {readiness_resp}"
    );
    assert_eq!(readiness_resp["result"], json!({"status": "never_probed"}));

    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}
