//! #7619 PR 2: the streamed prune-worktrees pass reports each worktree, beats
//! while quiet, and stops removing once its client is gone.
//!
//! Why: before #7619 the only socket form was unary. It printed nothing until
//! the end and ran to completion after its client died.
//! What: real `git worktree add` orphans driven through
//! [`super::open_prune_stream_in`], directly and over a real socket. A pass is
//! parked on the session store's write lock where a test needs it quiet; the
//! pass reads the store first, so nothing is classified while it is held.
//! Test: this file is the test module.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::server::{
    CODE_INVALID_PARAMS, RpcError, RpcRouter, RpcServeOptions, serve_until,
};

use super::{PRUNE_STREAM_METHOD, open_prune_stream_in};
use crate::core::trusty_tools_config::WorkspaceRootEnv;
use crate::daemon::managed_routes::prune::PruneWorktreesRequest;
use crate::daemon::rpc::managed::outcome::CODE_CONFLICT;
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// A lane that outlives the test: the pass task owns its permit (#8994).
fn leaked_lane() -> &'static tokio::sync::Semaphore {
    Box::leak(Box::new(tokio::sync::Semaphore::new(1)))
}

/// A request for the orphan sweep only, previewing unless `remove`.
fn request(remove: bool) -> PruneWorktreesRequest {
    serde_json::from_value(json!({"dry_run": !remove})).expect("request decodes")
}

/// `n` orphans the sweep would remove: owner provably gone, clean.
fn orphans(fx: &GitWorktreeFixture, stem: &str, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let wt = fx.add_worktree(&format!("{stem}-{i}"));
            GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
            wt
        })
        .collect()
}

async fn state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let root = crate::test_support::hermetic_temp_dir();
    let state = DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await;
    (Arc::new(state), root)
}

/// Every item until the stream ends, bounded.
async fn drain(
    items: &mut trusty_common::uds::server::RpcStreamItems,
) -> Vec<Result<Value, RpcError>> {
    let mut got = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(60), items.recv()).await {
            Ok(Some(item)) => got.push(item),
            Ok(None) => return got,
            Err(_) => panic!("the stream went quiet for 60s: {got:?}"),
        }
    }
}

/// Wait up to `bound` for `done`; returns its last answer.
async fn eventually(bound: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + bound;
    while tokio::time::Instant::now() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    done()
}

/// 🔴 #7619 (a): a plain prune — no `--merged-prs` — reports a progress line
/// naming each worktree, and the result is the last item.
///
/// Fails before #7619: the method does not exist, and the pass emits no
/// progress at all.
#[serial_test::serial]
#[tokio::test]
async fn worktree_7619_the_stream_reports_every_worktree_before_its_result() {
    let fx = GitWorktreeFixture::new();
    let trees = orphans(&fx, "stream-7619", 3);
    let _env = WorkspaceRootEnv::pin(&fx.repos_root);
    let (state, _root) = state().await;

    let mut items = open_prune_stream_in(
        state,
        request(false),
        leaked_lane(),
        Duration::from_secs(60),
    )
    .expect("the stream opens");
    let got: Vec<Value> = drain(&mut items)
        .await
        .into_iter()
        .map(|item| item.expect("no error item"))
        .collect();

    let (last, progress) = got.split_last().expect("at least the result");
    assert_eq!(
        last["kind"], "result",
        "the result is the last item: {got:?}"
    );
    assert_eq!(
        last["body"]["paths"].as_array().map(Vec::len),
        Some(3),
        "{last}"
    );
    for wt in &trees {
        let shown = wt.display().to_string();
        assert!(
            progress.iter().any(|p| p["kind"] == "progress"
                && p["line"].as_str().is_some_and(|l| l.contains(&shown))),
            "no progress line names {shown}: {progress:?}"
        );
    }
}

/// 🔴 #7619 (d1), Fail-Open Check: an item channel already closed when the
/// pass starts is read as cancel — the removal pass removes nothing.
///
/// The test runtime is single-threaded, so the receiver is dropped before any
/// spawned task runs. Fails if the closed channel reads as "keep going".
#[serial_test::serial]
#[tokio::test]
async fn worktree_7619_a_closed_stream_before_the_pass_removes_nothing() {
    let fx = GitWorktreeFixture::new();
    let trees = orphans(&fx, "closed-7619", 3);
    let _env = WorkspaceRootEnv::pin(&fx.repos_root);
    let (state, _root) = state().await;
    let lane = leaked_lane();
    let req: PruneWorktreesRequest =
        serde_json::from_value(json!({"dry_run": false, "merged_prs": true})).expect("decodes");

    drop(open_prune_stream_in(state, req, lane, Duration::from_secs(60)).expect("opens"));
    let freed = tokio::time::timeout(Duration::from_secs(60), lane.acquire()).await;
    assert!(
        freed.is_ok(),
        "the lane must free once the cancelled pass stops"
    );
    for wt in &trees {
        assert!(
            wt.exists(),
            "a closed stream still removed {}",
            wt.display()
        );
    }
}

/// 🔴 #7619 (b), real socket: a client that reads one frame and hangs up
/// stops the pass before its first removal, and the lane is released.
///
/// The pass is parked on the store lock while the client reads a keepalive
/// and leaves. Fails before #7619: the unary socket form never notices.
#[allow(clippy::await_holding_lock)]
#[serial_test::serial]
#[tokio::test]
async fn worktree_7619_a_dropped_stream_stops_removing_and_frees_the_lane() {
    let fx = GitWorktreeFixture::new();
    let trees = orphans(&fx, "socket-7619", 3);
    let _env = WorkspaceRootEnv::pin(&fx.repos_root);
    let (state, _root) = state().await;
    let lane = leaked_lane();
    let mgr = state.session_manager().await;
    let gate = mgr.store.write().await;

    let dir = tempfile::tempdir().expect("socket dir");
    let socket = dir.path().join("s").join("trusty-mpm.sock");
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("dir");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let served = Arc::clone(&state);
    let router =
        RpcRouter::new().typed_stream(PRUNE_STREAM_METHOD, move |req: PruneWorktreesRequest| {
            let state = Arc::clone(&served);
            async move { open_prune_stream_in(state, req, lane, Duration::from_millis(50)) }
        });
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(
            &listener,
            Arc::new(router),
            RpcServeOptions::default(),
            async {
                let _ = shutdown.await;
            },
        )
        .await;
    });

    let frame = json!({
        "jsonrpc": "2.0", "id": 1, "stream": true, "method": PRUNE_STREAM_METHOD,
        "params": {"dry_run": false, "merged_prs": true},
    });
    let mut stream = trusty_common::uds::send_framed_stream_request::<_, Value>(
        &socket,
        &frame,
        Duration::from_secs(10),
    )
    .await
    .expect("the stream opens");
    let first = stream
        .next_frame()
        .await
        .expect("one frame")
        .expect("an item");
    assert_eq!(first["kind"], "keepalive", "{first}");
    drop(stream);
    // The writer polls for a departed peer every 250 ms.
    tokio::time::sleep(Duration::from_millis(800)).await;
    drop(gate);

    let freed = tokio::time::timeout(Duration::from_secs(60), lane.acquire()).await;
    let _ = stop.send(());
    assert!(
        freed.is_ok(),
        "the lane must free once the cancelled pass stops"
    );
    for wt in &trees {
        assert!(
            wt.exists(),
            "the pass kept deleting after its client left: {}",
            wt.display()
        );
    }
}

/// #7619 `--detach`: a detached pass acknowledges first and runs to the end
/// after its client has gone.
#[serial_test::serial]
#[tokio::test]
async fn worktree_7619_a_detached_stream_runs_the_pass_to_the_end() {
    let fx = GitWorktreeFixture::new();
    let trees = orphans(&fx, "detach-7619", 3);
    let _env = WorkspaceRootEnv::pin(&fx.repos_root);
    let (state, _root) = state().await;
    let req: PruneWorktreesRequest =
        serde_json::from_value(json!({"dry_run": false, "detach": true})).expect("decodes");

    let mut items =
        open_prune_stream_in(state, req, leaked_lane(), Duration::from_secs(60)).expect("opens");
    let first = items.recv().await.expect("one item").expect("not an error");
    assert_eq!(first["kind"], "detached", "{first}");
    drop(items);

    let gone = eventually(Duration::from_secs(60), || {
        trees.iter().all(|wt| !wt.exists())
    })
    .await;
    assert!(gone, "a detached pass must remove every orphan: {trees:?}");
}

/// #7619: a refusal opens no pass and carries the unary method's code and
/// message — a bad caller id, and a lane another sweep holds.
#[tokio::test]
async fn worktree_7619_a_refused_stream_request_is_one_terminal_error() {
    let (state, _root) = state().await;
    let bad: PruneWorktreesRequest =
        serde_json::from_value(json!({"invoking_session": "not-a-uuid"})).expect("decodes");
    let err = open_prune_stream_in(
        Arc::clone(&state),
        bad,
        leaked_lane(),
        Duration::from_secs(60),
    )
    .expect_err("a bad caller id refuses");
    assert_eq!(err.code, CODE_INVALID_PARAMS);
    assert!(err.message.contains("#6806"), "{}", err.message);

    let lane = leaked_lane();
    let _held = lane.try_acquire().expect("the test holds the lane");
    let busy: PruneWorktreesRequest =
        serde_json::from_value(json!({"merged_prs": true})).expect("decodes");
    let err = open_prune_stream_in(state, busy, lane, Duration::from_secs(60))
        .expect_err("a held lane refuses");
    assert_eq!(err.code, CODE_CONFLICT);
    assert!(err.message.contains("#8994"), "{}", err.message);
}

/// #7619 (Q4): a quiet pass beats, so a client's idle budget never mistakes a
/// slow call for a stall; the result still comes last.
#[allow(clippy::await_holding_lock)]
#[serial_test::serial]
#[tokio::test]
async fn worktree_7619_the_stream_beats_while_the_pass_is_quiet() {
    let fx = GitWorktreeFixture::new();
    let _env = WorkspaceRootEnv::pin(&fx.repos_root);
    let (state, _root) = state().await;
    let mgr = state.session_manager().await;
    let gate = mgr.store.write().await;

    let mut items = open_prune_stream_in(
        Arc::clone(&state),
        request(false),
        leaked_lane(),
        Duration::from_millis(20),
    )
    .expect("opens");
    let first = items.recv().await.expect("one item").expect("not an error");
    assert_eq!(first["kind"], "keepalive", "{first}");
    assert!(first["elapsed_s"].is_u64(), "{first}");
    drop(gate);
    let rest = drain(&mut items).await;
    let last = rest
        .last()
        .expect("a result")
        .as_ref()
        .expect("not an error");
    assert_eq!(last["kind"], "result", "{rest:?}");
}
