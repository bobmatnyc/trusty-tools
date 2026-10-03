//! Daemon-state tests for `disk_survey` through its cache (#8985).
//!
//! Why: `disk_survey_cache_tests` drives the cache with synthetic surveys; these
//! drive [`super::disk_survey`] itself — a real `DaemonState`, a real git fleet
//! in a tempdir, the real size index and the real background task — so the
//! wiring between the tool, the cache and the background pass is proven, not
//! assumed.
//! What: the workspace root is pinned at the fixture with
//! [`WorkspaceRootEnv`] (and `#[serial_test::serial]`, for the tests that
//! mutate the same variable under `serial` alone), so no test here reads the
//! operator's fleet. A live pass is cut short deterministically by holding the
//! shared size index past the budget from another thread.
//! Test target: `super::disk_survey`, `super::settle_in_background`.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{disk_survey, settle_in_background};
use crate::core::trusty_tools_config::WorkspaceRootEnv;
use crate::daemon::disk_survey_cache::{Plan, SurveyKey};
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// A `DaemonState` with a pre-seeded fake-tmux session manager, so nothing
/// adopts the machine's live sessions.
async fn test_state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = DaemonState::with_root_isolated_managed(dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// The key a call with no `project` and no `group_by` uses.
fn key() -> SurveyKey {
    SurveyKey {
        project: None,
        group_by: None,
    }
}

/// The first worktree row's byte figure.
fn first_worktree_bytes(survey: &Value) -> Option<u64> {
    survey["root"]["projects"][0]["worktrees"][0]["bytes"].as_u64()
}

/// 🔴 MEDIUM-2 (#8985 review): a budgeted call that runs out of budget starts
/// the background pass, and once that pass lands a later budgeted call is
/// answered from it — `cached`, complete, with byte figures.
///
/// The live pass is cut short by holding the size index for longer than the
/// one-second budget; the background pass, which has no budget, waits for the
/// index and completes once it is released.
#[tokio::test]
#[serial_test::serial]
async fn a_budgeted_call_is_answered_from_the_background_pass() {
    let fleet = GitWorktreeFixture::new();
    fleet.add_worktree("a");
    let _env = WorkspaceRootEnv::pin(&fleet.repos_root);
    let (state, _dir) = test_state().await;

    let index = state.disk_size_index();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _held = index.lock();
        locked_tx.send(()).expect("signal the hold");
        // Ends when the test sends, or drops the sender on a panic.
        let _ = release_rx.recv();
    });
    locked_rx.recv().expect("the index is held");

    let first = disk_survey(&state, None, Some(1), None)
        .await
        .expect("a budgeted survey");
    assert_eq!(first["partial"], true, "{first}");
    assert_eq!(first["freshness"], "partial", "{first}");
    assert_eq!(first["background_pass"], "running", "{first}");
    assert_eq!(first["age_seconds"], 0, "{first}");

    release_tx.send(()).expect("release the index");
    holder.join().expect("the holder thread");

    let cache = state.disk_survey_cache();
    let deadline = Instant::now() + Duration::from_secs(120);
    while cache.plan(&key(), Instant::now()) == Plan::Live {
        assert!(
            Instant::now() < deadline,
            "the background pass never landed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let later = disk_survey(&state, None, Some(1), None)
        .await
        .expect("a budgeted survey");
    assert_eq!(later["freshness"], "cached", "{later}");
    assert_eq!(later["partial"], false, "{later}");
    assert_eq!(later["background_pass"], "idle", "{later}");
    assert_eq!(later["budget_clamped"], false, "{later}");
    assert!(later["root"]["bytes"].as_u64().is_some(), "{later}");
    assert!(first_worktree_bytes(&later).is_some(), "{later}");
}

/// 🔴 MEDIUM-1 (#8985 review): a call that omits `budget_seconds` runs a live,
/// unbounded survey even for a key known to exceed the budget, and its
/// complete pass clears that key's budget bit.
///
/// Fails before the fix: the call consulted the cache and answered the seeded
/// pass, `freshness: cached`, with the seeded root path.
#[tokio::test]
#[serial_test::serial]
async fn an_omitted_budget_always_runs_a_live_survey() {
    let root = tempfile::tempdir().expect("an empty workspace root");
    let _env = WorkspaceRootEnv::pin(root.path());
    let (state, _dir) = test_state().await;

    let cache = state.disk_survey_cache();
    let t0 = Instant::now();
    let (_, took) = cache.after_live(&key(), json!({ "partial": true }), t0, t0);
    assert!(took, "the seeded truncated pass takes the slot");
    let seeded = json!({ "partial": false, "root": { "path": "/seeded", "bytes": 1 } });
    cache.after_background(&key(), Ok(seeded), t0);
    assert!(
        matches!(cache.plan(&key(), t0), Plan::Serve { .. }),
        "precondition: the key is known to exceed the budget"
    );

    let answer = disk_survey(&state, None, None, None)
        .await
        .expect("an unbudgeted survey");
    assert_eq!(answer["freshness"], "live", "{answer}");
    assert_ne!(answer["root"]["path"], "/seeded", "{answer}");
    assert_eq!(answer["partial"], false, "{answer}");
    assert_eq!(
        cache.plan(&key(), Instant::now()),
        Plan::Live,
        "a complete live pass clears the budget bit"
    );
}

/// MEDIUM-2 error arm (#8985 review): a background pass that fails, or
/// panics, still hands the refresh slot back, so the next truncated pass can
/// start one.
#[tokio::test]
async fn a_failed_or_panicking_background_pass_hands_the_slot_back() {
    let (state, _dir) = test_state().await;
    let cache = state.disk_survey_cache();
    let partial = || json!({ "partial": true });
    let t0 = Instant::now();

    let (_, took) = cache.after_live(&key(), partial(), t0, t0);
    assert!(took);
    settle_in_background(&state, key(), async { Err("the pass failed".to_string()) })
        .await
        .expect("the settle task");
    let (_, took) = cache.after_live(&key(), partial(), t0, t0);
    assert!(took, "a failed pass handed the slot back");

    settle_in_background(&state, key(), async {
        panic!("the pass panicked");
    })
    .await
    .expect("the settle task survives the pass's panic");
    let (_, took) = cache.after_live(&key(), partial(), t0, t0);
    assert!(took, "a panicking pass handed the slot back");
}
