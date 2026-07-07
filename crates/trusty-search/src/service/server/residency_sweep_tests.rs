//! Tests for `run_residency_sweep_tick` — the per-tick orchestration of the
//! usage-based resident-index cap (issue #2161).
//!
//! Why: `service::lazy_loader::residency::tests` covers the pure
//! selection/detach logic against bare structures with no `SearchAppState`.
//! This file covers the orchestration layer itself: reading `indexes.toml`,
//! cross-referencing it against the live registry, and the reindex-in-progress
//! guard — all of which require a real `SearchAppState`.
//!
//! Isolation: every test overrides `TRUSTY_DATA_DIR` to a private tempdir
//! (mirrors `warm_boot_tests.rs`) and is `#[serial_test::serial]` because
//! `TRUSTY_DATA_DIR` / `TRUSTY_MAX_RESIDENT_INDEXES` are shared process env
//! state.
//!
//! Test: `cargo test -p trusty-search -- residency_sweep`

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::{save_index_registry_at, PersistedIndex};
use crate::service::reindex::{ReindexProgress, ReindexStatus};
use std::path::PathBuf;
use tokio::sync::RwLock as TokioRwLock;

/// Build a bare (no durable corpus) registered-style handle for `id`.
fn bare_handle(id: &str) -> IndexHandle {
    let index_id = IndexId::new(id.to_string());
    let root = PathBuf::from(format!("/tmp/residency-sweep-test-{id}"));
    let indexer = Arc::new(TokioRwLock::new(CodeIndexer::new(id, &root)));
    IndexHandle::bare(index_id, indexer, root)
}

fn entry(id: &str, last_queried: Option<u64>) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: PathBuf::from(format!("/tmp/residency-sweep-test-{id}")),
        last_queried_unix: last_queried,
        ..Default::default()
    }
}

/// Point `TRUSTY_DATA_DIR` at a fresh tempdir and write `entries` to its
/// `indexes.toml`. Returns the tempdir (keep it alive for the test's duration).
fn isolate_and_seed_toml(entries: &[PersistedIndex]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", tmp.path()) };
    let path = crate::service::persistence::indexes_toml_path().expect("indexes_toml_path");
    save_index_registry_at(&path, entries).expect("seed indexes.toml");
    tmp
}

fn clear_env() {
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_MAX_RESIDENT_INDEXES");
    }
}

/// Back-compat: with `TRUSTY_MAX_RESIDENT_INDEXES` unset, a sweep tick must
/// never park a resident index, no matter how many are registered.
#[tokio::test]
#[serial_test::serial]
async fn sweep_disabled_when_cap_unset_parks_nothing() {
    clear_env();
    let entries = vec![
        entry("a", Some(1)),
        entry("b", Some(2)),
        entry("c", Some(3)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);

    let state = SearchAppState::new(IndexRegistry::new());
    for e in &entries {
        state.registry.register(bare_handle(&e.id));
    }

    run_residency_sweep_tick(&Arc::new(state.clone())).await;

    for e in &entries {
        assert!(
            state.registry.get(&IndexId::new(e.id.clone())).is_some(),
            "'{}' must remain resident when the cap is unset",
            e.id
        );
    }
    assert!(
        state.cold_store.is_empty(),
        "nothing should ever be parked when TRUSTY_MAX_RESIDENT_INDEXES is unset"
    );
    clear_env();
}

/// Core acceptance test: the sweep keeps exactly the top-N most-recently
/// searched resident indexes and parks the rest.
#[tokio::test]
#[serial_test::serial]
async fn sweep_keeps_top_n_by_recency_parks_rest() {
    clear_env();
    // c is hottest (300), b is mid (200), a is coldest (100).
    let entries = vec![
        entry("a", Some(100)),
        entry("b", Some(200)),
        entry("c", Some(300)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);
    unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "2") };

    let state = SearchAppState::new(IndexRegistry::new());
    for e in &entries {
        state.registry.register(bare_handle(&e.id));
    }

    run_residency_sweep_tick(&Arc::new(state.clone())).await;

    assert!(
        state.registry.get(&IndexId::new("c".to_string())).is_some(),
        "hottest index 'c' must remain resident"
    );
    assert!(
        state.registry.get(&IndexId::new("b".to_string())).is_some(),
        "second-hottest index 'b' must remain resident"
    );
    assert!(
        state.registry.get(&IndexId::new("a".to_string())).is_none(),
        "coldest index 'a' must be cold-parked beyond the top-2 cap"
    );
    assert!(
        state.cold_store.contains(&IndexId::new("a".to_string())),
        "parked index must be discoverable in the cold store"
    );
    clear_env();
}

/// An index with a `Running` reindex must never be cold-parked, even if it
/// is the coldest candidate by recency — parking it would detach the
/// registry entry the in-flight reindex task is still mutating.
#[tokio::test]
#[serial_test::serial]
async fn sweep_never_parks_index_with_running_reindex() {
    clear_env();
    // "a" is coldest (would normally be parked first) but has an in-flight
    // reindex; "b" is hotter. Cap = 1 forces exactly one park decision.
    let entries = vec![entry("a", Some(100)), entry("b", Some(200))];
    let _tmp = isolate_and_seed_toml(&entries);
    unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "1") };

    let state = SearchAppState::new(IndexRegistry::new());
    for e in &entries {
        state.registry.register(bare_handle(&e.id));
    }
    let running = Arc::new(ReindexProgress::new());
    running.status.store(ReindexStatus::Running);
    state
        .reindex_progress
        .insert(IndexId::new("a".to_string()), running);

    run_residency_sweep_tick(&Arc::new(state.clone())).await;

    assert!(
        state.registry.get(&IndexId::new("a".to_string())).is_some(),
        "an index with a Running reindex must never be parked"
    );
    assert!(
        !state.cold_store.contains(&IndexId::new("a".to_string())),
        "the guarded index must not appear in the cold store either"
    );
    clear_env();
}
