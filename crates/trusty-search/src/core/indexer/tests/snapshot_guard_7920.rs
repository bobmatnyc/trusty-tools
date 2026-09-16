//! #7920 regression tests: the `chunks.json` writers must never replace a
//! populated snapshot with an empty or foreign in-memory corpus.
//!
//! Why: the shutdown flush of an indexer whose in-memory corpus was empty
//! wrote `{"version":1,"chunks":[],"entities":[]}` over a 27 MB snapshot it had
//! never loaded. The #1711 guard covered `hnsw.usearch` only.
//! What: the refusal arms (empty, foreign-partial, incremental persister), the
//! legitimate-write arm a never-flush fix would fail, and the I/O error arm.
//! Test: `shutdown_flush_refuses_empty_corpus_over_populated_chunks_json`,
//! `chunks_added_after_load_are_persisted`.

use super::*;
use crate::core::indexer::SnapshotOverwriteRefused;
use std::collections::HashSet;

/// Puts an environment variable back the way the test found it.
struct RestoreEnv {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl RestoreEnv {
    fn capture(key: &'static str) -> Self {
        Self {
            key,
            previous: std::env::var_os(key),
        }
    }
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        // SAFETY: dropped inside the same #[serial] span that changed the var.
        match self.previous.take() {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// Write a populated snapshot at `path` through a legitimate owner.
async fn seed(path: &std::path::Path) -> HashSet<String> {
    let writer = make_indexer();
    for (id, file) in [("a", "src/a.rs"), ("b", "src/b.rs"), ("c", "src/c.rs")] {
        writer
            .add_chunk(raw(id, file, "fn seeded() {}"))
            .await
            .expect("seed chunk");
    }
    writer
        .save_chunks_to_disk(path)
        .await
        .expect("seed snapshot");
    ["a", "b", "c"].iter().map(|s| s.to_string()).collect()
}

fn snapshot_ids(path: &std::path::Path) -> HashSet<String> {
    let bytes = std::fs::read(path).expect("snapshot must exist");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("snapshot must parse");
    v["chunks"]
        .as_array()
        .expect("chunks array")
        .iter()
        .map(|c| c["id"].as_str().expect("chunk id").to_string())
        .collect()
}

/// #7920 verbatim: an empty corpus flushed over a populated snapshot.
#[tokio::test]
async fn shutdown_flush_refuses_empty_corpus_over_populated_chunks_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");
    seed(&path).await;
    let before = std::fs::read(&path).unwrap();

    let empty = make_indexer();
    let err = empty
        .flush_corpus_to_disk(&path)
        .await
        .expect_err("#7920: the flush must surface the refusal, not report success");
    assert!(
        err.downcast_ref::<SnapshotOverwriteRefused>().is_some(),
        "the error must be the overwrite refusal, got: {err:#}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "snapshot must be byte-identical"
    );
    assert_eq!(
        empty.refused_snapshot_overwrites(),
        1,
        "the refusal must be counted"
    );
}

/// A non-empty corpus that never came from this file is just as destructive.
#[tokio::test]
async fn shutdown_flush_refuses_foreign_partial_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");
    seed(&path).await;
    let before = std::fs::read(&path).unwrap();

    let foreign = make_indexer();
    foreign
        .add_chunk(raw("other", "src/other.rs", "fn elsewhere() {}"))
        .await
        .unwrap();
    let err = foreign
        .flush_corpus_to_disk(&path)
        .await
        .expect_err("must refuse");
    assert!(err.downcast_ref::<SnapshotOverwriteRefused>().is_some());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(foreign.refused_snapshot_overwrites(), 1);
}

/// The guard must not turn into never-flush: chunks added after a load land.
#[tokio::test]
async fn chunks_added_after_load_are_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");
    let mut expected = seed(&path).await;

    let owner = make_indexer();
    assert_eq!(owner.load_chunks_from_disk(&path).await.unwrap(), 3);
    owner
        .add_chunk(raw("d", "src/d.rs", "fn fresh() {}"))
        .await
        .unwrap();
    owner
        .flush_corpus_to_disk(&path)
        .await
        .expect("owned write must land");
    expected.insert("d".to_string());

    assert_eq!(snapshot_ids(&path), expected, "exact id set after flush");
    assert_eq!(owner.refused_snapshot_overwrites(), 0);
}

/// An unparseable file on disk must not block the writer that owns the path.
///
/// Why: `OnDisk::Unreadable` is refused like a populated snapshot, so the
/// question is whether a corrupt file can wedge the write path shut. It cannot
/// on the owning path — `check_overwrite` returns before it ever reads the
/// file when the indexer owns the path and holds a chunk.
#[tokio::test]
async fn corrupt_snapshot_does_not_block_the_owning_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");
    let mut expected = seed(&path).await;

    let owner = make_indexer();
    assert_eq!(owner.load_chunks_from_disk(&path).await.unwrap(), 3);
    // The file rots under the owner: truncated JSON, not a snapshot any more.
    std::fs::write(&path, b"{\"version\":1,\"chunks\":[{\"id\":").unwrap();
    owner
        .add_chunk(raw("d", "src/d.rs", "fn fresh() {}"))
        .await
        .unwrap();

    owner
        .flush_corpus_to_disk(&path)
        .await
        .expect("the owning writer must not be blocked by an unreadable file");
    expected.insert("d".to_string());

    assert_eq!(snapshot_ids(&path), expected, "exact id set after flush");
    assert_eq!(owner.refused_snapshot_overwrites(), 0);
}

/// Sidecar copies of an unreadable snapshot left beside `path` (#7980).
fn preserved_sidecars(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let prefix = format!("{}.corrupt-", path.file_name().unwrap().to_string_lossy());
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(path.parent().unwrap())
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
        })
        .collect();
    found.sort();
    found
}

/// #7980 admit arm: a store-less writer holding a real corpus must not be
/// wedged out of persisting by an unreadable file on an unowned path.
///
/// Why: the pre-#7920 reindex self-healed this — it simply rewrote the file.
/// The guard turned it into a permanent refusal, so a legacy index whose
/// `chunks.json` rotted could never persist again, and the bytes it was
/// protecting were ones no reader (including the migration) can recover.
/// What: writes an unparseable file at a path this indexer never loaded, adds
/// chunks, flushes, and asserts the write landed AND the unreadable bytes were
/// preserved under a sidecar rather than destroyed.
/// Test: this IS the test.
#[tokio::test]
async fn unreadable_snapshot_no_longer_wedges_a_store_less_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");
    let rotted = b"{\"version\":1,\"chunks\":[{\"id\":".to_vec();
    std::fs::write(&path, &rotted).unwrap();

    // Store-less, never quarantined, holds a real corpus it never loaded from
    // this file — the shape `refuse_durable_write` lets through.
    let writer = make_indexer();
    assert!(!writer.is_write_quarantined());
    writer
        .add_chunk(raw("a", "src/a.rs", "fn fresh() {}"))
        .await
        .unwrap();

    writer
        .flush_corpus_to_disk(&path)
        .await
        .expect("#7980: an unreadable file must not refuse a store-less writer forever");

    assert_eq!(
        snapshot_ids(&path),
        HashSet::from(["a".to_string()]),
        "the writer's own corpus must be on disk now"
    );
    assert_eq!(
        writer.refused_snapshot_overwrites(),
        0,
        "#7980: this shape must not be counted as a refusal"
    );
    let kept = preserved_sidecars(&path);
    assert_eq!(kept.len(), 1, "the unreadable bytes must be preserved once");
    assert_eq!(
        std::fs::read(&kept[0]).unwrap(),
        rotted,
        "#7923: the preserved copy must be byte-identical"
    );
}

/// #7980 refuse arm: a stand-in corpus must never move the file aside.
///
/// Why: the self-heal is admissible only because the writer's corpus IS the
/// index's durable state. A write-quarantined indexer's corpus is empty because
/// redb never opened, so moving the snapshot aside would remove the one
/// recovery source #4226 exists to protect. `refuse_durable_write` stops it
/// before the guard, and the guard refuses it again if it ever arrives.
/// What: asserts both — the quarantined flush leaves the file byte-identical
/// with no sidecar, and the guard itself refuses the stand-in shape directly.
/// Test: this IS the test.
#[tokio::test]
async fn a_quarantined_writer_never_reaches_the_unreadable_self_heal() {
    use crate::core::corpus::CorpusOpenFailure;
    use crate::core::indexer::snapshot_guard::{SnapshotGuard, WriterShape};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");
    let rotted = b"{\"version\":1,\"chunks\":[{\"id\":".to_vec();
    std::fs::write(&path, &rotted).unwrap();

    let mut quarantined = make_indexer();
    quarantined
        .add_chunk(raw("a", "src/a.rs", "fn fresh() {}"))
        .await
        .unwrap();
    quarantined.quarantine_detached_corpus(CorpusOpenFailure::Unclassified, "#7980 test");
    assert!(quarantined.is_write_quarantined());
    assert_eq!(
        quarantined.snapshot_writer_shape(),
        WriterShape::CorpusMayBeStandIn,
        "a quarantined indexer's corpus is never authoritative"
    );

    quarantined
        .flush_corpus_to_disk(&path)
        .await
        .expect("the quarantine guard abandons the write without erroring");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        rotted,
        "#4226: the snapshot must be byte-identical"
    );
    assert!(
        preserved_sidecars(&path).is_empty(),
        "#7980: a stand-in corpus must never move the snapshot aside"
    );

    // The guard's own arm, reached directly: the stand-in shape is refused
    // even with chunks in hand, and still nothing is moved.
    let guard = SnapshotGuard::default();
    guard
        .check_overwrite("direct", &path, 1, WriterShape::CorpusMayBeStandIn)
        .expect_err("#7980: the stand-in shape must still be refused");
    assert_eq!(guard.refused(), 1, "the refusal must be counted");
    assert!(preserved_sidecars(&path).is_empty());
}

/// Error arm: a failed write surfaces as `Err` and leaves the source intact.
#[tokio::test]
async fn snapshot_write_failure_surfaces_error_and_keeps_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");
    seed(&path).await;
    let before = std::fs::read(&path).unwrap();

    let owner = make_indexer();
    owner.load_chunks_from_disk(&path).await.unwrap();
    owner
        .add_chunk(raw("d", "src/d.rs", "fn fresh() {}"))
        .await
        .unwrap();
    // A directory where the temp file goes makes the write fail portably.
    std::fs::create_dir(path.with_extension("json.tmp")).unwrap();

    let err = owner
        .flush_corpus_to_disk(&path)
        .await
        .expect_err("a failed write must not report success");
    assert!(
        err.downcast_ref::<SnapshotOverwriteRefused>().is_none(),
        "this is the I/O arm, not the refusal arm: {err:#}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "source must be untouched"
    );
}

/// The detached incremental persister applies the same refusal.
#[tokio::test]
#[serial_test::serial]
async fn incremental_persist_refuses_empty_corpus_over_populated_chunks_json() {
    let data_dir = tempfile::tempdir().unwrap();
    // Declared after `data_dir`, so the variable is restored before the dir goes.
    let _restore = RestoreEnv::capture("TRUSTY_DATA_DIR");
    // SAFETY: #[serial] excludes every other #[serial] test for this test's span.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
    let index_id = "persist-7920";
    let path = crate::service::persistence::chunks_path(index_id).expect("chunks_path");
    seed(&path).await;
    let before = std::fs::read(&path).unwrap();

    let idx = CodeIndexer::new(index_id, "/tmp/persist-7920-root");
    idx.force_incremental_persist();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while idx.refused_snapshot_overwrites() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "#7920: the incremental persister never refused the overwrite"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
