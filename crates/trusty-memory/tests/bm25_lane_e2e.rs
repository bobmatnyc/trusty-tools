//! End-to-end proof that the in-process BM25 lane survives a process restart.
//!
//! Why: this file is `bm25_supervisor_e2e.rs`, reworked for #5329. The old test
//! spawned a real `trusty-bm25-daemon` child, indexed a document over its
//! socket, searched it, then asserted the supervisor reaped the child and
//! unlinked the socket. Two of those three claims were about a subprocess that
//! no longer exists. The one that was never about the subprocess — a document
//! indexed through the lane is still findable after everything is torn down and
//! rebuilt from disk — is what this file keeps, and it is the claim BM25 recall
//! actually rests on.
//!
//! It also drops the old file's `#[ignore]`. That existed because the test
//! needed a built daemon binary; this needs a tempdir.
//!
//! Test: this *is* the test file.

use trusty_memory::bm25_backfill::{backfill_palace, BackfillStatus, PalaceDocs};
use trusty_memory::bm25_index::SNAPSHOT_FILENAME;
use trusty_memory::bm25_lane::Bm25Lane;

/// Why: the restart path is the whole reason the snapshot exists. If a lane
/// wrote only to memory, every recall after a daemon restart would answer from
/// an empty corpus and report it as a normal empty result.
/// What: indexes two documents across two palaces, shuts the lane down, drops
/// it, builds a second lane over the same data root, and searches both palaces.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_corpus_survives_a_full_lane_restart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    {
        let lane = Bm25Lane::with_limits(root.clone(), 3, None);
        lane.index("alpha", "drawer-1", "quarterly rollout checklist")
            .await
            .expect("index alpha");
        lane.index("beta", "drawer-2", "incident postmortem notes")
            .await
            .expect("index beta");

        let hits = lane.search("alpha", "rollout", 5).await.expect("search");
        assert_eq!(hits.len(), 1, "the live corpus must answer: {hits:?}");
        assert_eq!(hits[0].doc_id, "drawer-1");

        lane.shutdown().await;
    }

    // Both snapshots must be on disk at the daemon-era path, so a downgrade
    // reads them and #5329's migration promise holds in both directions.
    for palace in ["alpha", "beta"] {
        let snapshot = root.join(palace).join("bm25").join(SNAPSHOT_FILENAME);
        assert!(
            snapshot.is_file(),
            "shutdown must leave a snapshot at {}",
            snapshot.display()
        );
    }

    let restarted = Bm25Lane::with_limits(root.clone(), 3, None);
    let alpha = restarted
        .search("alpha", "rollout", 5)
        .await
        .expect("search alpha after restart");
    assert_eq!(alpha.len(), 1, "alpha lost its corpus: {alpha:?}");
    assert_eq!(alpha[0].doc_id, "drawer-1");

    let beta = restarted
        .search("beta", "postmortem", 5)
        .await
        .expect("search beta after restart");
    assert_eq!(beta.len(), 1, "beta lost its corpus: {beta:?}");
    assert_eq!(beta[0].doc_id, "drawer-2");

    // And the corpora stayed separate across the restart.
    assert!(restarted
        .search("alpha", "postmortem", 5)
        .await
        .expect("cross-palace search")
        .is_empty());

    restarted.shutdown().await;
}

/// Why: an operator who set `TRUSTY_BM25_DAEMON=1` before #5329 has real
/// snapshots written by the retired subprocess, and the PR's central claim is
/// that they are read in place — no conversion step, no rebuild. This drives
/// that claim through the lane rather than the index type, so it covers the
/// path resolution as well as the parse.
/// What: hand-writes a daemon-era snapshot into `<root>/<palace>/bm25/`, then
/// asks a fresh lane to search it.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_era_snapshot_is_served_without_migration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    let bm25_dir = root.join("legacy").join("bm25");
    std::fs::create_dir_all(&bm25_dir).expect("create legacy bm25 dir");
    std::fs::write(
        bm25_dir.join(SNAPSHOT_FILENAME),
        br#"[{"doc_id":"legacy-drawer","text":"written by trusty-bm25-daemon"}]"#,
    )
    .expect("plant legacy snapshot");

    let lane = Bm25Lane::with_limits(root.clone(), 3, None);
    let hits = lane
        .search("legacy", "trusty-bm25-daemon", 5)
        .await
        .expect("search the legacy corpus");
    assert_eq!(
        hits.len(),
        1,
        "a snapshot written by the retired daemon must be served as-is: {hits:?}"
    );
    assert_eq!(hits[0].doc_id, "legacy-drawer");

    // Coverage over the legacy ids answers by identity, not by count.
    let cov = lane
        .missing_docs("legacy", &["legacy-drawer".to_string()])
        .await
        .expect("coverage probe");
    assert!(cov.missing.is_empty(), "got {cov:?}");

    lane.shutdown().await;
}

/// Why (#8246): making coverage content-aware must not turn a pre-existing
/// snapshot into a migration. A daemon-era snapshot records no freshness
/// revision — the retired daemon had no concept of one — and the rule this
/// change adopts is that an unknown revision means "re-index", which is exactly
/// the rule that could have demanded a full rebuild before a palace served
/// anything. It does not, because the snapshot's own `text` IS the revision:
/// the corpus is searchable the moment it loads, and only the entries the
/// palace has actually edited make the sweep do anything at all.
/// What: plants a daemon-era snapshot holding two drawers, searches it before
/// any backfill runs, then backfills with one drawer's text changed in place.
/// Asserts the run happened (the skip did not fire), the new text is findable
/// and the old text no longer matches.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_era_snapshot_without_revisions_serves_then_upgrades() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    let bm25_dir = root.join("legacy2").join("bm25");
    std::fs::create_dir_all(&bm25_dir).expect("create legacy bm25 dir");
    std::fs::write(
        bm25_dir.join(SNAPSHOT_FILENAME),
        br#"[{"doc_id":"stable","text":"quarterly rollout checklist"},
            {"doc_id":"edited","text":"zqxjoldbody staging runbook"}]"#,
    )
    .expect("plant legacy snapshot");

    let lane = Bm25Lane::with_limits(root.clone(), 3, None);

    // Searchable IMMEDIATELY — no migration, no mandatory rebuild first.
    let before = lane
        .search("legacy2", "zqxjoldbody", 5)
        .await
        .expect("search the legacy corpus");
    assert_eq!(
        before.len(),
        1,
        "a revision-less snapshot must serve as-is: {before:?}"
    );

    // The palace has since edited `edited` in place; `stable` is untouched.
    let docs = vec![
        (
            "stable".to_string(),
            "quarterly rollout checklist".to_string(),
        ),
        (
            "edited".to_string(),
            "zqxjnewbody staging runbook".to_string(),
        ),
    ];
    let report = backfill_palace(&lane, "legacy2", PalaceDocs::from_pairs(docs), false).await;
    assert_eq!(
        report.status,
        BackfillStatus::Completed,
        "the edit must defeat the already-indexed skip: {report:?}"
    );
    assert_eq!(report.indexed, 2, "the run must have fed the corpus");
    assert!(report.fully_indexed(), "{report:?}");

    let new_text = lane
        .search("legacy2", "zqxjnewbody", 5)
        .await
        .expect("search the upgraded corpus");
    assert_eq!(
        new_text
            .iter()
            .map(|h| h.doc_id.as_str())
            .collect::<Vec<_>>(),
        vec!["edited"],
        "the upgraded snapshot must answer for the NEW text: {new_text:?}"
    );
    let old_text = lane
        .search("legacy2", "zqxjoldbody", 5)
        .await
        .expect("search for the superseded text");
    assert!(
        old_text.is_empty(),
        "the superseded text must stop matching: {old_text:?}"
    );

    lane.shutdown().await;
}
