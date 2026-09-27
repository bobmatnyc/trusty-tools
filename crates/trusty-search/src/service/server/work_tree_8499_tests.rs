//! #8499 round 3: the enclosing work tree, a store still held by an earlier
//! generation, and relocate under the registration claim.
//!
//! Why: round 2 compared the data dir only against the index root, answered a
//! busy store with `500`, and let relocate check its root collision without
//! the claim `POST /indexes` holds.
//! What: every test drives the real handlers against committed temp git
//! repos, reusing the fixtures in `tests_8499` and `registration_8499_tests`.
//! Test: this module. Run with `cargo test -p trusty-search work_tree_8499`.

use super::registration_8499_tests::post_create;
use super::tests_8499::{clean_repo, git, mock_state, unregister};
use super::tests_components::IsolatedDataDir;
use super::*;
use crate::core::registry::IndexId;
use crate::service::persistence::PersistedIndex;
use axum::http::StatusCode;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// #8499 round 3: a root below the top of its git work tree is refused when
/// the data dir sits anywhere in that work tree, not only under the root. A
/// data dir outside the repository is accepted, and an index already in
/// `indexes.toml` keeps registering (the #8438 exemption).
///
/// Why: `git clean -fdx` at the repository top deletes `<repo>/data` whatever
/// root the index was registered at; round 2 compared against the root only.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn create_refuses_a_data_dir_elsewhere_in_the_enclosing_work_tree() {
    let data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, repo) = clean_repo("nested-8499-", Some("target/\n"));
    let root = repo.join("src");
    let in_repo = repo.join("data");
    // SAFETY: #[serial]; `IsolatedDataDir`'s drop clears the variable.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", &in_repo) };

    let (status, body) = post_create(&state, "nested-in-repo-8499", &root, true).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "#8499: a data dir at {} is inside the work tree that holds {}: {body}",
        in_repo.display(),
        root.display()
    );
    assert!(
        body["error"].as_str().unwrap_or_default().contains("#8499"),
        "the refusal must name the reason: {body}"
    );
    assert!(!in_repo.join("indexes").exists(), "no store was written");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "", "#8499");

    // An entry registered before this rule keeps working.
    crate::service::persistence::upsert_index_registry_entry(PersistedIndex {
        id: "nested-existing-8499".to_string(),
        root_path: root.clone(),
        ..Default::default()
    })
    .expect("seed indexes.toml");
    let (status, body) = post_create(&state, "nested-existing-8499", &root, true).await;
    assert_eq!(status, StatusCode::OK, "#8438 existing entry: {body}");
    unregister(&state, "nested-existing-8499").await;

    // SAFETY: as above.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data.path()) };
    let (status, body) = post_create(&state, "nested-outside-8499", &root, true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a data dir outside the repo: {body}"
    );
    unregister(&state, "nested-outside-8499").await;
}

/// #8499 round 3: a `.git` FILE marks the top of a linked worktree (or a
/// submodule), exactly as `git rev-parse --show-toplevel` reads it; a path
/// in no repository has no enclosing work tree.
///
/// Why: the walk must stop at the linked worktree nested inside the main
/// repository rather than climb to the main repository's top.
/// Test: this test.
#[test]
fn a_git_file_marks_the_top_of_a_linked_worktree() {
    let (_dir, repo) = clean_repo("wt-8499-", None);
    let linked = repo.join("linked");
    let linked_arg = linked.to_str().expect("utf-8 temp path");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "linked-8499", linked_arg],
    );
    assert!(linked.join(".git").is_file(), "fixture: a linked worktree");
    let top = crate::service::storage_layout::enclosing_work_tree(&linked.join("src"));
    assert_eq!(top.as_deref(), Some(linked.as_path()), "#8499");
    assert_eq!(
        crate::service::storage_layout::enclosing_work_tree(&repo.join("src")).as_deref(),
        Some(repo.as_path())
    );
    let outside = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        crate::service::storage_layout::enclosing_work_tree(outside.path()),
        None,
        "no repository, no work tree"
    );
}

/// #8499 round 3: a re-registration that finds the store still open under an
/// earlier generation of the same index answers a retryable `503` naming the
/// index, never a `500`, and succeeds once that generation lets go.
///
/// Why: a deferred embed job holds its `Arc<IndexHandle>` — and with it the
/// redb file — while it waits for the one background permit. A residency park
/// deregisters the handle without closing it, and the next session-launch
/// `POST /indexes` then failed its corpus open with a bare `500`. This is also
/// how round 1's `index_survives_git_reset_hard_and_clean_fdx` flaked under
/// full-suite load.
/// What: a held clone of the registered handle stands in for the parked
/// embed job, which holds exactly that — waiting on the real, process-wide
/// queue made the release depend on every other test's jobs. The index is
/// parked cold as the residency sweep parks it, then re-registered; dropping
/// the clone releases the store and the retry registers.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn re_register_while_an_earlier_handle_holds_the_store_is_retryable() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = clean_repo("busy-8499-", None);
    const ID: &str = "busy-store-8499";
    let id = IndexId::new(ID);

    let (status, body) = post_create(&state, ID, &root, true).await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    let earlier = state.registry.get(&id).expect("registered");
    let entry = crate::service::persistence::find_index_registry_entry(ID)
        .expect("read indexes.toml")
        .expect("persisted");
    let parked = crate::service::lazy_loader::cold_park_index(
        &id,
        &state.registry,
        &state.cold_store,
        entry,
        || false,
    )
    .await;
    assert!(parked, "parked");
    // As `residency_sweep` does after a park.
    state.watcher_manager.stop_for_index(&id).await;

    let (status, body) = post_create(&state, ID, &root, true).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "#8499: a busy store is retryable, not a 500: {body}"
    );
    assert_eq!(body["index_id"], ID, "{body}");
    assert_eq!(body["retryable"], true, "{body}");
    assert!(state.registry.get(&id).is_none(), "nothing registered");

    drop(earlier);
    let (status, body) = post_create(&state, ID, &root, true).await;
    assert_eq!(status, StatusCode::OK, "retry after release: {body}");
    unregister(&state, ID).await;
}

/// Register a throwaway index `id` over a fresh committed repo.
async fn indexed_root(
    state: &Arc<SearchAppState>,
    prefix: &str,
    id: &str,
    keep: &mut Vec<tempfile::TempDir>,
) {
    let (dir, root) = clean_repo(prefix, None);
    keep.push(dir);
    let (status, body) = post_create(state, id, &root, true).await;
    assert_eq!(status, StatusCode::OK, "create {id}: {body}");
}

/// Relocate `id` to `root` through the real report function.
async fn relocate(state: &Arc<SearchAppState>, id: &str, root: &Path) -> StatusCode {
    let req = super::indexes_relocate::RelocateIndexRequest {
        root_path: root.to_path_buf(),
    };
    match super::indexes_relocate::relocate_index_report(state, id, req).await {
        Ok(_) => StatusCode::OK,
        Err((status, _)) => status,
    }
}

/// Count the `OK`s among `statuses`.
fn wins(statuses: &[StatusCode]) -> usize {
    statuses.iter().filter(|s| **s == StatusCode::OK).count()
}

/// #8499 round 3 / #2336: relocate takes the registration claim for its new
/// root, so two relocates into one root, or a create and a relocate into
/// overlapping roots, never both succeed — on every one of several attempts.
///
/// Why: relocate checked the collision and then acted without the claim, and
/// checked no nesting at all (#4289), so both racers could win.
/// What: each attempt starts the two writers on separate tasks behind a
/// barrier, then counts the winners.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn relocate_races_into_one_root_register_exactly_once() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    for attempt in 0..8 {
        let mut keep = Vec::new();
        let a = format!("reloc-a-8499-{attempt}");
        let b = format!("reloc-b-8499-{attempt}");
        let c = format!("reloc-c-8499-{attempt}");
        indexed_root(&state, "rr-a-8499-", &a, &mut keep).await;
        indexed_root(&state, "rr-b-8499-", &b, &mut keep).await;
        let (target_dir, target) = clean_repo("rr-t-8499-", None);
        let (outer_dir, outer) = clean_repo("rr-o-8499-", None);
        keep.extend([target_dir, outer_dir]);

        // Two relocates into the same root.
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        let racer = |id: String, to: PathBuf| {
            let (st, g) = (Arc::clone(&state), Arc::clone(&gate));
            tokio::spawn(async move {
                g.wait().await;
                relocate(&st, &id, &to).await
            })
        };
        let (ra, rb) = (racer(a.clone(), target.clone()), racer(b.clone(), target));
        let statuses = [ra.await.expect("task"), rb.await.expect("task")];
        assert_eq!(wins(&statuses), 1, "attempt {attempt}: {statuses:?}");
        assert!(statuses.contains(&StatusCode::CONFLICT), "{statuses:?}");

        // The loser relocates into a subtree while a create takes its parent.
        let loser = if statuses[0] == StatusCode::OK {
            &b
        } else {
            &a
        };
        let reloc = racer(loser.clone(), outer.join("src"));
        let (st, g, cid) = (Arc::clone(&state), Arc::clone(&gate), c.clone());
        let create = tokio::spawn(async move {
            g.wait().await;
            post_create(&st, &cid, &outer, true).await.0
        });
        let statuses = [reloc.await.expect("task"), create.await.expect("task")];
        assert_eq!(wins(&statuses), 1, "attempt {attempt}: nested {statuses:?}");

        for id in [&a, &b, &c] {
            if state.registry.get(&IndexId::new(id.as_str())).is_some() {
                unregister(&state, id).await;
            }
        }
    }
}
