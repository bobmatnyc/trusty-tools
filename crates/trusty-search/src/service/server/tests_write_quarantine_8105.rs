//! Issue #8105 regression tests: `POST /indexes/{id}/reindex` must refuse a
//! write-quarantined index instead of queueing a run that can never land.
//!
//! Why: on 0.52.0 a reindex against a corpus-open-failed index answered
//! `queued: true`, walked 39 files, embedded 496 chunks, and then had every
//! durable write refused by the #4226 guard — leaving `chunk_count` null and
//! the caller polling a completion that cannot arrive. The run was doomed
//! before it started: by the #4122 invariant a quarantined index has no
//! `CorpusStore` wired, and only a successful `CorpusStore::open` lifts the
//! quarantine.
//!
//! What: two tests.
//!   1. `reindex_of_quarantined_index_is_rejected_with_reason` — the refusal
//!      itself, through the shared `reindex_report` body both transports use,
//!      asserting the status code, the machine-readable fields, and that
//!      nothing was queued (the "`chunk_count` stays null forever" half: no
//!      progress entry is created, so there is no completion to wait for).
//!   2. `a_healthy_index_is_not_write_quarantined` — the anti-over-refusal
//!      control. The gate reads `corpus_open_failed`, so an index whose corpus
//!      opened must pass it untouched.
//!
//! The quarantine is induced through a production path — a DIRECTORY where
//! `index.redb` belongs, the same portable technique
//! `tests/corpus_open_quarantine_4122.rs` uses — never by setting
//! `corpus_open_failed` by hand.
//!
//! Test: `cargo test -p trusty-search -- tests_write_quarantine_8105`

use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use tokio::sync::RwLock;
use trusty_common::embedder::MockEmbedder;

use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::reindex::quarantine::WriteQuarantined;

use super::reindex_handlers::reindex_report;
use super::state::SearchAppState;

/// Build a colocated `PersistedIndex` entry rooted at `root`.
fn entry_at(id: &str, root: &Path) -> PersistedIndex {
    let mut e = PersistedIndex::new(id.to_string(), root.to_path_buf());
    e.colocated = true;
    e
}

/// Put a DIRECTORY where `index.redb` belongs so `CorpusStore::open` fails
/// portably, which is what raises the write quarantine.
fn sabotage_corpus_path(root: &Path) {
    let colocated = root.join(".trusty-search");
    std::fs::create_dir_all(colocated.join("index.redb")).expect("create dir at redb path");
}

/// Register one index built from `root` and return the state plus its id.
async fn state_with_index(id: &str, root: &Path) -> (Arc<SearchAppState>, IndexId) {
    let embedder: Arc<dyn crate::core::Embedder> = Arc::new(MockEmbedder::new(8));
    let indexer = build_indexer_from_entry(&entry_at(id, root), &embedder)
        .await
        .expect("build indexer");
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    (Arc::new(SearchAppState::new(registry)), IndexId::new(id))
}

/// Issue #8105 — THE REFUSAL.
///
/// Why: accepting the request is what produced the unterminating poll. The
/// refusal must carry the same #4333 classification the status surface
/// reports, so a caller is told the cause and not merely "no".
/// What: drives `reindex_report` — the body both the HTTP handler and the
/// socket transport serve — against a write-quarantined index and asserts the
/// 409, the fields, and that nothing was queued.
/// Test: this IS the test. Against pre-fix code the call returns `Ok` with
/// `queued: true`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_of_quarantined_index_is_rejected_with_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    sabotage_corpus_path(root);
    let (state, index_id) = state_with_index("quarantined-8105", root).await;

    let handle = state.registry.get(&index_id).expect("registered");
    assert!(
        handle.indexer.read().await.corpus_open_failed,
        "precondition: a directory at the redb path must fail the corpus open"
    );

    let (status, body) = reindex_report(&state, &index_id.0, None)
        .await
        .expect_err("#8105: a write-quarantined index must not accept a reindex");

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "index_write_quarantined");
    assert_eq!(body["index_id"], index_id.0);
    assert_eq!(
        body["queued"], false,
        "the caller must be told nothing was queued"
    );
    assert_eq!(
        body["retryable"], false,
        "the quarantine outlives every retry a caller can make"
    );
    // The refusal must name the SAME cause `GET /indexes/{id}/status` reports,
    // verbatim — a caller that sees two wordings for one state cannot act on
    // either.
    let kind = handle
        .indexer
        .read()
        .await
        .corpus_open_failure
        .expect("the flag is set in lockstep with the classification");
    assert_eq!(body["failure_kind"], kind.label());
    assert_eq!(body["reason"], kind.stage_reason());
    assert!(
        state.reindex_progress.get(&index_id).is_none(),
        "#8105: nothing may be queued, so there is no completion left to poll for"
    );
}

/// The anti-over-refusal control: the gate reads the quarantine flag, not the
/// absence of a corpus.
///
/// Why: a gate that refused every index would be a worse outage than the one
/// it fixes. `WriteQuarantined::check` is the whole predicate, so asserting it
/// here proves the healthy path stays open without spawning a real walk.
/// What: builds an index whose colocated corpus opens cleanly and asserts the
/// check declines to refuse it.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthy_index_is_not_write_quarantined() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let (state, index_id) = state_with_index("healthy-8105", root).await;

    let handle = state.registry.get(&index_id).expect("registered");
    assert!(
        !handle.indexer.read().await.corpus_open_failed,
        "precondition: the control index must open its corpus cleanly"
    );
    assert!(
        WriteQuarantined::check(&index_id.0, &handle).await.is_none(),
        "a healthy index must pass the #8105 gate untouched"
    );
}
