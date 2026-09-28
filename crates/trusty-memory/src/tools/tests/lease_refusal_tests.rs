//! #8733: the MCP maintenance tools refuse without the maintenance lease.
//!
//! Why: `palace_dream`, `dream_consolidate_room` and `palace_compact` evict,
//! delete and rewrite rows. A second writer on one data root must refuse them
//! the way `dream_run` does, and change nothing.
//! What: a second `MaintenanceLease` on the same root holds the lease; a
//! writer-intent state then calls each tool, and the test asserts the refusal
//! and that `kg.redb`'s row counts and files are unchanged.
//! Test: this IS the test module.

use super::*;
use trusty_common::memory_core::store::kg_redb::KgRedbStats;
use trusty_common::memory_core::{MaintenanceLease, PalaceRegistry};

/// A writer state whose data root's lease `holder` owns, with one palace
/// ("leased") holding one triple.
async fn leased_elsewhere() -> (AppState, tempfile::TempDir, MaintenanceLease) {
    skip_palace_enforcement();
    seed_embedder();
    let tmp = tempfile::tempdir().expect("tempdir");
    let holder = MaintenanceLease::new(tmp.path());
    assert!(holder.try_hold().is_held());
    let state = AppState::new(tmp.path().to_path_buf()).with_writer_intent();
    state.set_ready();
    dispatch_tool(&state, "palace_create", json!({"name": "leased"}))
        .await
        .expect("palace_create");
    dispatch_tool(
        &state,
        "kg_assert",
        json!({"palace": "leased", "subject": "alice", "predicate": "works_at", "object": "Acme"}),
    )
    .await
    .expect("kg_assert");
    (state, tmp, holder)
}

/// `(table, rows)` of the palace's `kg.redb`, plus whether a compaction
/// backup exists — the evidence that a refused call wrote nothing.
fn store_snapshot(state: &AppState) -> (Vec<(String, u64)>, bool) {
    let palace = PalaceRegistry::list_palaces(&state.data_root)
        .expect("list")
        .into_iter()
        .find(|p| p.id.0 == "leased")
        .expect("palace");
    let rows = KgRedbStats::measure(&palace.data_dir.join("kg.redb"), 90)
        .expect("measure")
        .tables
        .into_iter()
        .map(|t| (t.name, t.rows))
        .collect();
    let backup = palace.data_dir.join("kg.redb.pre-compact.bak").exists();
    (rows, backup)
}

/// Call `tool` under a lease held elsewhere; assert it refuses, naming the
/// lease, and leaves the store as it was.
async fn assert_refused(tool: &str, args: Value) {
    let (state, _tmp, _holder) = leased_elsewhere().await;
    let before = store_snapshot(&state);
    let err = dispatch_tool(&state, tool, args)
        .await
        .expect_err("a process without the maintenance lease must refuse");
    assert!(
        format!("{err:#}").contains("maintenance lease"),
        "{tool}: {err:#}"
    );
    assert_eq!(before, store_snapshot(&state), "{tool} changed the store");
    assert!(!before.1, "no compaction backup before the call");
}

#[tokio::test]
async fn palace_dream_is_refused_without_the_maintenance_lease() {
    assert_refused(
        "palace_dream",
        json!({"palace": "leased", "compact": true, "max_age_days": 0}),
    )
    .await;
}

#[tokio::test]
async fn dream_consolidate_room_is_refused_without_the_maintenance_lease() {
    assert_refused(
        "dream_consolidate_room",
        json!({"palace": "leased", "compact": true, "max_age_days": 0}),
    )
    .await;
}

#[tokio::test]
async fn palace_compact_is_refused_without_the_maintenance_lease() {
    assert_refused("palace_compact", json!({"palace": "leased"})).await;
}
