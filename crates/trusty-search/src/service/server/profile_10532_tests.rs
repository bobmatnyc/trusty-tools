//! #10532: the `std-v1` profile at create, through PATCH, and in `index_status`.
//!
//! Why: an index with no explicit excludes must skip the record directories
//! (`local/owner-queue/items/*.json` made a decision query return the answered
//! item), a profile change must act on what the index already holds without
//! a rebuild, and an index persisted before profiles must still load.
//! What: real handlers over a fixture tree holding source, docs and the
//! Architect's record directories.
//! Test: this module. Run with `cargo test -p trusty-search profile_10532`.

use super::index_config::{patch_index_config_report, PatchIndexConfigRequest};
use super::status::index_status_report;
use super::tests_8499::{clean_repo, create_req, mock_state};
use super::tests_components::IsolatedDataDir;
use super::*;
use crate::core::profile;
use crate::core::registry::{IndexId, StageStatus};
use crate::service::reindex::{spawn_reindex_awaitable, ReindexProgress};
use std::path::Path;
use std::sync::Arc;

const RECORD: &str = "local/owner-queue/items/x.json";
const ARCHIVE: &str = "local/archive/old.md";
const LOG: &str = "local/logs/run.txt";
const SOURCE: &str = "src/lib.rs";
const DOC: &str = "docs/guide.md";

/// A repo holding source, one doc and the three record files.
fn tree() -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, root) = clean_repo("prof-10532-", None);
    for (rel, body) in [
        (RECORD, "{\"decision\": \"answered item\"}\n"),
        (ARCHIVE, "# archived note\nold content\n"),
        (LOG, "log line about the answered item\n"),
        (DOC, "# Guide\nHow the widget works.\n"),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, body).expect("write fixture");
    }
    (dir, root)
}

/// Reindex `id` and return only once nothing on the index is still in flight.
///
/// Why (#10635): the walk's join handle resolves before the deferred embed
/// pass has taken the per-index permit, so a permit that is free for one poll
/// can be taken a moment later. A PATCH that changes the excludes needs that
/// permit and answers 409 otherwise.
/// What: awaits the walk, then waits (30 s deadline) for the semantic stage to
/// leave `Pending`/`InProgress`, which only the embed pass moves, and then for
/// the index's permit to be free. The embed pass releases the permit after it
/// settles the stage, and nothing queues another pass, so this order is final.
/// Test: every test in this module that reindexes before a PATCH.
async fn reindex(state: &Arc<SearchAppState>, id: &str) {
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    spawn_reindex_awaitable(Arc::clone(&handle), Arc::new(ReindexProgress::new()), false)
        .await
        .expect("reindex task");
    let index = IndexId::new(id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let semantic = handle.stages.read().await.semantic.status;
        let embedding = matches!(semantic, StageStatus::Pending | StageStatus::InProgress);
        if !embedding && !crate::service::reindex::index_task_in_flight(&index) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "index {id} never went idle: semantic={semantic:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

async fn chunks_of(state: &Arc<SearchAppState>, id: &str, rel: &str) -> usize {
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    let indexer = handle.indexer.read().await;
    indexer.chunk_ids_for_file(rel).await.len()
}

async fn create(
    state: &Arc<SearchAppState>,
    id: &str,
    root: &Path,
    globs: Option<Vec<String>>,
) -> serde_json::Value {
    let mut req = create_req(id, root.to_path_buf());
    req.exclude_globs = globs;
    let (status, body) = super::test_support::create_answer(state, req).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    body
}

/// Acceptance 1: no explicit excludes applies `std-v1`; the record fixture
/// yields zero chunks, the source and docs stay, and `index_status` says so.
/// Test: this test. Reverting `std_v1_globs()` to an empty list makes the
/// three record assertions fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_create_with_no_excludes_applies_std_v1() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = tree();
    let body = create(&state, "prof-default", &root, None).await;
    let label = format!("std-v1@{}", profile::hash(&profile::std_v1_globs()));
    assert_eq!(body["profile"], label.as_str(), "{body}");

    reindex(&state, "prof-default").await;
    for gone in [RECORD, ARCHIVE, LOG] {
        assert_eq!(chunks_of(&state, "prof-default", gone).await, 0, "{gone}");
    }
    for kept in [SOURCE, DOC] {
        assert!(chunks_of(&state, "prof-default", kept).await > 0, "{kept}");
    }
    let status = index_status_report(&state, "prof-default")
        .await
        .expect("status");
    assert_eq!(status["profile"], label.as_str(), "{status}");
}

/// Explicit globs win over the default; the stored profile is `custom`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn explicit_globs_replace_std_v1_and_are_stored_as_custom() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = tree();
    let globs = vec!["docs/**".to_string()];
    let body = create(&state, "prof-custom", &root, Some(globs.clone())).await;
    let label = format!("custom@{}", profile::hash(&globs));
    assert_eq!(body["profile"], label.as_str(), "{body}");
    reindex(&state, "prof-custom").await;
    assert!(
        chunks_of(&state, "prof-custom", RECORD).await > 0,
        "std-v1 off"
    );
    assert_eq!(chunks_of(&state, "prof-custom", DOC).await, 0);

    let (_other, other_root) = tree();
    let mut bad = create_req("prof-bad", other_root);
    bad.profile = Some("std-v9".into());
    let (status, refusal) = super::test_support::create_answer(&state, bad).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{refusal}");
    assert_eq!(refusal["error"], "unknown_profile");
    assert!(state.registry.get(&IndexId::new("prof-bad")).is_none());
}

/// Acceptance 3: a profile PATCH removes the newly excluded files in place and
/// runs no reindex; an `exclude_globs` PATCH does the same and stores `custom`.
/// Test: this test. The walk stamp and the source chunk ids are unchanged, so
/// the corpus was not rebuilt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_profile_patch_removes_newly_excluded_files_and_does_not_rebuild() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = tree();
    // An index built with an unrelated exclude holds the record files.
    create(&state, "prof-patch", &root, Some(vec!["nothing/**".into()])).await;
    reindex(&state, "prof-patch").await;
    assert!(chunks_of(&state, "prof-patch", RECORD).await > 0);
    let handle = state
        .registry
        .get(&IndexId::new("prof-patch"))
        .expect("handle");
    let walked_before = handle
        .walk_diagnostics
        .read()
        .await
        .last_walk_started_at
        .clone();
    let source_before = {
        let idx = handle.indexer.read().await;
        idx.chunk_ids_for_file(SOURCE).await
    };

    let patched = patch_index_config_report(
        &state,
        "prof-patch",
        PatchIndexConfigRequest {
            profile: Some("std-v1".into()),
            ..Default::default()
        },
    )
    .await
    .expect("PATCH");
    assert_eq!(patched["profile_purge"]["rebuilt"], false, "{patched}");
    assert_eq!(patched["profile_purge"]["files_removed"], 3, "{patched}");
    for gone in [RECORD, ARCHIVE, LOG] {
        assert_eq!(chunks_of(&state, "prof-patch", gone).await, 0, "{gone}");
    }
    let handle = state
        .registry
        .get(&IndexId::new("prof-patch"))
        .expect("handle");
    assert_eq!(
        handle.walk_diagnostics.read().await.last_walk_started_at,
        walked_before,
        "no walk ran"
    );
    let source_after = handle.indexer.read().await.chunk_ids_for_file(SOURCE).await;
    assert_eq!(source_after, source_before, "kept chunks were not rebuilt");
    let status = index_status_report(&state, "prof-patch")
        .await
        .expect("status");
    assert!(
        status["profile"]
            .as_str()
            .unwrap_or("")
            .starts_with("std-v1@"),
        "{status}"
    );

    // Adding a glob removes just what it adds, and the profile becomes custom.
    let mut globs = profile::std_v1_globs();
    globs.push("docs/**".into());
    let patched = patch_index_config_report(
        &state,
        "prof-patch",
        PatchIndexConfigRequest {
            exclude_globs: Some(globs.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("PATCH");
    assert_eq!(patched["profile_purge"]["files_removed"], 1, "{patched}");
    assert_eq!(chunks_of(&state, "prof-patch", DOC).await, 0);
    assert!(chunks_of(&state, "prof-patch", SOURCE).await > 0);
    assert_eq!(
        patched["profile"],
        format!("custom@{}", profile::hash(&globs)).as_str()
    );
}

/// A PATCH that names both a profile and a list, or an unknown profile, is
/// refused before anything changes.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn patch_refuses_a_conflicting_pair_and_an_unknown_profile() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = tree();
    create(&state, "prof-refuse", &root, None).await;
    for req in [
        PatchIndexConfigRequest {
            profile: Some("std-v1".into()),
            exclude_globs: Some(vec!["a/**".into()]),
            ..Default::default()
        },
        PatchIndexConfigRequest {
            profile: Some("std-v9".into()),
            ..Default::default()
        },
    ] {
        let (status, body) = patch_index_config_report(&state, "prof-refuse", req)
            .await
            .expect_err("refused");
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    }
}

/// Acceptance: an index persisted before this change loads, with no profile.
/// Test: this test. A row with the profile fields round-trips too.
#[test]
#[serial_test::parallel]
fn a_pre_profile_index_loads_with_no_profile() {
    use crate::service::persistence::{
        load_index_registry_at, upsert_index_registry_entry_at, PersistedIndex,
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("indexes.toml");
    std::fs::write(
        &path,
        "[[index]]\nid = \"legacy\"\nroot_path = \"/tmp/legacy\"\nexclude_globs = [\"x/**\"]\n",
    )
    .expect("write");
    let rows = load_index_registry_at(&path).expect("loads");
    assert_eq!(
        (rows[0].profile.clone(), rows[0].profile_hash.clone()),
        (None, None)
    );
    assert_eq!(
        super::profile_apply::stored_label(Some(&path), "legacy"),
        None
    );

    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "new".into(),
            root_path: "/tmp/new".into(),
            profile: Some("std-v1".into()),
            profile_hash: Some("abcd1234".into()),
            ..Default::default()
        },
    )
    .expect("upsert");
    assert_eq!(
        super::profile_apply::stored_label(Some(&path), "new").as_deref(),
        Some("std-v1@abcd1234")
    );
    assert_eq!(
        super::profile_apply::stored_label(Some(&path), "legacy"),
        None
    );
}

/// A fixture repo whose root sits under a directory called `parent`.
fn tree_under(parent: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, base) = super::test_support::allowlisted_index_root("prof-up-10532-");
    let root = base.join(parent).join("proj");
    crate::allowlist::test_fixtures::approve(&root);
    for (rel, body) in [
        (SOURCE, "pub fn widget() -> u32 { 42 }\n"),
        (DOC, "# Guide\nHow the widget works.\n"),
        (RECORD, "{\"decision\": \"answered item\"}\n"),
        (LOG, "log line about the answered item\n"),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, body).expect("write fixture");
    }
    (dir, root)
}

async fn indexes_source_but_not_records(parent: &str, id: &str) {
    let state = mock_state().await;
    let (_dir, root) = tree_under(parent);
    create(&state, id, &root, None).await;
    reindex(&state, id).await;
    for kept in [SOURCE, DOC] {
        assert!(
            chunks_of(&state, id, kept).await > 0,
            "{kept} under {parent}/"
        );
    }
    for gone in [RECORD, LOG] {
        assert_eq!(chunks_of(&state, id, gone).await, 0, "{gone}");
    }
}

/// An index root below a directory named `archive` indexes normally: the
/// globs match the root-relative path, not the directories above the root.
/// Test: this test. With absolute-path matching `src/lib.rs` has no chunks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn an_index_root_under_an_archive_dir_still_indexes_its_source() {
    let _data = IsolatedDataDir::new();
    indexes_source_but_not_records("archive", "prof-up-archive").await;
}

/// Same for a root below `.worktrees`, where linked worktrees live.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn an_index_root_under_a_worktrees_dir_still_indexes_its_source() {
    let _data = IsolatedDataDir::new();
    indexes_source_but_not_records(".worktrees", "prof-up-wt").await;
}

/// A glob written as an absolute pattern still matches the absolute path.
/// Test: this test.
#[test]
#[serial_test::parallel]
fn an_absolute_glob_still_matches_and_a_root_ancestor_name_does_not() {
    use crate::service::index_admission::globs_exclude;
    let root = std::path::PathBuf::from("/data/archive/proj");
    let file = root.join("src/lib.rs");
    let std_v1 = profile::std_v1_globs();
    assert!(!globs_exclude(&root, &[], &std_v1, &file));
    assert!(globs_exclude(
        &root,
        &[],
        &std_v1,
        &root.join("archive/old.md")
    ));
    assert!(globs_exclude(
        &root,
        &[],
        &["/data/archive/proj/src/**".to_string()],
        &file
    ));
    assert!(!globs_exclude(
        &root,
        &[],
        &["/elsewhere/**".to_string()],
        &file
    ));
}

/// A purge that failed is finished by repeating the same PATCH, even though
/// the stored list no longer differs from the request.
/// Test: this test. Holding the corpus scan gate makes the batch removal
/// busy; before the fix the repeat PATCH saw old == new and skipped the purge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_failed_purge_is_finished_by_a_repeat_patch() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = tree();
    create(&state, "prof-retry", &root, Some(vec!["nothing/**".into()])).await;
    reindex(&state, "prof-retry").await;
    let handle = state
        .registry
        .get(&IndexId::new("prof-retry"))
        .expect("handle");
    let gate = handle.indexer.read().await.scan_gate_for_test();
    let patch = || {
        patch_index_config_report(
            &state,
            "prof-retry",
            PatchIndexConfigRequest {
                profile: Some("std-v1".into()),
                ..Default::default()
            },
        )
    };
    {
        let _held = gate.read().await;
        let failed = patch()
            .await
            .expect("the config applies; the purge reports");
        assert!(failed["profile_purge"]["error"].is_string(), "{failed}");
        assert!(
            chunks_of(&state, "prof-retry", RECORD).await > 0,
            "nothing removed"
        );
    }
    let retried = patch().await.expect("PATCH");
    assert_eq!(retried["profile_purge"]["files_removed"], 3, "{retried}");
    assert_eq!(chunks_of(&state, "prof-retry", RECORD).await, 0);
}

/// A PATCH that changes the excludes is refused with 409 while a reindex (or
/// catch-up) holds the index, and changes nothing.
/// Test: this test. Before the fix a profile-only PATCH took no permit and
/// purged under a walk that still used the old globs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_profile_patch_is_refused_while_a_reindex_holds_the_index() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = tree();
    create(&state, "prof-409", &root, Some(vec!["nothing/**".into()])).await;
    reindex(&state, "prof-409").await;
    let permit = crate::service::reindex::index_semaphore(&IndexId::new("prof-409"))
        .try_acquire_owned()
        .expect("permit free");
    let (status, body) = patch_index_config_report(
        &state,
        "prof-409",
        PatchIndexConfigRequest {
            profile: Some("std-v1".into()),
            ..Default::default()
        },
    )
    .await
    .expect_err("refused");
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{body}");
    assert!(
        chunks_of(&state, "prof-409", RECORD).await > 0,
        "nothing purged"
    );
    let handle = state
        .registry
        .get(&IndexId::new("prof-409"))
        .expect("handle");
    assert_eq!(handle.exclude_globs, vec!["nothing/**".to_string()]);
    drop(permit);
}

/// After a PATCH, a reindex keeps the excluded files out.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_reindex_after_a_profile_patch_keeps_excluded_files_out() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = tree();
    create(&state, "prof-re", &root, Some(vec!["nothing/**".into()])).await;
    reindex(&state, "prof-re").await;
    patch_index_config_report(
        &state,
        "prof-re",
        PatchIndexConfigRequest {
            profile: Some("std-v1".into()),
            ..Default::default()
        },
    )
    .await
    .expect("PATCH");
    reindex(&state, "prof-re").await;
    for gone in [RECORD, ARCHIVE, LOG] {
        assert_eq!(chunks_of(&state, "prof-re", gone).await, 0, "{gone}");
    }
    assert!(chunks_of(&state, "prof-re", SOURCE).await > 0);
}
