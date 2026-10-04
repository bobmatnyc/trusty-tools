//! Rebase guard for `MemoryService::recall_all` (#9141, #8246).
//!
//! Why: this branch moved `recall_all` out of `core.rs`, where M1's #9141
//! empty-palace skip lands. A rebase that resolves that conflict by keeping
//! this side would drop the skip silently; this test fails when it does.
//! Test: this IS the test module.

use std::path::Path;

use chrono::Utc;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::{Drawer, PalaceRegistry};
use uuid::Uuid;

use super::super::core::MemoryService;
use crate::AppState;

/// Create `full` (one stored drawer) and `empty` palaces under `root`.
///
/// What: runs on its own thread and runtime, and drops both before returning,
/// so every KG writer task is gone and no redb lock outlives the setup.
fn create_on_disk(root: &Path, full: &PalaceId, empty: &PalaceId) {
    let (root, full, empty) = (root.to_path_buf(), full.clone(), empty.clone());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let writer = PalaceRegistry::new();
            for id in [&full, &empty] {
                let palace = Palace {
                    id: id.clone(),
                    name: id.as_str().to_string(),
                    description: None,
                    created_at: Utc::now(),
                    data_dir: root.join(id.as_str()),
                };
                let handle = writer.create_palace(&root, palace).expect("create");
                if id == &full {
                    let drawer = Drawer::new(Uuid::new_v4(), "a stored fact");
                    handle.kg.store().upsert_drawer(&drawer).expect("upsert");
                }
            }
        });
    })
    .join()
    .expect("setup thread");
}

/// Why (#9141): a recall-all cold-opened every palace on disk, most of them
/// empty. M1's `skip_empty_palaces` skips a provably empty palace unopened;
/// the rebase onto M1 must keep that skip in this branch's `recall_all`.
/// What: one full and one empty palace, neither resident. `peek` is None
/// after the call either way, because `recall_streamed` releases what it
/// opened, so a sentinel `unopenable` record carries the proof: a successful
/// open clears it (`PalaceRegistry::register_arc`).
#[tokio::test]
#[ignore = "enabled at the M1 rebase (#9141 skip_empty_palaces)"]
async fn recall_all_never_opens_an_empty_palace() {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (full, empty) = (PalaceId::new("unit-full"), PalaceId::new("unit-empty"));
    create_on_disk(tmp.path(), &full, &empty);
    let state = AppState::new(tmp.path().to_path_buf());
    let sentinel = "sentinel: cleared by any successful open";
    state
        .registry
        .record_unopenable(empty.clone(), sentinel.to_string());

    let out = MemoryService::new(state.clone())
        .recall_all("stored fact", 5, false)
        .await;

    assert!(out.is_array(), "{out}");
    assert!(state.registry.peek(&empty).is_none());
    assert_eq!(
        state.registry.unopenable_reason(&empty).as_deref(),
        Some(sentinel),
        "recall_all opened the empty palace"
    );
}
