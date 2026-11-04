//! Machine-local index state for the agent's OKG trees (#10746 S12).
//!
//! Why: SPEC-OKF-01 §4.9.1 — index state describes one machine's daemon, so
//! it must live outside the synced tree, and a second machine must read its
//! own (empty) state, not the first machine's.
//! What: drives `KbStore::okg_index_coverage_of` against real temp trees and
//! state directories, recording through `trusty_okf::state::IndexJournal`.
//! Test: `cargo test -p trusty-agents kb::okg::index_state`.

use super::*;
use crate::kb::okg::registry::Locator;
use trusty_okf::build::policy::DocStorePolicy;
use trusty_okf::profile::Profile;
use trusty_okf::state::{ByteHash, IndexJournal, IndexRecord};

/// A tree with two ingested doc-store entities, and its source spec.
fn fixture() -> (tempfile::TempDir, KbStore, SourceSpec) {
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
    let spec = store.okg_source("corpus").unwrap();
    (tmp, store, spec)
}

/// Record what a confirmed put and a passed probe of `path` would append.
fn confirm(store: &KbStore, state: &IndexStateRef, path: &str) {
    let bytes = ByteHash::of(&std::fs::read(store.root.join(path)).unwrap());
    let journal = IndexJournal::open(&state.dir, &store.root, &state.index_id).unwrap();
    journal
        .append(&[
            IndexRecord::PutAck {
                path: path.to_string(),
                bytes: bytes.as_str().to_string(),
                identity: None,
            },
            IndexRecord::Probe {
                path: path.to_string(),
                bytes: bytes.as_str().to_string(),
                identity: None,
                passed: true,
            },
        ])
        .unwrap();
}

fn coverage(store: &KbStore, spec: &SourceSpec, state: &IndexStateRef) -> (usize, usize) {
    let view = store.index_view(Some(state)).unwrap();
    let c = store.okg_index_coverage_of(spec, view.as_ref()).unwrap();
    (c.synced, c.pending)
}

/// Why: §3 (r) A — the agent passes a directory in its machine-local data
/// area. What: with the data area sandboxed, the state directory resolves
/// under it and never under a KB tree. Test: self-contained.
#[test]
fn machine_local_state_lives_in_the_data_area() {
    let _lock = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data = tempfile::tempdir().unwrap();
    let prior = std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV);
    // SAFETY: HOME_LOCK serialises every env mutation in this crate's tests.
    unsafe { std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, data.path()) };
    let state = IndexStateRef::machine_local("assistant-okg-x");
    // SAFETY: as above; restored before any assertion can unwind.
    unsafe {
        match prior {
            Some(v) => std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, v),
            None => std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV),
        }
    }
    let state = state.unwrap();
    assert_eq!(
        state.path(),
        data.path().join("trusty-agents").join(INDEX_STATE_SUBDIR)
    );
    assert_eq!(state.index_id, "assistant-okg-x");
}

/// Why: §4.9.1 rule 3 and §1.1 item 13 — nothing is synced until this
/// machine's daemon confirmed the bytes, and an edit re-enters the backlog.
/// What: a fresh ingest is (0, 2); a confirmed put and probe of one entity is
/// (1, 1); editing it is (0, 2) again; a state directory inside the tree is
/// refused. Test: self-contained.
#[test]
fn coverage_is_pending_until_this_machines_daemon_confirms() {
    let (tmp, store, spec) = fixture();
    let state = IndexStateRef::new(tmp.path().join("state"), "idx");
    assert_eq!(coverage(&store, &spec, &state), (0, 2));
    confirm(&store, &state, "notes/one.md");
    assert_eq!(coverage(&store, &spec, &state), (1, 1));
    std::fs::write(store.root.join("notes/one.md"), "edited by hand").unwrap();
    assert_eq!(
        coverage(&store, &spec, &state),
        (0, 2),
        "changed bytes are owed again"
    );
    let inside = IndexStateRef::new(store.root.join("state"), "idx");
    let err = store.index_view(Some(&inside)).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<trusty_okf::OkfError>(),
            Some(trusty_okf::OkfError::StateDirInBundle(_))
        ),
        "{err:?}"
    );
    let none = store.okg_index_coverage_of(&spec, None).unwrap();
    assert_eq!(
        (none.synced, none.pending),
        (0, 2),
        "no bound index: all pending"
    );
}

/// Why: the r4 BLOCKER 5 defect — two machines share one synced tree, and
/// neither may read the other's confirmations.
/// What: state A confirms both entities; state B over the same tree still
/// reads (0, 2), and A's journal sits outside the tree. Test: self-contained.
#[test]
fn two_state_dirs_over_one_tree_keep_separate_state() {
    let (tmp, store, spec) = fixture();
    let a = IndexStateRef::new(tmp.path().join("machine-a"), "idx");
    let b = IndexStateRef::new(tmp.path().join("machine-b"), "idx");
    confirm(&store, &a, "notes/one.md");
    confirm(&store, &a, "notes/two.md");
    assert_eq!(coverage(&store, &spec, &a), (2, 0));
    assert_eq!(
        coverage(&store, &spec, &b),
        (0, 2),
        "machine B never sees machine A's state"
    );
    let journal_a = IndexJournal::open(&a.dir, &store.root, "idx").unwrap();
    assert!(journal_a.path().starts_with(tmp.path().join("machine-a")));
    assert!(!journal_a.path().starts_with(&store.root));
}
