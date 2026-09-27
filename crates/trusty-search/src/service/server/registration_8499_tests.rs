//! #8499 round 2: where a new store may go, how registrations exclude each
//! other, and relocate against a running reindex.
//!
//! Why: round 1 left three holes — a data dir inside the root put the store
//! in the work tree, one process-wide lock serialized every `POST /indexes`,
//! and relocate could move a root under a running reindex.
//! What: every test drives the real handlers against committed temp git
//! repos, reusing the fixtures in `tests_8499`.
//! Test: this module. Run with `cargo test -p trusty-search registration_8499`.

use super::tests_8499::{clean_repo, create_req, git, mock_state, unregister};
use super::tests_components::IsolatedDataDir;
use super::*;
use crate::core::registry::IndexId;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::reindex::{spawn_reindex_awaitable, ReindexProgress, ReindexStatus};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::Path;
use std::sync::Arc;

/// POST `create_req(id, root)` (optionally lexical-only) through the real
/// handler; returns the status and JSON body.
pub(super) async fn post_create(
    state: &Arc<SearchAppState>,
    id: &str,
    root: &Path,
    lexical_only: bool,
) -> (StatusCode, serde_json::Value) {
    let mut req = create_req(id, root.to_path_buf());
    if lexical_only {
        req.lexical_only = Some(true);
    }
    let resp = super::indexes::create_index_handler(State(Arc::clone(state)), Json(req)).await;
    let status = resp.status();
    (status, super::tests_components::body_json(resp).await)
}

/// #8499 round 2: a new registration is refused when the data dir itself sits
/// inside the work tree — `TRUSTY_DATA_DIR` equal to the root, or a plain
/// subdirectory of it with no `.trusty-search` in the path — and an existing
/// colocated artifact is still adopted under the same configuration.
///
/// Why: the #8438 guard exempted a store under the root whenever the data dir
/// was also under the root, so `for_new_registration` wrote the whole store
/// into the work tree, where `git clean -fdx` deletes it.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn create_refuses_a_data_dir_inside_the_work_tree() {
    for (n, rel) in ["", "sub/data"].into_iter().enumerate() {
        let _data = IsolatedDataDir::new();
        let state = mock_state().await;
        let (_dir, root) = clean_repo("indata-8499-", Some("target/\n"));
        let data_dir = if rel.is_empty() {
            root.clone()
        } else {
            root.join(rel)
        };
        // SAFETY: #[serial]; `IsolatedDataDir`'s drop clears the variable.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", &data_dir) };
        let id = format!("in-tree-data-8499-{n}");

        let (status, body) = post_create(&state, &id, &root, false).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "#8499: TRUSTY_DATA_DIR={} puts the store in the work tree: {body}",
            data_dir.display()
        );
        assert!(
            body["error"].as_str().unwrap_or_default().contains("#8499"),
            "the refusal must name the reason: {body}"
        );
        assert!(state.registry.get(&IndexId::new(&id)).is_none());
        assert!(
            !data_dir.join("indexes").exists(),
            "#8499: a refused registration writes no store into the work tree"
        );
        assert_eq!(git(&root, &["status", "--porcelain"]), "", "#8499");

        // Adoption is unaffected: an existing colocated artifact is served.
        let colocated = root.join(COLOCATED_DIR_NAME);
        std::fs::create_dir(&colocated).expect("create .trusty-search");
        drop(
            crate::core::corpus::CorpusStore::open(&colocated.join("index.redb")).expect("corpus"),
        );
        let (status, body) = post_create(&state, &id, &root, false).await;
        assert_eq!(status, StatusCode::OK, "adoption must still work: {body}");
        let handle = state.registry.get(&IndexId::new(&id)).expect("adopted");
        assert_eq!(
            crate::service::storage_layout::layout_of(&handle).await,
            crate::service::storage_layout::StorageLayout::Colocated,
            "an existing colocated store is adopted in place"
        );
        drop(handle);
        unregister(&state, &id).await;
    }
}

/// #8499 round 2: a relocate that would put a data-dir store inside the new
/// root's work tree is refused, and the index stays where it was.
///
/// Why: relocating is registration at a new root (#767); the same store
/// placement rule applies.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn relocate_refuses_a_new_root_that_encloses_the_store() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_old_dir, old_root) = clean_repo("reloc-old-8499-", None);
    let (_new_dir, new_root) = clean_repo("reloc-new-8499-", None);
    // SAFETY: #[serial]; `IsolatedDataDir`'s drop clears the variable.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", new_root.join("data")) };
    const ID: &str = "reloc-enclose-8499";
    let (status, body) = post_create(&state, ID, &old_root, true).await;
    assert_eq!(status, StatusCode::OK, "create: {body}");

    let result = super::indexes_relocate::relocate_index_report(
        &state,
        ID,
        super::indexes_relocate::RelocateIndexRequest {
            root_path: new_root.clone(),
        },
    )
    .await;
    let (status, body) = result.expect_err("#8499: relocate must refuse");
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"].as_str().unwrap_or_default().contains("#8499"));
    let handle = state.registry.get(&IndexId::new(ID)).expect("registered");
    assert_eq!(
        handle.root_path, old_root,
        "the refused relocate moved nothing"
    );
    assert_eq!(handle.indexer.read().await.root_path, old_root);
    drop(handle);
    unregister(&state, ID).await;
}

/// Poll `cond` every 20 ms for up to 10 s; panic naming `what` on timeout.
pub(super) async fn wait_until(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("condition never became true within 10s: {what}");
}

/// #8499 round 2: relocate is refused while a reindex holds the index's
/// per-index permit, so the handle root and the indexer root (#4951) never
/// diverge under a running walk; once the reindex ends, relocate succeeds.
///
/// Why: relocate rebinds a data-dir index by mutating the shared indexer's
/// root in place. A reindex takes only the teardown read side, which relocate
/// also takes, so nothing excluded the two.
/// What: a real reindex is parked in flight — holding its permit — behind a
/// test-held indexer write lock. Relocate must answer `409` without waiting
/// on that lock. The lock is then released, the reindex completes, and a
/// second relocate moves both roots together.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn relocate_is_refused_while_a_reindex_is_in_flight() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_old_dir, old_root) = clean_repo("inflight-old-8499-", None);
    let (_new_dir, new_root) = clean_repo("inflight-new-8499-", None);
    const ID: &str = "reloc-inflight-8499";
    let (status, body) = post_create(&state, ID, &old_root, true).await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    let id = IndexId::new(ID);
    let handle = state.registry.get(&id).expect("registered");

    let gate = handle.indexer.write().await;
    let progress = Arc::new(ReindexProgress::new());
    let run = spawn_reindex_awaitable(Arc::clone(&handle), Arc::clone(&progress), false);
    wait_until("the reindex holds its per-index permit", || {
        crate::service::reindex::index_task_in_flight(&id)
    })
    .await;

    let relocate = || {
        super::indexes_relocate::relocate_index_report(
            &state,
            ID,
            super::indexes_relocate::RelocateIndexRequest {
                root_path: new_root.clone(),
            },
        )
    };
    let refused = tokio::time::timeout(std::time::Duration::from_secs(5), relocate())
        .await
        .expect("#8499: relocate must answer while a reindex runs, not wait on it");
    let (status, body) = refused.expect_err("#8499: relocate during a reindex must refuse");
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("reindex"),
        "the refusal names the running reindex: {body}"
    );
    assert_eq!(state.registry.get(&id).expect("live").root_path, old_root);

    drop(gate);
    run.await.expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete, "reindex");
    assert_eq!(handle.indexer.read().await.root_path, old_root, "#4951");
    wait_until("the permit is released", || {
        !crate::service::reindex::index_task_in_flight(&id)
    })
    .await;

    relocate()
        .await
        .expect("relocate after the reindex succeeds");
    let moved = state.registry.get(&id).expect("live");
    assert_eq!(moved.root_path, new_root);
    assert_eq!(moved.indexer.read().await.root_path, new_root, "#4951");
    drop((handle, moved));
    unregister(&state, ID).await;
}

/// #8499 round 2: a registration in flight over one root does not hold up a
/// registration over an unrelated root, but does hold up one over the same
/// root or under the same id.
///
/// Why: one process-wide lock held across the indexer build serialized every
/// `POST /indexes`.
/// What: a held claim on root A stands in for a registration mid-build. A
/// create at root B completes within a bounded time; a create at root A and a
/// claim under the same id stay parked until the claim is released, and the
/// root-A create then completes.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn unrelated_roots_register_concurrently() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_a_dir, root_a) = clean_repo("claim-a-8499-", None);
    let (_b_dir, root_b) = clean_repo("claim-b-8499-", None);
    let bound = std::time::Duration::from_secs(5);
    let held = super::create_layout::claim_registration("claim-hold-8499", &root_a).await;

    let (status, body) =
        tokio::time::timeout(bound, post_create(&state, "claim-b-8499", &root_b, true))
            .await
            .expect("#8499: an unrelated root must not wait on another registration");
    assert_eq!(status, StatusCode::OK, "{body}");
    let same_id = super::create_layout::claim_registration("claim-hold-8499", &root_b);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), same_id)
            .await
            .is_err(),
        "a claim under the same id must wait"
    );

    let st = Arc::clone(&state);
    let ra = root_a.clone();
    let same_root = tokio::spawn(async move { post_create(&st, "claim-a-8499", &ra, true).await });
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !same_root.is_finished(),
            "#2336: a same-root registration must wait for the one in flight"
        );
    }
    drop(held);
    let (status, body) = tokio::time::timeout(bound, same_root)
        .await
        .expect("releasing the claim wakes the waiter")
        .expect("create task must not panic");
    assert_eq!(status, StatusCode::OK, "{body}");
    unregister(&state, "claim-a-8499").await;
    unregister(&state, "claim-b-8499").await;
}

/// #8499 round 2 / #2336: two creates racing over one root under different
/// ids register exactly one index, on every one of several attempts.
///
/// Why: per-id data-dir stores no longer collide in redb, so only the
/// registration claim stops both racers passing the collision check.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn same_root_race_registers_exactly_once() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    for attempt in 0..8 {
        let (_dir, root) = clean_repo("race-8499-", None);
        let (a, b) = (
            format!("race-a-8499-{attempt}"),
            format!("race-b-8499-{attempt}"),
        );
        let (ra, rb) = tokio::join!(
            post_create(&state, &a, &root, true),
            post_create(&state, &b, &root, true),
        );
        let statuses = [ra.0, rb.0];
        let wins = statuses.iter().filter(|s| **s == StatusCode::OK).count();
        assert_eq!(
            wins, 1,
            "attempt {attempt}: exactly one racer wins: {statuses:?}"
        );
        assert!(statuses.contains(&StatusCode::CONFLICT), "{statuses:?}");
        let winner = if ra.0 == StatusCode::OK { &a } else { &b };
        assert_eq!(state.registry.list().len(), 1, "attempt {attempt}");
        unregister(&state, winner).await;
    }
}
