//! The one-time legacy index-journal migration (#10746 S12).
//!
//! Why: the pre-S12 `_sources/<id>.index.jsonl` journal syncs with the tree
//! and may describe another machine's daemon (SPEC-OKF-01 §4.9.1). Its
//! entries must survive as this machine's Pending paths — never as
//! acknowledgements — and the migration must be idempotent and survive an
//! interruption.
//! What: migrates a fixture journal into a temp state directory and checks
//! the state journal, the backlog, the marker and the untouched legacy file.
//! Test: `cargo test -p trusty-agents kb::okg::legacy_journal`.

use super::*;
use crate::kb::okg::registry::{Locator, SourceSpec};
use crate::kb::store::KbStore;
use trusty_okf::build::policy::DocStorePolicy;
use trusty_okf::bundle::Bundle;
use trusty_okf::profile::Profile;
use trusty_okf::state::{LiveIdentity, TaskOp};

/// A tree with `notes/one.md` live and `notes/two.md` tombstoned, plus a
/// legacy journal that claims `one` and a vanished `gone` indexed, `two`
/// removed, one invalid entity and one torn final line.
fn fixture() -> (tempfile::TempDir, KbStore, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let store = KbStore::new(tmp.path().join("kb"), Profile::default_profile());
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("one.md"), "first note").unwrap();
    std::fs::write(corpus.join("two.md"), "second note").unwrap();
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
    let policy = DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()]);
    store
        .okg_ingest_docstore("corpus", &policy, "2026-07-24T00:00:00Z")
        .unwrap();
    std::fs::remove_file(corpus.join("two.md")).unwrap();
    store
        .okg_ingest_docstore("corpus", &policy, "2026-07-24T01:00:00Z")
        .unwrap();
    let one =
        trusty_okf::state::ByteHash::of(&std::fs::read(store.root.join("notes/one.md")).unwrap());
    let legacy = store.root.join("_sources/corpus.index.jsonl");
    let lines = [
        format!(r#"{{"entity":"notes/one","content_hash":"{}","size":1,"state":"indexed","at":"t0"}}"#, one.as_str()),
        r#"{"entity":"notes/gone","content_hash":"sha256:00","size":1,"state":"indexed","at":"t0"}"#.to_string(),
        r#"{"entity":"notes/two","content_hash":"sha256:11","size":1,"state":"indexed","at":"t0"}"#.to_string(),
        r#"{"entity":"notes/two","content_hash":"sha256:11","size":1,"state":"removed","at":"t1"}"#.to_string(),
        r#"{"entity":"../escape","content_hash":"sha256:22","size":1,"state":"indexed","at":"t0"}"#.to_string(),
        r#"{"entity":"notes/to"#.to_string(),
    ];
    std::fs::write(&legacy, lines.join("\n")).unwrap();
    (tmp, store, legacy)
}

fn state(tmp: &tempfile::TempDir) -> IndexStateRef {
    IndexStateRef::new(tmp.path().join("state"), "idx")
}

fn records(store: &KbStore, st: &IndexStateRef) -> Vec<IndexRecord> {
    IndexJournal::open(&st.dir, &store.root, &st.index_id)
        .unwrap()
        .read()
        .unwrap()
}

/// Paths named by more than one record (a duplicate migration).
fn duplicates(records: &[IndexRecord]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut dup = Vec::new();
    for r in records {
        if let IndexRecord::PutFailed { path, .. } = r
            && !seen.insert(path.clone())
        {
            dup.push(path.clone());
        }
    }
    dup
}

/// Why: §4.9.1 and the brief — a legacy entry keeps its path but never
/// becomes this machine's acknowledgement; the legacy file stays in place.
/// What: migrates the fixture; asserts three Pending records (one per
/// valid entity, last line wins, torn line and invalid entity counted), no
/// acknowledgement, the backlog re-pushes `one` and withdraws `two` and the
/// vanished `gone`, the legacy bytes are unchanged and a second call is a
/// no-op. Test: self-contained.
#[test]
fn migrates_a_fixture_journal_as_pending_and_leaves_it_in_place() {
    let (tmp, store, legacy) = fixture();
    let before = std::fs::read(&legacy).unwrap();
    let st = state(&tmp);
    let report = migrate_legacy_journals(&store.root, &st).unwrap();
    assert_eq!(report.journals, 1);
    assert_eq!(report.migrated, 3, "{report:?}");
    assert_eq!(report.malformed_lines, 1, "the torn line is skipped (F11)");
    assert_eq!(report.skipped.len(), 1, "../escape is not a concept id");
    let recs = records(&store, &st);
    assert_eq!(recs.len(), 3);
    for r in &recs {
        match r {
            IndexRecord::PutFailed { error, .. } => assert!(error.starts_with(LEGACY_REASON)),
            other => panic!("a migration never acknowledges: {other:?}"),
        }
    }
    let (bundle, _) = Bundle::open(&store.root).unwrap();
    let backlog = bundle
        .index_backlog(&st.dir, "idx", &LiveIdentity::Unsupported)
        .unwrap();
    assert_eq!(
        backlog.task("notes/one.md"),
        Some(TaskOp::Push { replace: false }),
        "this machine re-pushes; the legacy hash is not trusted"
    );
    assert_eq!(backlog.task("notes/two.md"), Some(TaskOp::Remove));
    assert_eq!(
        backlog.task("notes/gone.md"),
        Some(TaskOp::Remove),
        "a vanished path is still withdrawn"
    );
    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        before,
        "legacy file untouched"
    );

    let again = migrate_legacy_journals(&store.root, &st).unwrap();
    assert_eq!(
        (again.journals, again.already_migrated, again.migrated),
        (0, 1, 0)
    );
    assert_eq!(records(&store, &st).len(), 3, "idempotent");
}

/// Why: a migration can die between its journal append and its marker, or
/// halfway through the append. What: (1) a state journal already holding
/// `notes/one.md` plus a torn line, with no marker, gains only the missing
/// paths; (2) a marker that cannot be written fails the call after the
/// append, and the re-run after the fault clears appends nothing and writes
/// the marker. No path is ever journaled twice. Test: self-contained.
#[test]
fn interrupted_migration_reruns_without_duplicates() {
    let (tmp, store, _) = fixture();
    let st = state(&tmp);
    let journal = IndexJournal::open(&st.dir, &store.root, "idx").unwrap();
    journal
        .append(&[IndexRecord::PutFailed {
            path: "notes/one.md".into(),
            bytes: "sha256:00".into(),
            error: format!("{LEGACY_REASON}: earlier run"),
        }])
        .unwrap();
    let mut torn = std::fs::read(journal.path()).unwrap();
    torn.extend_from_slice(br#"{"kind":"put_failed","path":"notes/gone.md""#);
    std::fs::write(journal.path(), torn).unwrap();

    // The marker directory is a file: the append lands, the marker fails.
    std::fs::write(st.path().join(MARKER_DIR), b"not a directory").unwrap();
    assert!(migrate_legacy_journals(&store.root, &st).is_err());
    let recs = records(&store, &st);
    assert_eq!(recs.len(), 3, "one, gone and two, once each: {recs:?}");
    assert!(duplicates(&recs).is_empty(), "{recs:?}");

    std::fs::remove_file(st.path().join(MARKER_DIR)).unwrap();
    let rerun = migrate_legacy_journals(&store.root, &st).unwrap();
    assert_eq!((rerun.migrated, rerun.already_journaled), (0, 3));
    assert!(duplicates(&records(&store, &st)).is_empty());
    let done = migrate_legacy_journals(&store.root, &st).unwrap();
    assert_eq!(done.already_migrated, 1, "the marker now stands");
}

/// Why: fail closed — no error arm may leave an acknowledgement or a marker
/// that skips the work. What: a state directory inside the tree
/// (`StateDirInBundle`), an unreadable legacy journal, and a state journal
/// that cannot be created each return `Err`, write no marker and no
/// acknowledgement. Test: self-contained.
#[test]
fn migration_error_arms_record_nothing_as_indexed() {
    let (tmp, store, legacy) = fixture();

    let inside = IndexStateRef::new(store.root.join("state"), "idx");
    let err = migrate_legacy_journals(&store.root, &inside).unwrap_err();
    assert!(matches!(
        err.downcast_ref::<trusty_okf::OkfError>(),
        Some(trusty_okf::OkfError::StateDirInBundle(_))
    ));
    assert!(!store.root.join("state").exists());

    let st = state(&tmp);
    let unreadable = store.root.join("_sources/broken.index.jsonl");
    std::fs::create_dir_all(&unreadable).unwrap();
    assert!(migrate_legacy_journals(&store.root, &st).is_err());
    assert!(!st.path().join(MARKER_DIR).exists(), "no marker on error");
    std::fs::remove_dir(&unreadable).unwrap();

    let journal = IndexJournal::open(&st.dir, &store.root, "idx").unwrap();
    std::fs::create_dir_all(st.path()).unwrap();
    std::fs::write(journal.path().parent().unwrap(), b"occupied").unwrap();
    assert!(migrate_legacy_journals(&store.root, &st).is_err());
    assert!(!st.path().join(MARKER_DIR).exists(), "no marker on error");
    assert!(legacy.is_file());
}
