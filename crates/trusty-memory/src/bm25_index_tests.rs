//! Unit tests for [`PalaceBm25Index`](super::PalaceBm25Index).
//!
//! Why: these carry over the daemon crate's `index.rs` coverage (#5329) so the
//! collapse into trusty-memory changes where the index runs and nothing about
//! what it does. The one genuinely new test is
//! `snapshot_written_by_the_daemon_is_read_in_place`, which pins the migration
//! claim the PR rests on.

use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// The quarantine file `load_or_create` moved a corrupt snapshot to, if any.
///
/// Why: the name carries a wall-clock millisecond stamp, so a test cannot spell
/// it out; it can only look for the one entry carrying the prefix.
/// Test: used by `a_corrupt_snapshot_is_quarantined_before_the_next_flush_can_overwrite_it`.
fn quarantined_snapshots(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let prefix = format!("{SNAPSHOT_FILENAME}{CORRUPT_SUFFIX}");
    let mut found: Vec<_> = std::fs::read_dir(dir)
        .expect("read tempdir")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    found.sort();
    found
}

/// Why: this is the migration contract. #5329 promises an operator who set
/// `TRUSTY_BM25_DAEMON=1` keeps their corpus with no conversion step, and the
/// only way to prove it is to write the daemon's exact bytes and read them back
/// through the in-process loader.
/// What: writes a literal daemon-era snapshot — the same filename, the same
/// `[{"doc_id","text"}]` shape — then loads it and searches it.
/// Test: this test itself.
#[test]
fn snapshot_written_by_the_daemon_is_read_in_place() {
    let dir = tempdir();
    let snap = dir.path().join(SNAPSHOT_FILENAME);
    std::fs::write(
        &snap,
        br#"[{"doc_id":"drawer-a","text":"the quick brown fox"},
            {"doc_id":"drawer-b","text":"lazy dog sleeping"}]"#,
    )
    .unwrap();

    let idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    assert_eq!(idx.doc_count(), 2);
    assert!(!idx.is_dirty(), "a freshly loaded snapshot is not dirty");
    let hits = idx.search("fox", 10);
    assert_eq!(hits.len(), 1, "got: {hits:?}");
    assert_eq!(hits[0].doc_id, "drawer-a");
    assert!(
        idx.missing_docs(&["drawer-a".into(), "drawer-b".into()])
            .is_empty(),
        "both daemon-written documents must be present by id"
    );
}

/// Why: a round trip through this crate must stay readable by the format the
/// daemon defined, so a downgrade to the previous release is not a data loss.
/// What: writes with the in-process index, then re-parses the file as the
/// daemon's row shape.
/// Test: this test itself.
#[test]
fn flush_round_trips() {
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    idx.index_doc("x", "one two three");
    idx.flush().unwrap();

    let raw = std::fs::read_to_string(idx.snapshot_path()).unwrap();
    assert!(raw.contains("\"doc_id\":\"x\""), "got: {raw}");
    assert!(raw.contains("\"text\":\"one two three\""), "got: {raw}");

    let reopened = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    assert_eq!(reopened.doc_count(), 1);
    assert!(reopened.search("two", 5).iter().any(|h| h.doc_id == "x"));
}

#[test]
fn search_returns_hits() {
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    idx.index_doc("doc1", "authentication login password");
    idx.index_doc("doc2", "rendering ui components");
    let hits = idx.search("authentication", 5);
    assert_eq!(hits.len(), 1, "got: {hits:?}");
    assert_eq!(hits[0].doc_id, "doc1");
    assert!(hits[0].score > 0.0);
}

/// Why: `top_k = 0` from a misconfigured caller must not silently return
/// nothing, which would read exactly like an unindexed palace.
/// Test: this test itself.
#[test]
fn search_clamps_a_zero_top_k() {
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    idx.index_doc("doc1", "alpha");
    assert_eq!(idx.search("alpha", 0).len(), 1);
}

#[test]
fn index_doc_marks_dirty() {
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    assert!(!idx.is_dirty());
    idx.index_doc("d", "hello");
    assert!(idx.is_dirty());
    idx.flush().unwrap();
    assert!(!idx.is_dirty());
}

#[test]
fn delete_doc_removes_and_marks_dirty() {
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    idx.index_doc("d", "alpha beta");
    idx.flush().unwrap();
    assert!(idx.delete_doc("d"));
    assert!(idx.is_dirty());
    assert_eq!(idx.doc_count(), 0);
    assert!(idx.search("alpha", 5).is_empty());
    idx.flush().unwrap();
    assert!(!idx.delete_doc("never-existed"));
    assert!(!idx.is_dirty(), "an unknown-id delete must not re-dirty");
}

/// Why: `stats` is what separates "indexed, no lexical hits" from "not
/// indexed", so both figures must track mutation exactly — including the delete
/// path, where a stale byte total would overstate the corpus the lane budgets
/// memory against.
/// Test: this test itself.
#[test]
fn stats_track_docs_and_bytes() {
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    assert_eq!(idx.doc_count(), 0);
    assert_eq!(idx.total_text_bytes(), 0);

    idx.index_doc("a", "hello");
    idx.index_doc("b", "world!");
    assert_eq!(idx.doc_count(), 2);
    assert_eq!(idx.total_text_bytes(), 11);

    assert!(idx.delete_doc("a"));
    assert_eq!(idx.doc_count(), 1);
    assert_eq!(idx.total_text_bytes(), 6);
}

/// Why: this is the assertion that separates identity from counting. The index
/// below holds TWO documents and the caller asks about TWO ids, so every count
/// comparison reports full coverage. Only one of the two ids is present.
/// Test: this test itself.
#[test]
fn missing_docs_answers_by_identity_not_count() {
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    idx.index_doc("a", "alpha");
    idx.index_doc("stale", "a drawer that was deleted from the palace");

    let asked = vec!["a".to_string(), "b".to_string()];
    assert_eq!(
        idx.doc_count(),
        asked.len(),
        "precondition: the count comparison is satisfied and still wrong"
    );
    assert_eq!(idx.missing_docs(&asked), vec!["b".to_string()]);

    idx.index_doc("b", "beta");
    assert!(idx.missing_docs(&asked).is_empty());
    assert!(
        idx.missing_docs(&[]).is_empty(),
        "an empty request is trivially covered"
    );
}

/// Why: a corrupt snapshot must not stop recall from coming up — the lexical
/// lane degrades, the vector lane does not.
/// Test: this test itself.
#[test]
fn load_recovers_from_a_corrupt_snapshot() {
    let dir = tempdir();
    std::fs::write(dir.path().join(SNAPSHOT_FILENAME), b"not valid json").unwrap();
    let idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    assert_eq!(idx.doc_count(), 0);
}

/// Why (#5909): starting empty on a corrupt snapshot is only survivable while
/// the unparseable bytes stay reachable. They did not: `dirty` starts `false`,
/// but the first write flips it, and the next `flush` renamed a one-document
/// snapshot over the file holding the rest of the corpus. Nothing logged it and
/// nothing errored.
/// What: writes a snapshot that is truncated mid-array — unparseable, yet still
/// carrying both drawers' text — loads it, writes one unrelated document,
/// flushes, then asserts the original bytes are still on disk under the
/// quarantine name and the live snapshot holds only the new document.
/// Test: this test itself.
#[test]
fn a_corrupt_snapshot_is_quarantined_before_the_next_flush_can_overwrite_it() {
    const TRUNCATED: &[u8] = br#"[{"doc_id":"drawer-a","text":"the quick brown fox"},
        {"doc_id":"drawer-b","text":"lazy dog sleep"#;

    let dir = tempdir();
    std::fs::write(dir.path().join(SNAPSHOT_FILENAME), TRUNCATED).unwrap();

    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    assert_eq!(idx.doc_count(), 0, "the corrupt corpus is not readable");

    idx.index_doc("drawer-c", "a write that arrives after the failed load");
    idx.flush().expect("flush");

    let quarantined = quarantined_snapshots(dir.path());
    assert_eq!(
        quarantined.len(),
        1,
        "the corrupt snapshot must be moved aside, got: {quarantined:?}"
    );
    assert_eq!(
        std::fs::read(&quarantined[0]).unwrap(),
        TRUNCATED,
        "the quarantined file must hold the original bytes verbatim"
    );

    let live = std::fs::read_to_string(idx.snapshot_path()).unwrap();
    assert!(live.contains("drawer-c"), "got: {live}");
    assert!(
        !live.contains("drawer-a"),
        "the rebuilt snapshot is genuinely empty-plus-one — that is why the \
         quarantine copy is the only thing standing between this flush and the \
         corpus; got: {live}"
    );
}

/// Why (#5909): the quarantine rename is the whole guarantee. If it fails and
/// the load carries on anyway, the caller gets an index whose first flush
/// destroys exactly the file the rename existed to protect — a failure branch
/// that downgrades an error while state advances.
/// What: makes the palace directory unwritable so the rename cannot succeed,
/// asserts the load fails, and asserts the corrupt snapshot is untouched.
/// Test: this test itself.
#[test]
#[cfg(unix)]
fn an_unquarantinable_corrupt_snapshot_fails_the_load() {
    use std::os::unix::fs::PermissionsExt;

    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = tempdir();
    let snap = dir.path().join(SNAPSHOT_FILENAME);
    std::fs::write(&snap, b"not valid json").unwrap();

    // r-x: the snapshot is still readable, but nothing can be renamed or created.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = PalaceBm25Index::load_or_create(dir.path());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = result
        .err()
        .expect("a corrupt snapshot that cannot be moved aside must fail the load");
    assert!(
        format!("{err:#}").contains("quarantine corrupt BM25 snapshot"),
        "got: {err:#}"
    );
    assert_eq!(
        std::fs::read(&snap).unwrap(),
        b"not valid json",
        "the corrupt snapshot must survive a failed quarantine"
    );
    assert!(
        quarantined_snapshots(dir.path()).is_empty(),
        "no quarantine file can exist when the rename failed"
    );
}

/// Why: a snapshot that exists but cannot be READ is different from one that is
/// merely malformed — starting empty there would silently drop a corpus the
/// operator can still see on disk, and the next flush would overwrite it.
/// What: makes the snapshot unreadable and asserts the load fails rather than
/// returning an empty index.
/// Test: this test itself.
#[test]
#[cfg(unix)]
fn load_propagates_an_unreadable_snapshot() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir();
    let snap = dir.path().join(SNAPSHOT_FILENAME);
    std::fs::write(&snap, br#"[{"doc_id":"a","text":"alpha"}]"#).unwrap();
    std::fs::set_permissions(&snap, std::fs::Permissions::from_mode(0o000)).unwrap();

    let result = PalaceBm25Index::load_or_create(dir.path());

    // Restore before asserting so the tempdir can always be cleaned up.
    std::fs::set_permissions(&snap, std::fs::Permissions::from_mode(0o644)).unwrap();

    // Running as root defeats the permission bits entirely; skip rather than
    // assert something the environment cannot produce.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    assert!(
        result.is_err(),
        "an unreadable snapshot must propagate, not read as an empty corpus"
    );
}

/// Why: `flush` clears `dirty` only on success. If it cleared it regardless, a
/// transient write failure would be indistinguishable from a successful flush
/// and the tick that follows would skip the retry.
/// What: points the index at a directory it cannot write into, flushes, and
/// asserts the dirty bit survives.
/// Test: this test itself.
#[test]
#[cfg(unix)]
fn a_failed_flush_leaves_the_index_dirty() {
    use std::os::unix::fs::PermissionsExt;

    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = tempdir();
    let mut idx = PalaceBm25Index::load_or_create(dir.path()).unwrap();
    idx.index_doc("a", "alpha");

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = idx.flush();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err(), "a read-only directory must fail the flush");
    assert!(
        idx.is_dirty(),
        "a failed flush must leave the index dirty so the next tick retries"
    );
    idx.flush().expect("the retry must succeed");
    assert!(!idx.is_dirty());
}
