use super::guard::CompactionGuard;
use super::helpers::now_secs;
use super::*;
use crate::memory_core::palace::{Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use chrono::{Duration as ChronoDuration, Utc};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::tempdir;
use uuid::Uuid;

/// Why: Lock the default config values so accidental changes are caught.
#[test]
fn dream_config_defaults() {
    let cfg = DreamConfig::default();
    assert_eq!(cfg.idle_secs, 300);
    assert!((cfg.dedup_threshold - 0.95).abs() < 1e-6);
    assert!((cfg.prune_importance - 0.05).abs() < 1e-6);
    assert_eq!(cfg.max_cycle_ms, 60_000);
    assert!(
        cfg.content_prune_enabled,
        "content-quality pruning is on by default"
    );
    assert_eq!(cfg.content_prune_min_words, 4);
    assert!(
        cfg.recall_benchmark_enabled,
        "recall benchmark is enabled by default"
    );
}

/// Why: `touch` must reset the idle clock; with `idle_secs=0` `is_idle`
/// flips to `true` immediately, and `touch` must NOT make it stay false
/// for >= idle_secs of zero. We use idle_secs=2 and assert the transition.
#[test]
fn dreamer_touch_resets_idle() {
    let dreamer = Dreamer::new(DreamConfig {
        idle_secs: 2,
        ..DreamConfig::default()
    });
    // Just-constructed: last_activity = now, so idle_secs has not elapsed.
    assert!(!dreamer.is_idle(), "fresh dreamer should not be idle yet");

    // Force the idle clock far into the past.
    dreamer
        .last_activity
        .store(now_secs().saturating_sub(10), Ordering::Relaxed);
    assert!(dreamer.is_idle(), "should be idle after 10s simulated wait");

    // Touch resets it.
    dreamer.touch();
    assert!(!dreamer.is_idle(), "touch should reset idle clock");
}

async fn open_test_handle(name: &str) -> Arc<PalaceHandle> {
    // Pre-seed the process-wide embedder with MockEmbedder so no HuggingFace
    // download is attempted. Safe to call multiple times — OnceCell semantics
    // make subsequent calls a no-op. Issue #850.
    seed_shared_embedder_with_mock();
    let dir = tempdir().unwrap();
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: Utc::now(),
        data_dir: dir.path().join(name),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();
    // Keep the tempdir alive by leaking it for the duration of the test —
    // tests are short and tempdir cleanup at process exit is fine.
    std::mem::forget(dir);
    handle
}

/// Why: Two near-identical drawers should collapse to one after a dream
/// cycle so the L1 cache isn't filled with duplicates.
/// What: Insert two drawers with the same content (verbatim — embeddings
/// will land identically), run a dream cycle with default config, and
/// assert the count drops from 2 to 1.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_merges_duplicates() {
    let handle = open_test_handle("dream-merge").await;
    handle
        .remember(
            "Rust uses HNSW for vector search".into(),
            RoomType::Backend,
            vec!["rust".into()],
            0.7,
        )
        .await
        .unwrap();
    handle
        .remember(
            "Rust uses HNSW for vector search".into(),
            RoomType::Backend,
            vec!["rust".into()],
            0.6,
        )
        .await
        .unwrap();
    assert_eq!(handle.drawers.read().len(), 2);

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(stats.merged, 1, "expected exactly one merge");
    assert_eq!(handle.drawers.read().len(), 1, "expected dedup to 1 drawer");
}

/// Why: Old, low-importance drawers must be pruned so storage doesn't
/// grow without bound.
/// What: Insert one drawer with importance=0.01 and back-date its
/// `created_at` to 60 days ago (older than the 30-day prune floor); run
/// dream_cycle and assert it's gone.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_prunes_low_importance() {
    let handle = open_test_handle("dream-prune").await;
    handle
        .remember(
            "very stale fact nobody cares about".into(),
            RoomType::General,
            vec![],
            0.01,
        )
        .await
        .unwrap();
    // Back-date this drawer to satisfy the >30 days requirement.
    {
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut() {
            d.created_at = Utc::now() - ChronoDuration::days(60);
        }
    }
    assert_eq!(handle.drawers.read().len(), 1);

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(stats.pruned, 1, "expected exactly one prune");
    assert!(
        handle.drawers.read().is_empty(),
        "low-importance aged drawer should be removed"
    );
}

/// Why: Regression for issue #55. With the previous strict `<` condition
/// and `prune_importance == DecayConfig::floor == 0.05`, a drawer whose
/// `effective_importance` decayed to the floor was clamped at exactly
/// `0.05`, making `eff < 0.05` unsatisfiable — nothing was ever pruned.
/// The `<=` fix means a drawer at the floor (old, unimportant) is now
/// correctly eligible for pruning.
/// What: Insert one drawer with `importance == prune_importance == floor`,
/// age it past 30 days so the decay floor clamps `eff`, run a cycle, and
/// assert it gets pruned.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_prunes_at_floor_importance() {
    let handle = open_test_handle("dream-prune-floor").await;
    // Importance exactly at the prune threshold (and decay floor default).
    handle
        .remember(
            "drawer that decays to the floor".into(),
            RoomType::General,
            vec![],
            0.05,
        )
        .await
        .unwrap();
    {
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut() {
            // 60 days ago — well past the 30-day prune-age floor and
            // enough decay time to push `eff` down to `floor`.
            d.created_at = Utc::now() - ChronoDuration::days(60);
        }
    }
    assert_eq!(handle.drawers.read().len(), 1);

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.pruned, 1,
        "drawer at floor importance + aged > 30d must be prunable (was unsatisfiable under strict `<`)"
    );
    assert!(handle.drawers.read().is_empty());
}

/// Why: The serve daemon must be able to terminate the dream loop on
/// SIGTERM/Ctrl-C; verify the watch-channel shutdown path actually causes
/// the spawned task to exit instead of looping forever.
/// What: Spawn `start_with_shutdown` with `idle_secs=10` (so it would
/// otherwise sleep), flip the shutdown flag, and assert the join handle
/// completes within a short bounded timeout.
/// Test: This test itself.
#[tokio::test]
async fn dreamer_shutdown_terminates_loop() {
    let handle = open_test_handle("dream-shutdown").await;
    // The unpinned dream loop resolves the handle from a registry each cycle.
    let id = handle.id.clone();
    let registry = crate::memory_core::PalaceRegistry::new();
    registry.register_arc(handle);
    let dreamer = Arc::new(Dreamer::new(DreamConfig {
        idle_secs: 10,
        ..DreamConfig::default()
    }));
    let (tx, rx) = tokio::sync::watch::channel(false);
    let join = dreamer.clone().start_with_shutdown(registry, id, rx);

    // Yield once so the task is scheduled.
    tokio::task::yield_now().await;
    tx.send(true).expect("send shutdown signal");

    // The task should exit promptly — bound the wait to keep the test fast.
    let outcome = tokio::time::timeout(Duration::from_secs(2), join).await;
    assert!(
        outcome.is_ok(),
        "dream loop did not exit within 2s of shutdown"
    );
    outcome.unwrap().expect("join handle clean exit");
}

/// Part 1 (unpin): a running dream loop must NOT hold an `Arc<PalaceHandle>`
/// for the process lifetime — otherwise it defeats LRU / idle-to-disk
/// eviction (the palace can never be freed while its dream loop lives).
///
/// Why: the original design captured `Arc<PalaceHandle>` in the spawned task,
/// so `registry.remove` (or an idle-evict) could not actually drop the
/// handle's heavy state. The fix resolves the handle from the registry each
/// cycle instead.
/// What: registers a handle, keeps a `Weak` to it, spawns the dream loop with
/// a long idle interval (so it only sleeps — never resolves the handle during
/// the test), removes the palace from the registry, and asserts the handle is
/// fully dropped (`Weak::upgrade` is `None`). Under the old pinning bug the
/// upgrade would still succeed.
/// Test: this test itself.
#[tokio::test]
async fn dream_loop_does_not_pin_palace_handle() {
    let handle = open_test_handle("dream-unpin").await;
    let id = handle.id.clone();
    let weak = Arc::downgrade(&handle);
    let registry = crate::memory_core::PalaceRegistry::new();
    registry.register_arc(handle); // registry now holds the ONLY strong ref

    // Long idle interval: the loop's first action is a `sleep(3600s)`, so it
    // never peeks the registry during this test and holds no handle Arc.
    let dreamer = Arc::new(Dreamer::new(DreamConfig {
        idle_secs: 3600,
        ..DreamConfig::default()
    }));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let _join = dreamer.start_with_shutdown(registry.clone(), id.clone(), rx);
    // Let the task reach its sleep.
    tokio::task::yield_now().await;

    // Drop the registry's strong reference. If the loop had captured an Arc
    // (the pinning bug), the handle would survive this.
    registry.remove(&id);
    tokio::task::yield_now().await;
    assert!(
        weak.upgrade().is_none(),
        "dream loop must not pin the handle: after registry.remove it must be fully dropped"
    );
}

/// Why: When drawer rows disappear without their matching vector being
/// removed (partial write, schema migration, pre-fix bug), the HNSW index
/// fills with orphans and the cold-start warning fires. The compact pass
/// must clean these up so `index_vectors == drawer_records` again.
/// What: Remember three drawers, then directly remove two from the drawer
/// table (bypassing `forget`, so the vectors stay in the HNSW index),
/// then run a dream cycle and assert exactly two vectors were compacted.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_compacts_orphaned_vectors() {
    let handle = open_test_handle("dream-compact").await;
    let id_keep = handle
        .remember(
            "alpha drawer about HNSW".into(),
            RoomType::Backend,
            vec![],
            0.7,
        )
        .await
        .unwrap();
    let id_orphan_a = handle
        .remember(
            "beta drawer about something else".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    let id_orphan_b = handle
        .remember(
            "gamma drawer about yet another topic".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    assert_eq!(handle.drawers.read().len(), 3);
    let before_idx = handle.vector_store.index_size();
    let before_ids = handle.vector_store.all_ids().len();
    assert_eq!(before_ids, 3, "key_map should track all three upserts");

    // Manually orphan two: drop them from the drawer table (and the SQLite
    // mirror) but leave their vectors in the HNSW index. This mirrors the
    // pre-fix bug pattern that produced 720 index vectors against 129
    // drawer rows.
    {
        let mut drawers = handle.drawers.write();
        drawers.retain(|d| d.id == id_keep);
    }
    let _ = handle.kg.delete_drawer(id_orphan_a).await;
    let _ = handle.kg.delete_drawer(id_orphan_b).await;

    // Dedup threshold high enough that the surviving drawer's L3 hits
    // don't trigger an accidental merge against the orphan vectors.
    let dreamer = Dreamer::new(DreamConfig {
        dedup_threshold: 0.999,
        ..DreamConfig::default()
    });
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.compacted, 2,
        "expected exactly two orphan vectors removed; got stats={stats:?}"
    );
    let after_ids = handle.vector_store.all_ids().len();
    assert_eq!(
        after_ids, 1,
        "key_map should only track the surviving drawer (before={before_ids}, before_idx={before_idx})"
    );
    // The surviving drawer's id must still be present.
    assert!(
        handle.vector_store.all_ids().contains(&id_keep),
        "compaction must not remove the live drawer's vector"
    );
}

/// Why: The admin dashboard reads `dream_stats.json` to surface the last
/// run's outcome and a "last ran X ago" timestamp; the dream cycle must
/// snapshot itself to that file after every run so the file is current.
/// What: Run a dream cycle on a palace, then load the persisted snapshot
/// from disk and assert the timestamp is recent + stats match.
/// Test: This test itself.
#[tokio::test]
async fn dream_stats_persisted_after_cycle() {
    let handle = open_test_handle("dream-persist").await;
    // One harmless drawer so the cycle has something to scan.
    handle
        .remember(
            "non-duplicate baseline drawer".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    let data_dir = handle.data_dir.clone().expect("data_dir set");
    let loaded = PersistedDreamStats::load(&data_dir)
        .unwrap()
        .expect("dream_stats.json should exist after a cycle");

    assert_eq!(
        loaded.stats, stats,
        "persisted stats must match cycle output"
    );
    let age = chrono::Utc::now().signed_duration_since(loaded.last_run_at);
    assert!(
        age.num_seconds().abs() < 5,
        "last_run_at must be within a few seconds of now; got {age}"
    );
}

/// Why: After a dream cycle, the closet index should map keywords from
/// drawer content back to that drawer's id so L2 can use it as a cheap
/// pre-filter.
/// What: Insert a drawer with a distinctive keyword, run the cycle, and
/// assert the closets map contains that keyword pointing to the drawer.
/// Test: This test itself.
#[tokio::test]
async fn closet_refresh_builds_index() {
    let handle = open_test_handle("dream-closets").await;
    let id = handle
        .remember(
            "Quokkas are the happiest marsupials in Australia".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();
    assert!(
        stats.closets_updated > 0,
        "closet index should be non-empty"
    );

    let closets = handle.closets.read();
    let entry = closets.get("quokkas").expect("expected `quokkas` keyword");
    assert!(
        entry.contains(&id),
        "closet entry must reference the source drawer"
    );
}

/// Why: The operator dashboard depends on `is_compacting()` flipping to
/// `true` while a dream cycle runs and back to `false` once it's done;
/// otherwise the dreaming spinner would either never appear or never
/// clear.
/// What: Confirms the flag starts cleared, then runs a dream cycle and
/// asserts the flag is cleared again after completion. (Catching the
/// `true` window requires racy mid-cycle inspection; the drop-guard
/// semantics are also covered by direct construction below.)
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_toggles_is_compacting() {
    let handle = open_test_handle("dream-compacting-flag").await;
    assert!(!handle.is_compacting(), "flag must start cleared");

    // Direct guard exercise — the in-flight `true` window.
    {
        let _g = CompactionGuard::new(handle.is_compacting.clone());
        assert!(handle.is_compacting(), "guard must set the flag");
    }
    assert!(!handle.is_compacting(), "guard must clear on drop");

    // Full cycle still clears the flag on exit.
    let dreamer = Dreamer::new(DreamConfig::default());
    let _stats = dreamer.dream_cycle(&handle).await.unwrap();
    assert!(
        !handle.is_compacting(),
        "flag must be cleared after dream_cycle returns"
    );
}

/// Why: Drawers captured before the write-path blocklist landed (PR #221)
/// still pollute existing palaces with `Tool use: Bash`-style noise. The
/// dream cycle's content-prune pass must drop them retroactively so the
/// palace self-heals on the next idle window.
/// What: Insert a drawer whose content matches the blocklist prefix and a
/// second sentence-length drawer that should survive, run a dream cycle,
/// and assert only the noise drawer was content-pruned.
/// Test: This test itself.
#[tokio::test]
async fn dream_content_prune_drops_blocklist_drawer() {
    let handle = open_test_handle("dream-content-blocklist").await;
    // `force=true` bypasses the write-path filter so we can plant a
    // pre-blocklist-era noise drawer that the dream pass must clean up.
    handle
        .remember_with_options(
            "Tool use: Bash".into(),
            RoomType::General,
            vec![],
            0.5,
            crate::memory_core::retrieval::RememberOptions::forced(),
        )
        .await
        .unwrap();
    let keep_id = handle
        .remember(
            "Refactor the dream loop to add a content-quality prune pass.".into(),
            RoomType::Backend,
            vec!["dream".into()],
            0.7,
        )
        .await
        .unwrap();
    assert_eq!(handle.drawers.read().len(), 2);

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.content_pruned, 1,
        "expected exactly one blocklist-pruned drawer; got stats={stats:?}"
    );
    let surviving: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert_eq!(surviving, vec![keep_id], "noise drawer must be gone");
}

/// Why: Three-word one-liners (and shorter) carry no semantic value but
/// burn L1 budget and recall slots; the content-prune pass must drop
/// anything under `content_prune_min_words`.
/// What: Insert one 2-word drawer and one comfortably long drawer, run
/// the cycle, and assert only the short one was pruned.
/// Test: This test itself.
#[tokio::test]
async fn dream_content_prune_drops_short_drawer() {
    let handle = open_test_handle("dream-content-short").await;
    // `force=true` bypasses the write-path token-count gate so we can
    // plant a too-short drawer for the dream pass to clean up.
    handle
        .remember_with_options(
            "hello world".into(),
            RoomType::General,
            vec![],
            0.5,
            crate::memory_core::retrieval::RememberOptions::forced(),
        )
        .await
        .unwrap();
    let keep_id = handle
        .remember(
            "This drawer has more than four words and should survive.".into(),
            RoomType::General,
            vec![],
            0.6,
        )
        .await
        .unwrap();
    assert_eq!(handle.drawers.read().len(), 2);

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.content_pruned, 1,
        "expected exactly one short drawer pruned; got stats={stats:?}"
    );
    let surviving: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert_eq!(surviving, vec![keep_id], "short drawer must be gone");
}

/// Why: The prune pass must not be over-eager — normal multi-sentence
/// drawers should survive untouched even when the cycle runs with default
/// config. Without this regression test a future tightening of the
/// blocklist or min-word floor could silently delete useful memories.
/// What: Insert a single multi-sentence drawer, run the cycle, and assert
/// `content_pruned == 0` and the drawer is still present.
/// Test: This test itself.
#[tokio::test]
async fn dream_content_prune_keeps_good_drawer() {
    let handle = open_test_handle("dream-content-keep").await;
    let keep_id = handle
        .remember(
            "Dreaming runs a content-quality prune pass before dedup. \
             It enforces the same rule the write path uses."
                .into(),
            RoomType::Backend,
            vec!["dream".into()],
            0.7,
        )
        .await
        .unwrap();
    assert_eq!(handle.drawers.read().len(), 1);

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.content_pruned, 0,
        "well-formed drawer must not be content-pruned; got stats={stats:?}"
    );
    let surviving: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert_eq!(surviving, vec![keep_id], "good drawer must survive");
}

// ─── Semantic consolidation integration tests ────────────────────────────

/// Why: The dream cycle's semantic phase must add canonical drawers and
/// preserve the originals when a MockInference returns a Merge action.
/// What: Injects a MockInference that merges two drawers into one canonical
/// summary; runs dream_cycle; asserts the canonical drawer is added and the
/// originals are still present (additive-only).
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_semantic_consolidation_with_mock() {
    use crate::memory_core::semantic_consolidation::{
        ConsolidationAction, MockInference, SemanticConsolidationConfig, SemanticConsolidator,
    };

    let handle = open_test_handle("dream-semantic-mock").await;

    // Plant two drawers with distinct content (so NLP dedup doesn't remove one).
    let id1 = handle
        .remember(
            "ts is the search tool used for code navigation".into(),
            RoomType::Backend,
            vec!["ts".into()],
            0.7,
        )
        .await
        .unwrap();
    let id2 = handle
        .remember(
            "trusty-search provides hybrid BM25 and vector retrieval".into(),
            RoomType::Backend,
            vec!["trusty-search".into()],
            0.6,
        )
        .await
        .unwrap();
    assert_eq!(handle.drawers.read().len(), 2);

    // Configure the mock to merge both into one canonical summary.
    let canonical_text = "trusty-search (alias: ts) provides hybrid BM25 + vector code search";
    let actions = vec![ConsolidationAction::Merge {
        canonical_content: canonical_text.to_string(),
        superseded_ids: vec![id1, id2],
    }];
    let mock = std::sync::Arc::new(MockInference::new(actions));
    let call_count = mock.call_count.clone();
    let cfg = SemanticConsolidationConfig {
        enabled: true,
        max_batch_size: 8,
        max_calls_per_cycle: 20,
        ..Default::default()
    };
    let consolidator = std::sync::Arc::new(SemanticConsolidator::new(mock, cfg));

    let dreamer = Dreamer::with_consolidator(
        DreamConfig {
            // High dedup threshold so NLP pass doesn't remove the drawers.
            dedup_threshold: 0.999,
            semantic: SemanticConsolidationConfig {
                enabled: true,
                ..Default::default()
            },
            ..DreamConfig::default()
        },
        consolidator,
    );

    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    // One canonical drawer added.
    assert_eq!(
        stats.semantically_consolidated, 1,
        "expected one canonical drawer; got stats={stats:?}"
    );
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "expected exactly one LLM call"
    );
    assert_eq!(stats.semantic_llm_calls, 1);

    // Original drawers still present (additive-only).
    let drawer_ids: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(
        drawer_ids.contains(&id1),
        "original drawer 1 must be preserved"
    );
    assert!(
        drawer_ids.contains(&id2),
        "original drawer 2 must be preserved"
    );

    // Canonical drawer was added.
    let has_canonical = handle
        .drawers
        .read()
        .iter()
        .any(|d| d.content == canonical_text);
    assert!(has_canonical, "canonical drawer must be present");
}

/// Why: spec-001 — `DrawerType::Task` drawers must never be fed to the
/// semantic consolidator, so they can never be superseded by a canonical
/// summary. This guards the snapshot filter in `semantic_consolidation_pass`.
/// What: plants two Task drawers, injects a MockInference that WOULD merge any
/// drawers it is handed, runs a dream cycle, and asserts the consolidator was
/// never called (call_count == 0), no canonical drawer was created, and both
/// Task drawers survive unchanged.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_semantic_consolidation_skips_task_drawers() {
    use crate::memory_core::palace::DrawerType;
    use crate::memory_core::retrieval::RememberOptions;
    use crate::memory_core::semantic_consolidation::{
        ConsolidationAction, MockInference, SemanticConsolidationConfig, SemanticConsolidator,
    };

    let handle = open_test_handle("dream-semantic-task-skip").await;

    let task_opts = || RememberOptions {
        force: true,
        classify_as: Some(DrawerType::Task),
        ..RememberOptions::default()
    };
    let id1 = handle
        .remember_with_options(
            "Goal: migrate the chat store to redb".into(),
            RoomType::Planning,
            vec![],
            0.7,
            task_opts(),
        )
        .await
        .unwrap();
    let id2 = handle
        .remember_with_options(
            "Milestone: ship the MCP chat-session tools".into(),
            RoomType::Planning,
            vec![],
            0.6,
            task_opts(),
        )
        .await
        .unwrap();
    assert_eq!(handle.drawers.read().len(), 2);

    // A mock that would merge anything it is handed — it must never be called.
    let actions = vec![ConsolidationAction::Merge {
        canonical_content: "tasks should NOT be merged".to_string(),
        superseded_ids: vec![id1, id2],
    }];
    let mock = std::sync::Arc::new(MockInference::new(actions));
    let call_count = mock.call_count.clone();
    let cfg = SemanticConsolidationConfig {
        enabled: true,
        ..Default::default()
    };
    let consolidator = std::sync::Arc::new(SemanticConsolidator::new(mock, cfg));

    let dreamer = Dreamer::with_consolidator(
        DreamConfig {
            dedup_threshold: 0.999,
            semantic: SemanticConsolidationConfig {
                enabled: true,
                ..Default::default()
            },
            ..DreamConfig::default()
        },
        consolidator,
    );

    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.semantically_consolidated, 0,
        "Task drawers must not be consolidated"
    );
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "consolidator must never be called when only Task drawers exist"
    );
    let ids: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(
        ids.contains(&id1) && ids.contains(&id2),
        "both tasks survive"
    );
    assert_eq!(handle.drawers.read().len(), 2, "no canonical drawer added");
}

/// Why: When no inference backend is configured, the semantic phase must
/// silently skip without error and the dream cycle must complete normally
/// with the same behavior as pre-#87.
/// What: Run dream_cycle with default config (no env var, local_model_enabled=false);
/// assert semantically_consolidated == 0 and the cycle succeeds.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_semantic_consolidation_no_inference() {
    // Ensure no env key is set for this test.
    let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");

    let handle = open_test_handle("dream-semantic-no-inference").await;
    handle
        .remember(
            "some memory that should not be semantically consolidated".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig {
        semantic: crate::memory_core::semantic_consolidation::SemanticConsolidationConfig {
            enabled: true,
            ..Default::default()
        },
        local_model_enabled: false,
        openrouter_api_key: String::new(),
        ..DreamConfig::default()
    });

    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.semantically_consolidated, 0,
        "no inference available → semantic phase must be no-op"
    );
    assert_eq!(
        stats.semantic_llm_calls, 0,
        "no LLM calls when inference unavailable"
    );
    // Palace must be intact.
    assert_eq!(
        handle.drawers.read().len(),
        1,
        "drawer must survive untouched"
    );
}

/// Why (issue #2593): the exact production failure — default config
/// (`local_model_enabled = true`, no OpenRouter key, `semantic.model =
/// "anthropic/claude-haiku-4-5"`) — must be caught once and disable the
/// phase for this `Dreamer`'s lifetime instead of retrying every cycle.
/// What: Runs `dream_cycle` twice with no injected consolidator and
/// `DreamConfig::default()`. Asserts both cycles report zero semantic work,
/// `Dreamer::is_semantic_consolidation_disabled()` is `true` after the first
/// cycle, and it stays `true` (no reset) after the second — proving the
/// second cycle short-circuited before rebuilding/retrying the backend.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_semantic_consolidation_invalid_model_disables_once() {
    let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");

    let handle = open_test_handle("dream-semantic-invalid-model").await;
    handle
        .remember(
            "some memory that should not be semantically consolidated".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig::default());
    assert!(!dreamer.is_semantic_consolidation_disabled());

    let stats1 = dreamer.dream_cycle(&handle).await.unwrap();
    assert_eq!(
        stats1.semantically_consolidated, 0,
        "misconfigured model must not produce any consolidation output"
    );
    assert_eq!(stats1.semantic_llm_calls, 0);
    assert!(
        dreamer.is_semantic_consolidation_disabled(),
        "invalid model/provider combination must disable the phase after one cycle"
    );

    let stats2 = dreamer.dream_cycle(&handle).await.unwrap();
    assert_eq!(
        stats2.semantically_consolidated, 0,
        "second cycle must remain a no-op (no retry of the known-bad config)"
    );
    assert_eq!(stats2.semantic_llm_calls, 0);
    assert!(
        dreamer.is_semantic_consolidation_disabled(),
        "disabled state must persist across cycles"
    );

    // Palace must be intact — no partial writes from the doomed build attempt.
    assert_eq!(
        handle.drawers.read().len(),
        1,
        "drawer must survive untouched"
    );
}

/// Why (issue #2593): a correctly-configured local model (a bare Ollama tag,
/// no OpenRouter/cloud vendor prefix) must pass validation and NOT trip the
/// disable flag — only the specific cloud-vendor-prefix mismatch should.
/// What: Sets `semantic.model = "llama3.1"` with the local-model path
/// enabled. The actual HTTP call still fails in this sandboxed test
/// environment (no live Ollama server), which `SemanticConsolidator`
/// already handles gracefully (counts the call, returns no actions) — the
/// assertion here is specifically that `is_semantic_consolidation_disabled()`
/// stays `false`, proving the model passed validation and normal operation
/// (build + attempt) proceeded unchanged.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_semantic_consolidation_valid_local_model_not_disabled() {
    let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");

    let handle = open_test_handle("dream-semantic-valid-local-model").await;
    handle
        .remember(
            "some memory that should not be semantically consolidated".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig {
        semantic: crate::memory_core::semantic_consolidation::SemanticConsolidationConfig {
            enabled: true,
            model: "llama3.1".to_string(),
            ..Default::default()
        },
        local_model_enabled: true,
        openrouter_api_key: String::new(),
        ..DreamConfig::default()
    });

    let _stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert!(
        !dreamer.is_semantic_consolidation_disabled(),
        "a valid local model id must pass validation and not disable the phase"
    );
}

/// Why: When `semantic.enabled = false`, the phase must be skipped even
/// if an inference backend is configured, so operators can opt out cheaply.
/// What: Supply a consolidator but set enabled=false; assert the consolidator's
/// call_count stays at zero after a dream cycle.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_semantic_consolidation_disabled_by_config() {
    use crate::memory_core::semantic_consolidation::{
        MockInference, SemanticConsolidationConfig, SemanticConsolidator,
    };

    let handle = open_test_handle("dream-semantic-disabled").await;
    handle
        .remember(
            "this drawer should not be touched by semantic phase".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();

    let mock = std::sync::Arc::new(MockInference::no_op());
    let call_count = mock.call_count.clone();
    let consolidator = std::sync::Arc::new(SemanticConsolidator::new(
        mock,
        SemanticConsolidationConfig::default(),
    ));

    let dreamer = Dreamer::with_consolidator(
        DreamConfig {
            semantic: SemanticConsolidationConfig {
                enabled: false, // ← disabled
                ..Default::default()
            },
            ..DreamConfig::default()
        },
        consolidator,
    );

    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(stats.semantically_consolidated, 0);
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "mock must not be called when semantic phase is disabled"
    );
}

// ─── Effectiveness metrics tests (issue #1530) ───────────────────────────

/// Why: The compression ratio is the key effectiveness signal; guard its
/// arithmetic against common edge cases (normal case, zero drawers).
/// What: Directly constructs `DreamStats` with known field values and
/// asserts `update_compression_ratio` sets the expected ratio.
/// Test: This test itself.
#[test]
fn dream_compression_ratio_math() {
    let mut stats = DreamStats {
        drawers_before: 10,
        drawers_after: 7,
        ..DreamStats::default()
    };
    stats.update_compression_ratio();
    // (10 - 7) / 10 = 0.3
    let diff = (stats.compression_ratio - 0.3_f64).abs();
    assert!(
        diff < 1e-10,
        "expected 0.3, got {}",
        stats.compression_ratio
    );
}

/// Why: Guard the divide-by-zero path when the palace starts empty.
/// What: `drawers_before = 0` must yield `compression_ratio = 0.0`.
/// Test: This test itself.
#[test]
fn dream_compression_ratio_zero_drawers() {
    let mut stats = DreamStats {
        drawers_before: 0,
        drawers_after: 0,
        ..DreamStats::default()
    };
    stats.update_compression_ratio();
    assert_eq!(
        stats.compression_ratio, 0.0,
        "zero drawers_before must produce 0.0 compression_ratio"
    );
}

/// Why: The semantic consolidation phase can add canonical drawers, causing
/// `drawers_after > drawers_before`. The ratio must be clamped to 0.0 and
/// must not panic.
/// What: Directly construct `DreamStats` with `drawers_after > drawers_before`
/// and assert `compression_ratio == 0.0`.
/// Test: This test itself.
#[test]
fn dream_compression_ratio_net_growth() {
    let mut stats = DreamStats {
        drawers_before: 5,
        drawers_after: 8, // net growth
        ..DreamStats::default()
    };
    stats.update_compression_ratio();
    assert_eq!(
        stats.compression_ratio, 0.0,
        "net palace growth (after > before) must clamp compression_ratio to 0.0"
    );
}
/// Why: `DreamStats` adds `f64` fields so the old `Eq` bound is gone;
/// verify serde round-trips preserve all new fields faithfully.
/// What: Construct a `DreamStats` with non-default effectiveness fields,
/// serialize to JSON, deserialize back, and assert equality on each field.
/// Test: This test itself.
#[test]
fn dream_stats_serde_roundtrip_new_fields() {
    let original = DreamStats {
        merged: 2,
        pruned: 1,
        drawers_before: 8,
        drawers_after: 5,
        compression_ratio: 0.375,
        recall_score_before: Some(0.72),
        recall_score_after: Some(0.81),
        duration_ms: 1200,
        ..DreamStats::default()
    };
    let json = serde_json::to_string(&original).expect("serialize");
    let decoded: DreamStats = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded.drawers_before, 8);
    assert_eq!(decoded.drawers_after, 5);
    let cr_diff = (decoded.compression_ratio - 0.375_f64).abs();
    assert!(cr_diff < 1e-10, "compression_ratio round-trip failed");
    assert_eq!(decoded.recall_score_before, Some(0.72));
    assert_eq!(decoded.recall_score_after, Some(0.81));
    assert_eq!(decoded.merged, 2);
    assert_eq!(decoded.duration_ms, 1200);
}

/// Why: Old `dream_stats.json` files written before this PR lack the new
/// fields. `#[serde(default)]` must allow them to deserialize without error,
/// defaulting new fields to zero / None.
/// What: Deserializes a JSON string that only contains the fields present
/// before issue #1530, then asserts the new fields are at their defaults.
/// Test: This test itself.
#[test]
fn dream_stats_backward_compat() {
    // A dream_stats.json as written by the pre-#1530 code.
    let legacy_json = r#"{
        "merged": 3,
        "pruned": 1,
        "closets_updated": 12,
        "compacted": 0,
        "content_pruned": 2,
        "semantically_consolidated": 0,
        "semantic_llm_calls": 0,
        "semantic_cache_hits": 0,
        "duration_ms": 800
    }"#;
    let decoded: DreamStats =
        serde_json::from_str(legacy_json).expect("backward-compat deserialize must succeed");
    assert_eq!(decoded.merged, 3);
    assert_eq!(decoded.drawers_before, 0, "missing field must default to 0");
    assert_eq!(decoded.drawers_after, 0, "missing field must default to 0");
    assert_eq!(
        decoded.compression_ratio, 0.0,
        "missing field must default to 0.0"
    );
    assert_eq!(
        decoded.recall_score_before, None,
        "missing Option field must default to None"
    );
    assert_eq!(
        decoded.recall_score_after, None,
        "missing Option field must default to None"
    );
}

/// Why: After a dream cycle the stats must include drawer counts that
/// reflect the actual palace state before and after consolidation passes.
/// What: Seed one drawer, run a dream cycle with high dedup threshold so
/// nothing is removed, and assert `drawers_before == drawers_after == 1`.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_records_drawer_counts() {
    let handle = open_test_handle("dream-drawer-counts").await;
    handle
        .remember(
            "drawer count baseline for effectiveness metrics".into(),
            RoomType::General,
            vec![],
            0.6,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig {
        dedup_threshold: 0.999, // nothing deduped
        ..DreamConfig::default()
    });
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(stats.drawers_before, 1, "expected 1 drawer before");
    assert_eq!(
        stats.drawers_after, 1,
        "expected 1 drawer after (none removed)"
    );
    // Compression ratio: (1 - 1) / 1 = 0.0
    assert_eq!(
        stats.compression_ratio, 0.0,
        "no drawers removed → ratio 0.0"
    );
}

/// Why: When the dedup pass removes a drawer, `drawers_after < drawers_before`
/// and the compression ratio must be non-zero.
/// What: Insert two identical drawers, run a cycle, assert `compression_ratio > 0`.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_compression_ratio_nonzero_after_dedup() {
    let handle = open_test_handle("dream-compress-nonzero").await;
    handle
        .remember(
            "duplicate drawer for compression test".into(),
            RoomType::General,
            vec![],
            0.7,
        )
        .await
        .unwrap();
    handle
        .remember(
            "duplicate drawer for compression test".into(),
            RoomType::General,
            vec![],
            0.6,
        )
        .await
        .unwrap();
    assert_eq!(handle.drawers.read().len(), 2);

    let dreamer = Dreamer::new(DreamConfig::default());
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(stats.drawers_before, 2, "two drawers before cycle");
    assert_eq!(stats.drawers_after, 1, "one remaining after dedup");
    // (2 - 1) / 2 = 0.5
    let diff = (stats.compression_ratio - 0.5_f64).abs();
    assert!(
        diff < 1e-10,
        "expected compression_ratio=0.5, got {}",
        stats.compression_ratio
    );
}

/// Why: An empty palace must not cause the recall benchmark to panic or
/// return a misleading score. `run_benchmark` must return `None`.
/// What: Run `run_benchmark` directly on an empty palace and assert `None`.
/// Test: This test itself.
#[tokio::test]
async fn dream_recall_benchmark_empty_palace_returns_none() {
    let handle = open_test_handle("dream-bench-empty").await;
    // Palace is empty — no drawers seeded.
    let result = super::recall_benchmark::run_benchmark(&handle).await;
    assert_eq!(
        result, None,
        "empty palace must yield None from recall benchmark"
    );
}

/// Why: With at least one drawer, the recall benchmark must return a score
/// in the valid [0, 1] range.
/// What: Seed two drawers, run the benchmark, assert `Some(score)` in range.
/// Test: This test itself.
#[tokio::test]
async fn dream_recall_benchmark_returns_score_with_drawers() {
    let handle = open_test_handle("dream-bench-score").await;
    handle
        .remember(
            "cargo build and test commands for the Rust workspace".into(),
            RoomType::Backend,
            vec!["cargo".into()],
            0.8,
        )
        .await
        .unwrap();
    handle
        .remember(
            "error handling with thiserror for libraries and anyhow for binaries".into(),
            RoomType::Backend,
            vec!["errors".into()],
            0.7,
        )
        .await
        .unwrap();

    let result = super::recall_benchmark::run_benchmark(&handle).await;
    let score = result.expect("expected Some(score) with seeded drawers");
    assert!(
        score.is_finite() && score >= 0.0,
        "recall benchmark score must be finite and non-negative; got {score}"
    );
}

/// Why: The dream cycle must record both pre- and post-cycle recall scores
/// in the returned stats so they land in dream_stats.json.
/// What: Seed a drawer, run the cycle, assert both score fields are Some.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_records_recall_scores() {
    let handle = open_test_handle("dream-recall-scores").await;
    handle
        .remember(
            "HNSW vector search and batch embedding performance patterns".into(),
            RoomType::Backend,
            vec!["hnsw".into(), "embedding".into()],
            0.8,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig {
        dedup_threshold: 0.999, // nothing removed
        ..DreamConfig::default()
    });
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert!(
        stats.recall_score_before.is_some(),
        "recall_score_before must be Some with a seeded palace"
    );
    assert!(
        stats.recall_score_after.is_some(),
        "recall_score_after must be Some with a seeded palace"
    );
    let before = stats.recall_score_before.unwrap();
    let after = stats.recall_score_after.unwrap();
    assert!(
        before.is_finite() && before >= 0.0,
        "recall_score_before={before} must be finite and non-negative"
    );
    assert!(
        after.is_finite() && after >= 0.0,
        "recall_score_after={after} must be finite and non-negative"
    );
}

/// Why: Effectiveness fields must survive a round-trip through
/// `PersistedDreamStats::save` → `PersistedDreamStats::load`.
/// What: Run a cycle with seeded drawers, write to disk, read back, and
/// assert all effectiveness fields match.
/// Test: This test itself.
#[tokio::test]
async fn dream_stats_effectiveness_fields_persisted() {
    let handle = open_test_handle("dream-persist-eff").await;
    handle
        .remember(
            "unit test patterns and mock usage in Rust tests".into(),
            RoomType::General,
            vec!["testing".into()],
            0.7,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig {
        dedup_threshold: 0.999,
        ..DreamConfig::default()
    });
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    let data_dir = handle.data_dir.clone().expect("data_dir set");
    let loaded = PersistedDreamStats::load(&data_dir)
        .unwrap()
        .expect("dream_stats.json must exist after cycle");

    assert_eq!(
        loaded.stats.drawers_before, stats.drawers_before,
        "drawers_before must be persisted"
    );
    assert_eq!(
        loaded.stats.drawers_after, stats.drawers_after,
        "drawers_after must be persisted"
    );
    let cr_diff = (loaded.stats.compression_ratio - stats.compression_ratio).abs();
    assert!(cr_diff < 1e-10, "compression_ratio must be persisted");
    assert_eq!(
        loaded.stats.recall_score_before, stats.recall_score_before,
        "recall_score_before must be persisted"
    );
    assert_eq!(
        loaded.stats.recall_score_after, stats.recall_score_after,
        "recall_score_after must be persisted"
    );
}

/// Why: The recall benchmark adds two full embed+search passes per cycle.
/// When `recall_benchmark_enabled = false`, both passes must be skipped and
/// both recall score fields must be `None`, while the cycle itself completes
/// normally.
/// What: Run `dream_cycle` with `recall_benchmark_enabled = false` on a
/// seeded palace and assert both `recall_score_before` and
/// `recall_score_after` are `None`.
/// Test: This test itself.
#[tokio::test]
async fn dream_cycle_recall_benchmark_disabled() {
    let handle = open_test_handle("dream-bench-disabled").await;
    handle
        .remember(
            "recall benchmark opt-out test drawer".into(),
            RoomType::General,
            vec![],
            0.6,
        )
        .await
        .unwrap();

    let dreamer = Dreamer::new(DreamConfig {
        recall_benchmark_enabled: false,
        dedup_threshold: 0.999, // nothing removed
        ..DreamConfig::default()
    });
    let stats = dreamer.dream_cycle(&handle).await.unwrap();

    assert_eq!(
        stats.recall_score_before, None,
        "recall_score_before must be None when benchmark is disabled"
    );
    assert_eq!(
        stats.recall_score_after, None,
        "recall_score_after must be None when benchmark is disabled"
    );
    // Cycle must still complete and report drawer counts.
    assert_eq!(
        stats.drawers_before, 1,
        "drawers_before must still be counted"
    );
    assert_eq!(
        stats.drawers_after, 1,
        "drawers_after must still be counted"
    );
}

// ─── spec-001 Phase 3: on-demand room-scoped consolidation ───────────────────

/// Why: `dream_consolidate_room` must scope to a single room — drawers in other
/// rooms must not even reach the consolidator.
/// What: seeds two aged Planning drawers, runs `consolidate_scoped` first for
/// Backend (no aged drawers there) and asserts the consolidator is never called
/// and nothing changes, then for Planning and asserts the two originals are
/// consolidated into one summary and evicted.
/// Test: this function.
#[tokio::test]
async fn consolidate_scoped_filters_by_room() {
    use crate::memory_core::semantic_consolidation::{
        ConsolidationAction, MockInference, SemanticConsolidationConfig, SemanticConsolidator,
    };
    use std::sync::atomic::Ordering;

    let handle = open_test_handle("scoped-room-filter").await;
    let p1 = handle
        .remember(
            "planning note one about the roadmap".into(),
            RoomType::Planning,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    let p2 = handle
        .remember(
            "planning note two about the roadmap".into(),
            RoomType::Planning,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    // Age both past the default 7-day window.
    {
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut() {
            d.created_at = Utc::now() - ChronoDuration::days(30);
        }
    }

    let mock = std::sync::Arc::new(MockInference::new(vec![ConsolidationAction::Merge {
        canonical_content: "roadmap planning summary".to_string(),
        superseded_ids: vec![p1, p2],
    }]));
    let call_count = mock.call_count.clone();
    let consolidator = std::sync::Arc::new(SemanticConsolidator::new(
        mock,
        SemanticConsolidationConfig {
            enabled: true,
            ..Default::default()
        },
    ));
    let cfg = DreamConfig::default();

    // Backend has no aged drawers => consolidator never runs, nothing changes.
    let backend = consolidate_scoped(
        &handle,
        &cfg,
        Some(RoomType::Backend),
        7,
        Some(consolidator.clone()),
    )
    .await
    .unwrap();
    assert_eq!(backend, RoomConsolidationStats::default());
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        0,
        "wrong room must not consolidate"
    );
    assert_eq!(handle.drawers.read().len(), 2);

    // Planning has the two aged drawers => one summary created, both evicted.
    let planning = consolidate_scoped(
        &handle,
        &cfg,
        Some(RoomType::Planning),
        7,
        Some(consolidator),
    )
    .await
    .unwrap();
    assert_eq!(planning.summary_facts_created, 1);
    assert_eq!(planning.facts_evicted, 2);
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        1,
        "consolidator ran once for Planning"
    );
    let ids: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(
        !ids.contains(&p1) && !ids.contains(&p2),
        "superseded originals evicted"
    );
}

/// Why: Task drawers must never be consolidated, even by the on-demand tool.
/// What: seeds two aged Task drawers, runs `consolidate_scoped` with a mock that
/// would merge them, and asserts the consolidator is never called and both
/// tasks survive.
/// Test: this function.
#[tokio::test]
async fn consolidate_scoped_skips_task_drawers() {
    use crate::memory_core::palace::DrawerType;
    use crate::memory_core::retrieval::RememberOptions;
    use crate::memory_core::semantic_consolidation::{
        ConsolidationAction, MockInference, SemanticConsolidationConfig, SemanticConsolidator,
    };
    use std::sync::atomic::Ordering;

    let handle = open_test_handle("scoped-task-skip").await;
    let task_opts = || RememberOptions {
        force: true,
        classify_as: Some(DrawerType::Task),
        ..RememberOptions::default()
    };
    let t1 = handle
        .remember_with_options(
            "Goal: keep this forever".into(),
            RoomType::Planning,
            vec![],
            0.5,
            task_opts(),
        )
        .await
        .unwrap();
    let t2 = handle
        .remember_with_options(
            "Goal: and this one too".into(),
            RoomType::Planning,
            vec![],
            0.5,
            task_opts(),
        )
        .await
        .unwrap();
    {
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut() {
            d.created_at = Utc::now() - ChronoDuration::days(30);
        }
    }

    let mock = std::sync::Arc::new(MockInference::new(vec![ConsolidationAction::Merge {
        canonical_content: "tasks must NOT merge".to_string(),
        superseded_ids: vec![t1, t2],
    }]));
    let call_count = mock.call_count.clone();
    let consolidator = std::sync::Arc::new(SemanticConsolidator::new(
        mock,
        SemanticConsolidationConfig {
            enabled: true,
            ..Default::default()
        },
    ));

    let stats = consolidate_scoped(
        &handle,
        &DreamConfig::default(),
        None,
        7,
        Some(consolidator),
    )
    .await
    .unwrap();
    assert_eq!(
        stats,
        RoomConsolidationStats::default(),
        "task-only palace yields no work"
    );
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        0,
        "consolidator never sees Task drawers"
    );
    let ids: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(ids.contains(&t1) && ids.contains(&t2), "both tasks survive");
}

/// Why: when no inference backend is configured the tool must no-op cleanly.
/// What: with the OpenRouter env key removed, the local-model path explicitly
/// disabled, and no injected consolidator, `consolidate_scoped` returns zero
/// counts without error. `local_model_enabled: false` is set explicitly here
/// (issue #2593): `DreamConfig::default()` alone is no longer "no inference"
/// — its default model/provider combination is exactly the misconfiguration
/// this issue fixes, so it now returns `Err` (see
/// `consolidate_scoped_invalid_model_returns_err`) instead of a silent no-op.
/// Test: this function.
#[tokio::test]
async fn consolidate_scoped_no_inference_is_noop() {
    let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");
    let handle = open_test_handle("scoped-no-inference").await;
    handle
        .remember("some aged fact".into(), RoomType::Backend, vec![], 0.5)
        .await
        .unwrap();
    let cfg = DreamConfig {
        local_model_enabled: false,
        ..DreamConfig::default()
    };
    let stats = consolidate_scoped(&handle, &cfg, None, 7, None)
        .await
        .unwrap();
    assert_eq!(stats, RoomConsolidationStats::default());
}

/// Why (issue #2593): the on-demand `dream_consolidate_room` tool must
/// surface a misconfigured model/provider combination to the caller as an
/// error rather than silently no-op'ing (it's a single user-triggered call,
/// not a recurring background job, so there is nothing to "disable").
/// What: `DreamConfig::default()` — local-model path enabled, no OpenRouter
/// key, default OpenRouter-style model id — is exactly the #2593
/// misconfiguration. Asserts `consolidate_scoped` returns `Err` naming both
/// the model and Ollama.
#[tokio::test]
async fn consolidate_scoped_invalid_model_returns_err() {
    let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");
    let handle = open_test_handle("scoped-invalid-model").await;
    handle
        .remember("some aged fact".into(), RoomType::Backend, vec![], 0.5)
        .await
        .unwrap();
    let err = consolidate_scoped(&handle, &DreamConfig::default(), None, 7, None)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("anthropic/"),
        "error should name the model: {msg}"
    );
    assert!(
        msg.contains("Ollama"),
        "error should name the provider: {msg}"
    );
}

/// Why: `max_age_days <= 0` is a guard value meaning "consolidate nothing".
/// Regression for the off-by-zero bug where a cutoff of `now` made *every*
/// drawer — including freshly created ones — eligible, evicting the whole room.
/// What: seeds two recent (NOT aged) drawers plus a mock consolidator that would
/// merge them, then calls `consolidate_scoped` with `max_age_days = 0` and a
/// negative value; asserts zero counts, the consolidator never runs, and both
/// recent drawers survive.
/// Test: this function.
#[tokio::test]
async fn consolidate_scoped_non_positive_age_is_noop() {
    use crate::memory_core::semantic_consolidation::{
        ConsolidationAction, MockInference, SemanticConsolidationConfig, SemanticConsolidator,
    };

    let handle = open_test_handle("scoped-non-positive-age").await;
    let r1 = handle
        .remember(
            "recent planning note one".into(),
            RoomType::Planning,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    let r2 = handle
        .remember(
            "recent planning note two".into(),
            RoomType::Planning,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    // Deliberately leave `created_at` at "now" — these drawers are NOT aged.

    let mock = std::sync::Arc::new(MockInference::new(vec![ConsolidationAction::Merge {
        canonical_content: "must NOT merge recent drawers".to_string(),
        superseded_ids: vec![r1, r2],
    }]));
    let call_count = mock.call_count.clone();
    let consolidator = std::sync::Arc::new(SemanticConsolidator::new(
        mock,
        SemanticConsolidationConfig {
            enabled: true,
            ..Default::default()
        },
    ));
    let cfg = DreamConfig::default();

    for age in [0_i64, -5] {
        let stats = consolidate_scoped(
            &handle,
            &cfg,
            Some(RoomType::Planning),
            age,
            Some(consolidator.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            stats,
            RoomConsolidationStats::default(),
            "max_age_days={age} must consolidate/evict nothing"
        );
    }

    assert_eq!(
        call_count.load(Ordering::Relaxed),
        0,
        "consolidator must never run for a non-positive age window"
    );
    let ids: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(
        ids.contains(&r1) && ids.contains(&r2),
        "recent drawers must survive a non-positive age window"
    );
}

/// Why (issue #1713): consolidation must never evict an original drawer
/// unless the `superseded_by` KG-triple write actually landed — evicting on
/// a failed (previously logged-only) write leaves the canonical drawer with
/// no recorded provenance link back to the original, silently destroying
/// history with no way to recover it.
/// What: Opens a `KnowledgeGraph` forced into read-only snapshot mode via
/// the same lock-contention trick as
/// `kg_redb::tests::open_on_locked_file_returns_snapshot_handle` (seed the
/// file, drop the handle so the in-process cache entry expires, hold the
/// file's exclusive flock via the raw `redb` API, then reopen) so every
/// `handle.kg.assert` call fails. Calls
/// `record_provenance_and_collect_superseded` directly — the extracted
/// per-canonical provenance step of `apply_consolidation_result` — and
/// asserts the original id is NOT added to the eviction-candidate list.
/// Test: this function.
#[tokio::test]
async fn apply_consolidation_result_keeps_original_when_kg_write_fails() {
    use super::cycle::record_provenance_and_collect_superseded;
    use crate::memory_core::store::kg::KnowledgeGraph;
    use crate::memory_core::store::kg_redb::KgStoreRedb;
    use crate::memory_core::store::vector::UsearchStore;

    let dir = tempdir().unwrap();
    let kg_path = dir.path().join("kg.redb");
    // Seed the file via the lower-level `KgStoreRedb` (no `KgWriter` actor
    // spawned, unlike `KnowledgeGraph::open`), then drop it so the in-process
    // db cache entry expires synchronously — no async drop race to wait out
    // — before we grab the raw lock below.
    drop(KgStoreRedb::open(&kg_path).unwrap());
    // Hold the file's exclusive flock via the raw redb API so the next
    // `KnowledgeGraph::open` hits the lock-contention path and falls back to
    // a read-only snapshot (issue #59 behaviour).
    let _live = redb::Database::create(&kg_path).unwrap();

    let kg = KnowledgeGraph::open(&kg_path).unwrap();
    assert!(
        kg.is_read_only(),
        "precondition: KG must be a read-only snapshot for this test to be valid"
    );

    let vs = UsearchStore::new(dir.path().join("idx.usearch"), 384).unwrap();
    let handle = Arc::new(PalaceHandle::new(
        PalaceId::new("kg-write-fail"),
        "test".to_string(),
        vs,
        kg,
    ));

    let canonical_id = Uuid::new_v4();
    let orig_id = Uuid::new_v4();
    let mut superseded_ids: Vec<Uuid> = Vec::new();
    record_provenance_and_collect_superseded(
        &handle,
        canonical_id,
        &[orig_id],
        &mut superseded_ids,
    )
    .await;

    assert!(
        superseded_ids.is_empty(),
        "original must NOT be marked evictable when the superseded_by KG write failed"
    );
}

// ─── RAII env-var guard for tests ────────────────────────────────────────
//
// Safety: test-only; the tokio::test macro with default settings uses the
// current-thread runtime so env-var mutation is single-threaded.

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        // Safety: test-only; single-threaded test execution.
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Safety: test-only; single-threaded test execution.
        match &self.previous {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
