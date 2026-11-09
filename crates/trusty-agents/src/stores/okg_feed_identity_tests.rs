//! Offline reads after a daemon identity change (#10876).
//!
//! Why: a re-created index or a generation bump (F14) leaves every older
//! acknowledgement stale. A pass that reads the new identity and then fails
//! to re-push leaves only an in-memory error behind; a fresh process reading
//! offline must still report the path pending and blocking.
//! What: drives `feed_source` against the fake daemon of
//! `okg_feed_tests.rs`, then reads through a NEW `KbStore` over the same
//! tree and state directory, so no pass error is in memory.
//! Test: `cargo test -p trusty-agents stores::okg_feed::identity_tests`.

use trusty_okf::build::policy::DocStorePolicy;
use trusty_okf::profile::Profile;

use super::tests::{Fake, fixture};
use super::*;
use crate::kb::okg::IndexCoverage;

/// One offline read by a fresh process: the coverage and the blocking paths.
fn offline(
    root: &std::path::Path,
    spec: &SourceSpec,
    st: &IndexStateRef,
) -> (IndexCoverage, Vec<String>) {
    let fresh = KbStore::new(root.to_path_buf(), Profile::default_profile());
    let view = fresh.index_view(Some(st)).unwrap();
    let cov = fresh.okg_index_coverage_of(spec, view.as_ref()).unwrap();
    let mut blocking = fresh
        .source_index_state(spec, view.as_ref(), false)
        .unwrap()
        .blocking;
    blocking.sort();
    (cov, blocking)
}

/// A daemon with identity `(instance, generation)`.
fn daemon(root: &std::path::Path, instance: &str, generation: u64) -> Fake {
    let mut f = Fake::new(root);
    f.identity = Some((instance.into(), generation));
    f
}

/// A daemon that refuses every call.
fn down(root: &std::path::Path) -> Fake {
    let mut f = Fake::new(root);
    f.down = true;
    f
}

/// Asserts the offline read owes `pending` and blocks exactly `blocking`,
/// with nothing `synced`.
fn assert_offline(
    root: &std::path::Path,
    spec: &SourceSpec,
    st: &IndexStateRef,
    pending: usize,
    unconfirmed: usize,
    blocking: &[&str],
    ctx: &str,
) {
    let (cov, blk) = offline(root, spec, st);
    assert_eq!(
        (cov.synced, cov.unconfirmed, cov.pending),
        (0, unconfirmed, pending),
        "{ctx}: {cov:?}"
    );
    assert_eq!(blk, blocking, "{ctx}");
}

/// Why: the core #10876 defect — an identity change seen by a pass that
/// then pushed nothing read as settled offline.
/// What: pass 1 under `i1` settles both entities. Pass 2 reads a new
/// instance (`i2`) or a generation bump (`i1`, 2) and confirms nothing: it
/// refuses the put of `notes/one.md` and the old-revision remove of
/// `notes/two.md`. A fresh offline read reports both pending and blocking,
/// and still does after a later pass with the daemon down.
/// Test: self-contained.
#[tokio::test]
async fn identity_change_makes_offline_read_pending() {
    for (instance, generation) in [("i2", 1), ("i1", 2)] {
        let ctx = format!("{instance}/{generation}");
        let (tmp, store, spec) = fixture();
        let st = IndexStateRef::new(tmp.path().join("state"), "idx");
        let r1 = feed_source(&store, &spec, &st, &daemon(&store.root, "i1", 1)).await;
        assert!(r1.acknowledged("idx"), "{ctx}: {r1:?}");
        assert_offline(&store.root, &spec, &st, 0, 2, &[], &ctx);

        let mut changed = daemon(&store.root, instance, generation);
        changed.refuse = Some("notes/one.md".into());
        changed.refuse_remove = Some("notes/two.md".into());
        let r2 = feed_source(&store, &spec, &st, &changed).await;
        assert_eq!((r2.indexed, r2.removed), (0, 0), "{ctx}: {r2:?}");
        let both = ["notes/one.md", "notes/two.md"];
        assert_offline(&store.root, &spec, &st, 2, 0, &both, &ctx);

        let r3 = feed_source(&store, &spec, &st, &down(&store.root)).await;
        assert!(r3.index.is_none(), "{ctx}: not fed: {r3:?}");
        assert_offline(&store.root, &spec, &st, 2, 0, &both, &ctx);
    }
}

/// Pass 1 under `i1`, then pass 2 under `i2` with `notes/one.md` refused:
/// its put (`refuse_put`) or its old-revision remove.
async fn refused_after_change(
    refuse_put: bool,
) -> (tempfile::TempDir, KbStore, SourceSpec, IndexStateRef) {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let r1 = feed_source(&store, &spec, &st, &daemon(&store.root, "i1", 1)).await;
    assert!(r1.acknowledged("idx"), "{r1:?}");
    let mut second = daemon(&store.root, "i2", 1);
    if refuse_put {
        second.refuse = Some("notes/one.md".into());
    } else {
        second.refuse_remove = Some("notes/one.md".into());
    }
    let r2 = feed_source(&store, &spec, &st, &second).await;
    assert!(!r2.acknowledged("idx"), "{r2:?}");
    (tmp, store, spec, st)
}

/// Why: a refused re-push after F14 leaves the `i1` acknowledgement at
/// matching bytes; only the pass's in-memory error blocked it.
/// What: offline, `notes/one.md` is pending and blocking, `notes/two.md`
/// (re-pushed under `i2`) is unconfirmed; unchanged after a down pass.
/// Test: self-contained.
#[tokio::test]
async fn refused_repush_leaves_offline_read_pending() {
    let (_tmp, store, spec, st) = refused_after_change(true).await;
    assert_offline(
        &store.root,
        &spec,
        &st,
        1,
        1,
        &["notes/one.md"],
        "refused put",
    );
    feed_source(&store, &spec, &st, &down(&store.root)).await;
    assert_offline(
        &store.root,
        &spec,
        &st,
        1,
        1,
        &["notes/one.md"],
        "after down",
    );
}

/// Why: the same shape for a refused remove of the old revision, which
/// stops the re-push before its put.
/// What: as `refused_repush_leaves_offline_read_pending`.
/// Test: self-contained.
#[tokio::test]
async fn refused_old_revision_remove_leaves_offline_read_pending() {
    let (_tmp, store, spec, st) = refused_after_change(false).await;
    assert_offline(
        &store.root,
        &spec,
        &st,
        1,
        1,
        &["notes/one.md"],
        "refused remove",
    );
    feed_source(&store, &spec, &st, &down(&store.root)).await;
    assert_offline(
        &store.root,
        &spec,
        &st,
        1,
        1,
        &["notes/one.md"],
        "after down",
    );
}

/// Why: a withdrawal confirmed under `i1` says nothing about `i2`, which
/// may serve the path again.
/// What: `notes/two.md` is tombstoned and withdrawn under `i1` (it reads
/// settled offline); a pass under `i2` refuses its remove. Offline, the
/// path is pending and blocking. Test: self-contained.
#[tokio::test]
async fn withdrawn_path_after_identity_change_reads_pending_offline() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    feed_source(&store, &spec, &st, &daemon(&store.root, "i1", 1)).await;
    std::fs::remove_file(tmp.path().join("corpus/two.md")).unwrap();
    let policy = DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()]);
    store
        .okg_ingest_docstore("corpus", &policy, "2026-07-25T00:00:00Z")
        .unwrap();
    let r = feed_source(&store, &spec, &st, &daemon(&store.root, "i1", 1)).await;
    assert!(r.acknowledged("idx"), "withdrawn under i1: {r:?}");
    assert_offline(&store.root, &spec, &st, 0, 2, &[], "withdrawn");

    let mut second = daemon(&store.root, "i2", 1);
    second.refuse_remove = Some("notes/two.md".into());
    feed_source(&store, &spec, &st, &second).await;
    assert_offline(&store.root, &spec, &st, 1, 1, &["notes/two.md"], "i2");
}

/// Why: the observation must not make an unchanged daemon owe work, and an
/// offline read is never `synced`.
/// What: two passes under one identity; the offline read reports both
/// unconfirmed with nothing pending, and the journal holds one `observed`
/// line. Test: self-contained.
#[tokio::test]
async fn unchanged_identity_offline_read_is_unconfirmed_not_pending() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    for _ in 0..2 {
        let r = feed_source(&store, &spec, &st, &daemon(&store.root, "i1", 1)).await;
        assert!(r.acknowledged("idx"), "{r:?}");
    }
    assert_offline(&store.root, &spec, &st, 0, 2, &[], "unchanged");
    let journal = IndexJournal::open(&st.dir, &store.root, "idx").unwrap();
    let text = std::fs::read_to_string(journal.path()).unwrap();
    let observed = text
        .lines()
        .filter(|l| l.contains(r#""kind":"observed""#))
        .count();
    assert_eq!(observed, 1, "{text}");
}
