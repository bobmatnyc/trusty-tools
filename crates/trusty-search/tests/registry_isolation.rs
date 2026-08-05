//! The operator's live `indexes.toml` must be unreachable from a test process
//! (issue #4255).
//!
//! Why: this file lives in `tests/`, so it links the library built WITHOUT
//! `cfg(test)` — exactly the linkage issue #4094's compile-time guard did not
//! cover, and exactly the linkage that let a test register a throwaway fixture
//! root in the operator's registry. A unit test in `src/` cannot prove this,
//! because there `cfg(test)` is set and the old guard already applied. Placing
//! the proof here is the point of the file.
//!
//! Why these assertions and not a mock: the failure being guarded against is
//! "a real write reached a real path". Asserting against an injected path
//! would pass even with the guard removed, so every assertion below resolves
//! the operator's real location independently of the code under test.
//!
//! Test: the three tests in this file.

use std::path::PathBuf;
use trusty_search::service::persistence::{
    indexes_toml_path, load_index_registry_at, upsert_index_registry_entry, PersistedIndex,
};

/// The operator's real `indexes.toml`, resolved WITHOUT going through the code
/// under test.
///
/// Why: if this called `indexes_toml_path()` the comparison would be a
/// tautology. Duplicating the well-known layout here is deliberate — it is the
/// independent oracle the isolation is measured against.
/// What: `dirs::data_local_dir()/trusty-search/indexes.toml`, falling back to
/// the `$HOME`-relative platform path (mirrors `service::data_dir`'s issue
/// #718 fallback).
/// Test: used by every test below.
fn production_indexes_toml() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| {
        let home = dirs::home_dir().expect("HOME must be set to run these tests");
        if cfg!(target_os = "macos") {
            home.join("Library").join("Application Support")
        } else {
            home.join(".local").join("share")
        }
    });
    base.join("trusty-search").join("indexes.toml")
}

/// The detector must fire in this binary — the linkage that used to slip
/// through.
///
/// Why: everything else here depends on it. If this fails, the isolation is
/// not merely weakened, it is absent, and the more specific failures below
/// would be confusing to diagnose.
/// What: asserts `running_under_test_harness()` is true from an integration
/// test process, where `cfg!(test)` is false for the library.
/// Test: this test.
#[test]
fn integration_test_process_is_detected_as_a_test_harness() {
    assert!(
        trusty_common::running_under_test_harness(),
        "an integration test binary must be detected as a test harness; \
         without this every other guard in issue #4255 is inert"
    );
}

/// The registry path a test resolves must not be the operator's.
#[test]
fn registry_path_is_not_the_production_registry() {
    let resolved = indexes_toml_path().expect("indexes_toml_path must resolve");
    assert_ne!(
        resolved,
        production_indexes_toml(),
        "a test process resolved the operator's live registry (issue #4255)"
    );
}

/// A test that actually registers an index must leave the operator's registry
/// byte-for-byte untouched.
///
/// Why: this is the regression itself, not a proxy for it. It performs the
/// exact misbehaviour issue #4255 reports — a real `upsert_index_registry_entry`
/// with a fixture root — and pins the consequence. With the runtime guard in
/// `persistence::default_data_dir` removed, the write lands in the operator's
/// `indexes.toml` and this test fails.
/// What: snapshots the production file's bytes, upserts a fixture entry,
/// asserts the production bytes are unchanged (or that the file still does not
/// exist), and asserts the entry did land in the isolated registry — so the
/// test cannot pass by the write silently failing.
/// Test: this test.
#[test]
fn registering_an_index_never_writes_to_the_production_registry() {
    let production = production_indexes_toml();
    let before = std::fs::read(&production).ok();

    let id = format!("ts-4255-isolation-{}", std::process::id());
    let entry = PersistedIndex {
        id: id.clone(),
        root_path: PathBuf::from("/nonexistent/ts-4255-fixture-root"),
        ..Default::default()
    };
    upsert_index_registry_entry(entry).expect("upsert must succeed against the isolated registry");

    let after = std::fs::read(&production).ok();
    assert_eq!(
        before,
        after,
        "registering an index from a test mutated the operator's live registry at {} \
         (issue #4255)",
        production.display()
    );

    // The write must have actually happened somewhere — otherwise this test
    // would also pass if `upsert` had become a no-op.
    let isolated = indexes_toml_path().expect("indexes_toml_path must resolve");
    let registry = load_index_registry_at(&isolated).expect("isolated registry must load");
    assert!(
        registry.iter().any(|i| i.id == id),
        "the fixture entry must land in the isolated registry at {}",
        isolated.display()
    );
}
