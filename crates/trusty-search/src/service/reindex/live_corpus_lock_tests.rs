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
//! `an_unopenable_live_path_refuses_promotion`.

use super::live_corpus_lock::acquire_for_promotion;
use crate::core::corpus::CorpusStore;
use crate::core::registry::IndexId;

fn id() -> IndexId {
    IndexId::new("live-lock-7991")
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
