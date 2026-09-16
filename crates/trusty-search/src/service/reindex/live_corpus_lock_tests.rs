//! #7991 regression tests: staged promotion must not rename over a live
//! `index.redb` another opener holds.
//!
//! Why: the reindex window drops this daemon's handle on the live corpus, so a
//! second daemon on a shared filesystem can open it and then be left reading an
//! inode the rename unlinked. The gate's whole job is to tell "held" from "not
//! held" using the same advisory lock redb takes, and every branch it cannot
//! decide must fall to refuse.
//! What: one test per arm — held, unheld, absent, and unopenable — plus proof
//! that the held case is detected through a real `redb::Database` rather than a
//! synthetic lock, so the gate is contending with redb's own primitive.
//! Test: `a_held_live_corpus_refuses_promotion`,
//! `an_unheld_live_corpus_is_promotable`,
//! `a_missing_live_corpus_is_promotable`,
//! `an_unopenable_live_path_refuses_promotion`,
//! `a_deferred_promotion_is_reported_in_status_and_is_not_complete`,
//! `a_landed_promotion_clears_an_earlier_deferral`,
//! `a_rename_failure_quarantines_and_is_not_reported_as_a_deferral`,
//! `a_rename_failure_is_not_a_completed_reindex`,
//! `the_next_runs_probe_discards_a_deferred_runs_staging_corpus`.

use super::live_corpus_lock::acquire_for_promotion;
use crate::core::corpus::CorpusStore;
use crate::core::registry::IndexId;

fn id() -> IndexId {
    IndexId::new("live-lock-7991")
}

/// A colocated index whose live corpus exists and whose staging store is
/// attached, checkpointed, and ready to promote.
///
/// Why: `commit_staged_corpus_swap` routes on `has_colocated_storage`, so the
/// test has to build the real `.trusty-search/` layout rather than hand it two
/// arbitrary paths. Returns the handle, the live path and the staging path.
/// Test: used by `a_deferred_promotion_is_reported_in_status_and_is_not_complete`.
fn staged_index(
    index_id: &str,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::core::registry::IndexHandle>,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexRegistry};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let colocated = root.join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&colocated).unwrap();
    let live = colocated.join("index.redb");
    let tmp = colocated.join("index.redb.tmp");
    // The live corpus must exist for `has_colocated_storage` and for the gate
    // to have something to lock.
    drop(CorpusStore::open(&live).expect("create live corpus"));

    let staging = CorpusStore::open(&tmp).expect("create staging corpus");
    staging
        .write_reindex_checkpoint_sync(br#"{"probe":"7991"}"#)
        .expect("stamp checkpoint");
    let mut indexer = CodeIndexer::new(index_id, &root);
    indexer.set_corpus_store(std::sync::Arc::new(staging));

    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new(index_id),
        std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
        root,
    ));
    (dir, handle, live, tmp)
}

/// #7991 verbatim: a live corpus another opener holds must refuse promotion.
///
/// Why: this is the shared-filesystem case. The holder is a real
/// `CorpusStore`, so the lock the gate contends with is redb's own — a
/// hand-rolled `flock` in the test would prove only that the test locks.
/// What: opens a corpus, asserts the gate refuses while it lives, and asserts
/// it admits again once the holder drops. The second half is what makes the
/// first half a detection of the holder rather than of the file.
/// Test: this IS the test.
#[tokio::test]
async fn a_held_live_corpus_refuses_promotion() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("index.redb");
    let holder = CorpusStore::open(&live).expect("open live corpus");

    assert!(
        acquire_for_promotion(&live, &id()).await.is_none(),
        "#7991: a live corpus held open by another redb opener must refuse promotion"
    );

    drop(holder);
    assert!(
        acquire_for_promotion(&live, &id()).await.is_some(),
        "#7991: the refusal must track the holder, not the file's existence"
    );
}

/// The gate must not wedge an ordinary promotion shut.
#[tokio::test]
async fn an_unheld_live_corpus_is_promotable() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("index.redb");
    CorpusStore::open(&live).expect("create live corpus");

    let guard = acquire_for_promotion(&live, &id())
        .await
        .expect("#7991: an unheld live corpus must be promotable");
    // Held across the would-be rename: any opener arriving now fails its own
    // open rather than landing on the inode about to be unlinked.
    assert!(
        CorpusStore::open(&live).is_err(),
        "#7991: the guard must hold the lock, not merely have taken it"
    );
    drop(guard);
    CorpusStore::open(&live).expect("the lock must be released with the guard");
}

/// A first promotion has no live file, so nothing can be holding one.
#[tokio::test]
async fn a_missing_live_corpus_is_promotable() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("index.redb");
    assert!(
        acquire_for_promotion(&live, &id()).await.is_some(),
        "#7991: an absent live corpus must not block the first promotion"
    );
}

/// #7991 round 2: a refused promotion must be visible, not just `false`.
///
/// Why: `resolve_corpus_swap` used `promoted` only to gate the #7004
/// reconciliation, so a refused run still pushed `ReindexStatus::Complete`. The
/// indexer kept serving the staging corpus, `chunk_count` looked healthy, and
/// the live `index.redb` stayed at its pre-reindex state until a restart threw
/// the whole run away — with nothing anywhere saying so.
/// What: drives the real `commit_staged_corpus_swap` against a live corpus a
/// second opener holds, then asserts the whole contract: refused, nothing on
/// disk touched, staging still attached and still checkpointed, the deferral in
/// the status body, and `resolve_corpus_swap` reporting the non-Complete
/// terminal status to `finish_reindex`.
/// Test: this IS the test.
#[tokio::test]
async fn a_deferred_promotion_is_reported_in_status_and_is_not_complete() {
    use super::finish_teardown::resolve_corpus_swap;
    use super::{staging::StagingResolution, validate::ReindexOutcome};

    let (_dir, handle, live, tmp) = staged_index("deferred-7991");
    // Another process (here, another opener) holds the live corpus. The
    // baseline is taken AFTER that open, because opening a redb database
    // rewrites its header — this test is about the RENAME, not about redb's own
    // bookkeeping.
    let _holder = CorpusStore::open(&live).expect("hold the live corpus");
    let live_before = std::fs::read(&live).unwrap();

    let deferred = resolve_corpus_swap(
        &handle,
        &handle.id,
        handle.root_path.as_path(),
        Some(tmp.as_path()),
        &StagingResolution::Commit,
        &ReindexOutcome::Ready,
        false,
        true,
    )
    .await;

    assert!(
        deferred,
        "#7991: the refusal must reach finish_reindex as a non-Complete terminal status"
    );
    assert_eq!(
        std::fs::read(&live).unwrap(),
        live_before,
        "#7991: the live corpus must be byte-identical"
    );
    assert!(tmp.exists(), "#7991: the staging file must not be deleted");

    let indexer = handle.indexer.read().await;
    assert!(
        indexer.has_corpus_store(),
        "#7991: staging must stay attached — nothing was released"
    );
    let staging = indexer.corpus_store().expect("staging store");
    assert_eq!(
        staging
            .read_reindex_checkpoint_sync()
            .expect("read checkpoint")
            .as_deref(),
        Some(&br#"{"probe":"7991"}"#[..]),
        "#7991: the checkpoint must be intact — the clear runs only on promotion"
    );
    let reason = indexer
        .promotion_deferred()
        .expect("#7991: the refusal must be recorded where status can report it")
        .reason;
    drop(indexer);
    assert!(
        reason.contains("could not be exclusively locked"),
        "the reason must say what happened, got: {reason}"
    );

    let state = std::sync::Arc::new(crate::service::server::SearchAppState::new({
        let r = crate::core::registry::IndexRegistry::new();
        r.register(crate::core::registry::IndexHandle::bare(
            handle.id.clone(),
            handle.indexer.clone(),
            handle.root_path.clone(),
        ));
        r
    }));
    let body = crate::service::server::index_status_report(&state, "deferred-7991")
        .await
        .expect("status 200");
    assert!(
        body["promotion_deferred"]["reason"].is_string(),
        "#7991: GET /indexes/:id/status must carry the deferral, got: {}",
        body["promotion_deferred"]
    );
    assert!(body["promotion_deferred"]["at"].is_string());
}

/// A promotion that lands must clear an earlier deferral.
#[tokio::test]
async fn a_landed_promotion_clears_an_earlier_deferral() {
    use super::corpus_swap::commit_staged_corpus_swap;

    let (_dir, handle, _live, tmp) = staged_index("landed-7991");
    handle
        .indexer
        .read()
        .await
        .record_promotion_deferred("an earlier run was refused");

    assert!(
        commit_staged_corpus_swap(&handle, &handle.id, &tmp).await,
        "#7991: an unheld live corpus must still promote"
    );
    assert!(
        handle.indexer.read().await.promotion_deferred().is_none(),
        "#7991: a landed promotion must clear the stale deferral"
    );
}

/// #7991: a FAILED rename must not borrow the deferral's terminal status.
///
/// Why: `commit_staged_corpus_swap` returns `false` from several arms, and only
/// the promotion gate's refusal means "nothing was attempted, the live corpus
/// still serves". A failed rename is the opposite state — the checkpoint is
/// cleared and the staging handle released, so the indexer is quarantined with
/// no corpus at all. Keying `ReindexStatus::PromotionDeferred` off the bare
/// `!promoted` would report that damaged run as a clean deferral, and the
/// rename arm had no call-site coverage at all.
/// What: forces `std::fs::rename` to fail with a read-only colocated directory
/// (the live and staging FILES stay openable, so the gate itself passes), then
/// asserts the run is NOT deferred, no deferral was recorded, the index is
/// quarantined, and the live corpus is byte-identical.
/// Test: this IS the test.
#[tokio::test]
async fn a_rename_failure_quarantines_and_is_not_reported_as_a_deferral() {
    let (_dir, handle, live, tmp, live_before, outcome) =
        drive_promotion_over_a_readonly_parent("rename-fail-7991").await;

    assert!(
        !outcome.deferred,
        "#7991: a failed rename is a damaged run, not a clean deferral — it must \
         not be reported as PromotionDeferred"
    );
    let indexer = handle.indexer.read().await;
    assert!(
        indexer.promotion_deferred().is_none(),
        "#7991: only the promotion gate's refusal records a deferral"
    );
    assert!(
        indexer.corpus_open_failed,
        "#7920: a rename failure leaves nothing attached — it must quarantine"
    );
    assert!(!indexer.has_corpus_store());
    drop(indexer);
    assert_eq!(
        std::fs::read(&live).unwrap(),
        live_before,
        "#7991: a rename that never ran cannot have changed the live corpus"
    );
    assert!(
        tmp.exists(),
        "the staging file is still on disk — nothing renamed it away"
    );
    assert!(
        outcome.quarantined,
        "#7920: the promotion is what quarantined this index"
    );
}

/// #7920: a rename failure must not report the run as a completed reindex.
///
/// Why: `reindex_outcome` is computed BEFORE `resolve_corpus_swap` runs, and
/// nothing re-read the quarantine afterwards — so the rename/re-open failure
/// arm released the staging store, quarantined the index, and the run still
/// pushed `ReindexStatus::Complete` with a fresh `last_indexed_at` stamp and a
/// HEAD-SHA marker claiming the live corpus was current. Excluding only
/// `promotion_deferred` fixed half the problem.
/// What: drives the real promotion into a rename failure, then feeds the two
/// values `finish_reindex` computes into the real decision function and asserts
/// the verdict is neither `Complete` nor the deferral's status.
/// Test: this IS the test.
#[tokio::test]
async fn a_rename_failure_is_not_a_completed_reindex() {
    use super::finish::settled_promotion_status;
    use super::progress::ReindexStatus;

    let (_dir, _handle, _live, _tmp, _before, outcome) =
        drive_promotion_over_a_readonly_parent("rename-status-7920").await;

    assert_eq!(
        settled_promotion_status(outcome.deferred, outcome.quarantined),
        Some(ReindexStatus::Failed),
        "#7920: a promotion that quarantined the index cannot report Complete"
    );
    assert_eq!(
        settled_promotion_status(false, false),
        None,
        "a settled promotion still earns Complete — the gate must not fire always"
    );
}

/// What `finish_reindex` observes around `resolve_corpus_swap`.
///
/// Why: the terminal-status decision reads exactly these two values, so a test
/// that asserts on them is asserting on the real inputs rather than on a
/// restatement of them.
struct PromotionOutcome {
    /// `resolve_corpus_swap`'s return — the gate refused.
    deferred: bool,
    /// The index was NOT quarantined before the swap and IS after it.
    quarantined: bool,
}

/// Drive a real staged promotion whose rename cannot run (#7920, #7991).
///
/// Why: a read-only colocated parent refuses `rename(2)` on macOS and Linux
/// alike while leaving the already-created live and staging FILES openable, so
/// the lock gate admits the promotion and the rename is what fails. Two tests
/// assert on different halves of the same drive; duplicating the permission
/// dance would risk them drifting apart.
/// What: returns the tempdir (kept alive by the caller), the handle, the live
/// and staging paths, the live corpus's bytes from before the attempt, and the
/// two values `finish_reindex` reads. Permissions are restored before returning
/// so no assertion can leave an unremovable tempdir.
/// Test: `a_rename_failure_quarantines_and_is_not_reported_as_a_deferral`,
/// `a_rename_failure_is_not_a_completed_reindex`.
async fn drive_promotion_over_a_readonly_parent(
    index_id: &str,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::core::registry::IndexHandle>,
    std::path::PathBuf,
    std::path::PathBuf,
    Vec<u8>,
    PromotionOutcome,
) {
    use super::finish_teardown::resolve_corpus_swap;
    use super::{staging::StagingResolution, validate::ReindexOutcome};

    let (dir, handle, live, tmp) = staged_index(index_id);
    let colocated = live.parent().expect("colocated dir").to_path_buf();
    let live_before = std::fs::read(&live).unwrap();
    let quarantined_before = handle.indexer.read().await.is_write_quarantined();

    let original = std::fs::metadata(&colocated).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&colocated, readonly).unwrap();
    let deferred = resolve_corpus_swap(
        &handle,
        &handle.id,
        handle.root_path.as_path(),
        Some(tmp.as_path()),
        &StagingResolution::Commit,
        &ReindexOutcome::Ready,
        false,
        true,
    )
    .await;
    std::fs::set_permissions(&colocated, original).unwrap();

    // #7920: computed exactly as `finish_reindex` computes it.
    let quarantined = !quarantined_before && handle.indexer.read().await.is_write_quarantined();
    (
        dir,
        handle,
        live,
        tmp,
        live_before,
        PromotionOutcome {
            deferred,
            quarantined,
        },
    )
}

/// #7991: the doc's claim that a deferred run's staging is NOT adoptable.
///
/// Why: `commit_staged_corpus_swap` used to document a refusal as leaving the
/// staged corpus "for the next run to adopt". It does not: the refusal returns
/// before the release, so this process still holds `index.redb.tmp` open, and
/// the next run's `probe_resume` cannot open it and discards it. The doc was
/// corrected rather than the behaviour — detaching staging at the refusal would
/// put the index in the no-corpus state the post-release arms quarantine — so
/// this test pins the claim the corrected doc now makes.
/// What: stamps a checkpoint the probe WOULD adopt — otherwise the discard
/// proves only that the record was unparseable — defers a promotion for real,
/// then runs the next run's probe with the staging handle still held and asserts
/// it neither adopts nor keeps the file.
/// Test: this IS the test.
#[tokio::test]
async fn the_next_runs_probe_discards_a_deferred_runs_staging_corpus() {
    use super::checkpoint::{probe_resume, ReindexCheckpoint};
    use super::corpus_swap::commit_staged_corpus_swap;

    let (_dir, handle, live, tmp) = staged_index("deferred-resume-7991");
    let current = ReindexCheckpoint::for_run(&handle, &handle.id, handle.root_path.as_path());
    handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("staging store")
        .write_reindex_checkpoint_sync(&serde_json::to_vec(&current).expect("encode checkpoint"))
        .expect("stamp an adoptable checkpoint");

    let _holder = CorpusStore::open(&live).expect("hold the live corpus");
    assert!(
        !commit_staged_corpus_swap(&handle, &handle.id, &tmp).await,
        "test setup: a held live corpus must refuse the promotion"
    );
    assert!(
        tmp.exists() && handle.indexer.read().await.has_corpus_store(),
        "test setup: the refusal keeps the staging file AND its open handle"
    );

    let resumed = probe_resume(&handle, &handle.id, handle.root_path.as_path(), &current).await;

    assert!(
        resumed.is_none(),
        "#7991: a staging file this process still holds open is not adoptable"
    );
    assert!(
        !tmp.exists(),
        "#7991: the probe discards what it cannot adopt — the deferred run's work \
         is gone, which is exactly what the corrected doc says"
    );
}

/// An open failure other than absence cannot prove the file is unheld.
///
/// Why: the rung-5 requirement is that every undecidable branch falls to
/// refuse. A directory at the live path is the portable way to force an open
/// error that is neither `NotFound` nor a lock outcome.
/// Test: this IS the test.
#[tokio::test]
async fn an_unopenable_live_path_refuses_promotion() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("index.redb");
    std::fs::create_dir(&live).unwrap();
    assert!(
        acquire_for_promotion(&live, &id()).await.is_none(),
        "#7991: a live path this host cannot open is one it cannot prove is unheld"
    );
}
