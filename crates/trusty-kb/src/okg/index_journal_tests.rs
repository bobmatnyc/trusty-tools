//! Reconcile-layer tests for the OKG index journal (#3892).
//!
//! Why: the whole point of the journal is that the tree and the bound search
//! index converge WITHOUT the ledger ever depending on a reachable daemon.
//! Every property that claim rests on is proved here, with no network and no
//! daemon: a fresh ingest is owed to the index, a recorded push leaves the
//! backlog, an edit re-enters it, a tombstone becomes a withdrawal, and a
//! crash-torn journal line costs one record rather than the file.
//! What: drives `KbStore::okg_index_backlog` against a real temp tree.
//! Test: `cargo test -p trusty-kb okg::index_journal`.

use super::*;
use crate::okg::policy::DocStorePolicy;
use crate::okg::registry::Locator;
use crate::schema::Profile;

/// A store plus a doc-store corpus directory under one tempdir.
fn fixture() -> (tempfile::TempDir, KbStore, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let store = KbStore::new(tmp.path().join("kb"), Profile::default_profile());
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    (tmp, store, corpus)
}

fn policy(tmp: &tempfile::TempDir) -> DocStorePolicy {
    DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()])
}

/// Register a doc-store source over `corpus`, with tombstoning on.
fn register(store: &KbStore, corpus: &std::path::Path) -> SourceSpec {
    let mut spec = SourceSpec::new(
        "corpus",
        Some("notes"),
        Locator::DocStore {
            path: corpus.to_string_lossy().to_string(),
            extensions: vec![],
            recursive: true,
        },
        "t0",
    );
    spec.tombstone_deleted = true;
    store.okg_register_source(spec).unwrap();
    store.okg_source("corpus").unwrap()
}

/// Simulate a successful push of every task in the backlog.
fn drain(store: &KbStore, spec: &SourceSpec, now: &str) -> usize {
    let backlog = store.okg_index_backlog(spec).unwrap();
    let mut journal = IndexJournal::load(&store.root, &spec.id).unwrap();
    for task in &backlog.tasks {
        store.okg_record_index(&mut journal, task, now).unwrap();
    }
    backlog.tasks.len()
}

/// Why: this is the #3892 failure in miniature — an ingest that reports N
/// entities must leave exactly N items owed to the index, not zero.
/// What: ingests two files and asserts both appear as first-time push tasks
/// resolved to real, readable entity files.
/// Test: self-contained.
#[test]
fn backlog_lists_freshly_ingested_entities() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    std::fs::write(corpus.join("two.md"), "second note").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();

    let backlog = store.okg_index_backlog(&spec).unwrap();
    assert_eq!(
        backlog.tasks.len(),
        2,
        "both entities are owed to the index"
    );
    assert_eq!(backlog.synced, 0);
    assert!(backlog.notes.is_empty(), "notes: {:?}", backlog.notes);
    for task in &backlog.tasks {
        assert_eq!(task.op, IndexOp::Index { replace: false });
        assert!(task.path.is_file(), "{} must exist", task.path.display());
        assert!(task.content_hash.starts_with("sha256:"));
        let bytes = std::fs::read(&task.path).unwrap();
        assert_eq!(
            task.content_hash,
            format!("sha256:{:x}", sha2::Sha256::digest(&bytes)),
            "the task must pin the CURRENT file content"
        );
    }
}

/// Why: idempotency — re-running an unchanged source must push nothing, or
/// every run would re-embed the whole corpus and duplicate index work.
/// What: drains the backlog, then asserts a second reconcile (and one after a
/// full re-ingest that skips everything) is empty.
/// Test: self-contained.
#[test]
fn recorded_entity_leaves_the_backlog() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();
    assert_eq!(drain(&store, &spec, "t0"), 1);

    let after = store.okg_index_backlog(&spec).unwrap();
    assert!(after.tasks.is_empty(), "tasks: {:?}", after.tasks);
    assert_eq!(after.synced, 1);

    // A full re-ingest skips the item entirely; the index must stay settled.
    let report = store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
        .unwrap();
    assert_eq!(report.skipped, 1);
    assert!(store.okg_index_backlog(&spec).unwrap().tasks.is_empty());
}

/// Why: a push that never happened (daemon down, HTTP error, crash between the
/// entity write and the push) must be RETRIED, not silently skipped — the
/// convergence guarantee this design exists for.
/// What: ingests, deliberately records nothing, re-runs the ingest (which skips
/// the unchanged item), and asserts the item is still owed to the index.
/// Test: self-contained.
#[test]
fn unrecorded_push_stays_pending_across_runs() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();

    // Simulate a run whose push failed: the ledger has the item, the journal
    // does not.
    assert_eq!(store.okg_index_backlog(&spec).unwrap().tasks.len(), 1);
    let second = store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
        .unwrap();
    assert_eq!(second.skipped, 1, "the ingest engine skips it — by design");

    let backlog = store.okg_index_backlog(&spec).unwrap();
    assert_eq!(
        backlog.tasks.len(),
        1,
        "an unindexed item must never fall out of the backlog"
    );
    assert_eq!(drain(&store, &spec, "t1"), 1, "and a later run drains it");
    assert!(store.okg_index_backlog(&spec).unwrap().tasks.is_empty());
}

/// Why: a changed entity must REPLACE its index content. The old chunks have to
/// be withdrawn first (trusty-search chunks by content, so a shorter revision
/// would leave orphans searchable), which is what `replace: true` signals.
/// What: edits the source file, re-ingests, and asserts the task reappears as a
/// replacement carrying the NEW hash.
/// Test: self-contained.
#[test]
fn changed_entity_reenters_the_backlog() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();
    drain(&store, &spec, "t0");
    let before = store.okg_index_backlog(&spec).unwrap();
    assert!(before.tasks.is_empty());

    std::fs::write(corpus.join("one.md"), "first note, revised").unwrap();
    let report = store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
        .unwrap();
    assert_eq!(report.updated, 1);

    let backlog = store.okg_index_backlog(&spec).unwrap();
    assert_eq!(backlog.tasks.len(), 1);
    assert_eq!(
        backlog.tasks[0].op,
        IndexOp::Index { replace: true },
        "the stale revision must be withdrawn before the new one lands"
    );
    let content = std::fs::read_to_string(&backlog.tasks[0].path).unwrap();
    assert!(content.contains("revised"));
}

/// Why: a vanished source item must not stay searchable — that is the same
/// silent-lie failure as #3892, only inverted.
/// What: deletes the file, re-ingests (tombstoning), and asserts a withdrawal
/// task appears and settles once recorded.
/// Test: self-contained.
#[test]
fn tombstoned_entity_becomes_a_remove_task() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();
    drain(&store, &spec, "t0");

    std::fs::remove_file(corpus.join("one.md")).unwrap();
    let report = store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
        .unwrap();
    assert_eq!(report.tombstoned, 1);

    let backlog = store.okg_index_backlog(&spec).unwrap();
    assert_eq!(backlog.tasks.len(), 1);
    assert_eq!(backlog.tasks[0].op, IndexOp::Remove);

    assert_eq!(drain(&store, &spec, "t1"), 1);
    let settled = store.okg_index_backlog(&spec).unwrap();
    assert!(settled.tasks.is_empty(), "withdrawal settles");
    assert_eq!(settled.synced, 1);
}

/// Why: withdrawing something that was never pushed would make every offline
/// ingest emit spurious `remove_file` calls against the daemon.
/// What: ingests and tombstones WITHOUT ever recording a push, then asserts the
/// backlog is empty rather than holding a phantom removal.
/// Test: self-contained.
#[test]
fn never_indexed_tombstone_needs_no_removal() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();
    std::fs::remove_file(corpus.join("one.md")).unwrap();
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
        .unwrap();

    let backlog = store.okg_index_backlog(&spec).unwrap();
    assert!(
        backlog.tasks.is_empty(),
        "nothing was ever pushed, so nothing is owed: {:?}",
        backlog.tasks
    );
}

/// Why: the index journal inherits the ledger's crash tolerance — a `kill -9`
/// mid-append must cost one record, not the file. Without this the source would
/// re-push its whole corpus on every subsequent run.
/// What: tears the final line, then asserts the intact record still counts as
/// synced, the damage is counted, and a later append is not swallowed.
/// Test: self-contained.
#[test]
fn torn_index_journal_line_is_skipped_not_fatal() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    std::fs::write(corpus.join("two.md"), "second note").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();
    drain(&store, &spec, "t0");

    let path = IndexJournal::path_for(&store.root, "corpus").unwrap();
    let mut torn = std::fs::read_to_string(&path).unwrap();
    torn.push_str("{\"entity\":\"notes/thr");
    std::fs::write(&path, torn).unwrap();

    let journal = IndexJournal::load(&store.root, "corpus").unwrap();
    assert_eq!(journal.malformed_lines(), 1);
    let backlog = store.okg_index_backlog(&spec).unwrap();
    assert!(
        backlog.tasks.is_empty(),
        "the two intact records survive the tear: {:?}",
        backlog.tasks
    );

    // And the journal keeps working: a new entity records cleanly.
    std::fs::write(corpus.join("three.md"), "third note").unwrap();
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
        .unwrap();
    assert_eq!(drain(&store, &spec, "t1"), 1);
    assert!(store.okg_index_backlog(&spec).unwrap().tasks.is_empty());
}

/// Why: a status view that re-hashed every entity on every read would make the
/// GUI's store card quadratically expensive on a large tree. The cheap
/// `(size, mtime)` short-circuit is what keeps a settled reconcile to one
/// `stat` per entity — but it must never mask a real edit.
/// What: proves an edit that PRESERVES the byte length is still detected (mtime
/// moves), and that a byte-identical rewrite settles back to "synced" rather
/// than looping (the hash absorbs the false positive).
/// Test: self-contained.
#[test]
fn same_length_edit_is_still_detected() {
    let (tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("one.md"), "aaaa").unwrap();
    let spec = register(&store, &corpus);
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t0")
        .unwrap();
    drain(&store, &spec, "t0");

    std::fs::write(corpus.join("one.md"), "bbbb").unwrap();
    store
        .okg_ingest_docstore("corpus", &policy(&tmp), "t1")
        .unwrap();
    let backlog = store.okg_index_backlog(&spec).unwrap();
    assert_eq!(
        backlog.tasks.len(),
        1,
        "a same-length edit must still re-index"
    );
    drain(&store, &spec, "t1");

    // Touch the entity file without changing a byte: the cheap check fires,
    // the hash absorbs it, and nothing is re-pushed.
    let entity = store.entity_path("notes", "one").unwrap();
    let bytes = std::fs::read(&entity).unwrap();
    std::fs::write(&entity, &bytes).unwrap();
    let settled = store.okg_index_backlog(&spec).unwrap();
    assert!(
        settled.tasks.is_empty(),
        "an identical rewrite must not re-push: {:?}",
        settled.tasks
    );
}
