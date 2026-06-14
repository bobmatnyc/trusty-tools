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
    let dreamer = Arc::new(Dreamer::new(DreamConfig {
        idle_secs: 10,
        ..DreamConfig::default()
    }));
    let (tx, rx) = tokio::sync::watch::channel(false);
    let join = dreamer.clone().start_with_shutdown(handle, rx);

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
