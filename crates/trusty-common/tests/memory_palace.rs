//! Integration tests for `DrawerType::Task` protection (spec-001 Phase 4).
//!
//! Why: Task drawers hold goals/checkpoints an application must re-derive
//! across sessions, so they must survive the dream cycle's eviction and
//! consolidation passes. These tests exercise that contract end-to-end through
//! the public palace + dream API, and confirm the optional `completed_at`
//! timestamp round-trips through a palace reopen.
//! What: builds a palace with the mock embedder (no model download), seeds Task
//! and ordinary drawers, ages them past the prune floor, runs a full dream
//! cycle, and asserts the Task drawers remain while the ordinary one is evicted.
//! Test: this IS the test module.

use chrono::{Duration as ChronoDuration, Utc};
use std::sync::Arc;
use tempfile::tempdir;
use trusty_common::memory_core::dream::{DreamConfig, Dreamer};
use trusty_common::memory_core::palace::{DrawerType, Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    PalaceHandle, RememberOptions, seed_shared_embedder_with_mock,
};

/// Open a fresh palace backed by a leaked tempdir and the mock embedder.
///
/// Why: tests must not hit HuggingFace; the mock embedder is deterministic
/// (identical content => identical vectors) which also lets the dedup pass
/// fire on duplicate Task drawers.
/// What: returns an open `PalaceHandle` and the data dir (so a second handle
/// can reopen the same palace).
/// Test: used by both tests below.
fn open_palace(name: &str) -> (Arc<PalaceHandle>, std::path::PathBuf) {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join(name);
    std::fs::create_dir_all(&data_dir).expect("mkdir");
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: Utc::now(),
        data_dir: data_dir.clone(),
    };
    let handle = PalaceHandle::open(&palace).expect("open palace");
    std::mem::forget(dir); // keep the tempdir alive for the test's lifetime
    (handle, data_dir)
}

/// Remember a drawer of an explicit `DrawerType`, bypassing the write filter.
///
/// Why: tests need to pin the classification (`Task` vs ordinary) and store
/// otherwise-filterable short content deterministically.
/// What: calls `remember_with_options` with `force=true` and the requested
/// `classify_as`.
/// Test: used by both tests below.
async fn remember_typed(
    handle: &Arc<PalaceHandle>,
    content: &str,
    importance: f32,
    drawer_type: DrawerType,
) -> uuid::Uuid {
    handle
        .remember_with_options(
            content.to_string(),
            RoomType::General,
            vec![],
            importance,
            RememberOptions {
                force: true,
                classify_as: Some(drawer_type),
                ..RememberOptions::default()
            },
        )
        .await
        .expect("remember")
}

/// Task drawers survive a full dream cycle (eviction + dedup); ordinary aged,
/// low-importance drawers do not.
///
/// Why: spec-001 acceptance criterion 4 — Task drawers are never evicted or
/// consolidated.
/// What: seeds two identical Task drawers (which dedup would normally merge)
/// plus one ordinary drawer, ages all three 60 days at importance 0.01 (well
/// past the 30-day / 0.05 prune floor), runs a full dream cycle, and asserts
/// both Task drawers remain while the ordinary one is pruned.
/// Test: this function.
#[tokio::test]
async fn task_drawers_survive_full_dream_cycle() {
    let (handle, _dir) = open_palace("task-survival");

    let goal = "Goal: ship the trusty-memory chat session manager to production";
    remember_typed(&handle, goal, 0.01, DrawerType::Task).await;
    remember_typed(&handle, goal, 0.01, DrawerType::Task).await; // identical => dedup bait
    let ordinary_id = remember_typed(
        &handle,
        "an ordinary stale note nobody reads",
        0.01,
        DrawerType::UserFact,
    )
    .await;

    // Age every drawer well past the 30-day prune floor.
    {
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut() {
            d.created_at = Utc::now() - ChronoDuration::days(60);
        }
    }
    assert_eq!(handle.drawers.read().len(), 3, "three drawers seeded");

    let dreamer = Dreamer::new(DreamConfig::default());
    dreamer.dream_cycle(&handle).await.expect("dream cycle");

    let drawers = handle.drawers.read();
    let task_count = drawers
        .iter()
        .filter(|d| d.drawer_type == DrawerType::Task)
        .count();
    assert_eq!(
        task_count, 2,
        "both Task drawers must survive eviction + dedup; got {task_count}"
    );
    assert!(
        !drawers.iter().any(|d| d.id == ordinary_id),
        "ordinary aged low-importance drawer should have been pruned"
    );
}

/// A Task drawer's `completed_at` timestamp survives a palace reopen.
///
/// Why: spec-001 — `completed_at` is tracked; it must persist so an
/// application can distinguish open vs done tasks across restarts.
/// What: stores a Task drawer, sets `completed_at`, flushes, drops the handle,
/// reopens the palace from disk, and asserts the type + timestamp survived.
/// Test: this function.
#[tokio::test]
async fn task_completed_at_round_trips_through_reopen() {
    let (handle, data_dir) = open_palace("task-completed");
    let id = remember_typed(&handle, "Goal: cut the v2 release", 0.5, DrawerType::Task).await;
    let done = Utc::now();
    // Mark the task complete in-memory, then persist through redb (the
    // authoritative store consulted on reopen) so the change survives.
    let updated = {
        let mut drawers = handle.drawers.write();
        let d = drawers.iter_mut().find(|d| d.id == id).expect("drawer");
        d.completed_at = Some(done);
        d.clone()
    };
    handle
        .kg
        .upsert_drawer_sync(&updated)
        .expect("persist completed_at");
    handle.flush().expect("flush");
    drop(handle); // release the redb write lock before reopening

    let palace = Palace {
        id: PalaceId::new("task-completed"),
        name: "task-completed".into(),
        description: None,
        created_at: Utc::now(),
        data_dir,
    };
    let reopened = PalaceHandle::open(&palace).expect("reopen palace");
    let drawers = reopened.drawers.read();
    let d = drawers
        .iter()
        .find(|d| d.id == id)
        .expect("drawer survived");
    assert_eq!(d.drawer_type, DrawerType::Task);
    let got = d.completed_at.expect("completed_at persisted");
    assert_eq!(got.timestamp_millis(), done.timestamp_millis());
}
