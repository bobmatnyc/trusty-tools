//! Regression tests for #9391: a dream cycle on an unchanged palace does no
//! embedding work.
//!
//! Why: the idle loop re-embedded every drawer of an unchanged palace every
//! 300 s, and each run raised the daemon's RSS by 0.8–1.7 GB. The skip is
//! invisible in `DreamStats` — an unchanged palace merges nothing either way —
//! so these tests count the embedder's calls.
//! What: a counting embedder injected with `Dreamer::with_embedder`; one test
//! per branch of the skip decision in `settled`, and one per arm of the
//! completion gate in `Dreamer::dream_cycle`. Merge and evict failures are
//! injected with the kg.redb test hooks; inference failures with a failing
//! `Inference` backend.
//! Test: itself.

use super::config::DreamConfig;
use super::dreamer::Dreamer;
use super::settled;
use crate::credentials::env_guard::EnvVarGuard;
use crate::embedder::MockEmbedder;
use crate::memory_core::PalaceRegistry;
use crate::memory_core::embed::{EMBED_DIM, Embedder};
use crate::memory_core::palace::{Drawer, DrawerType, Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::semantic_consolidation::{
    ConsolidationAction, Inference, MockInference, SemanticConsolidationConfig,
    SemanticConsolidator,
};
use crate::memory_core::store::kg_redb::BatchWriteOp;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::tempdir;

/// An [`Embedder`] that counts its `embed_batch` calls.
struct CountingEmbedder {
    inner: MockEmbedder,
    calls: AtomicUsize,
}

impl CountingEmbedder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MockEmbedder::new(EMBED_DIM),
            calls: AtomicUsize::new(0),
        })
    }

    /// The calls since the last `take`, resetting the count.
    fn take(&self) -> usize {
        self.calls.swap(0, Ordering::SeqCst)
    }
}

#[async_trait]
impl Embedder for CountingEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.embed_batch(texts).await
    }

    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
}

/// A dreamer that embeds with `embedder` and touches no network.
///
/// The recall benchmark stays ON: it embeds too, so a settled cycle must skip
/// it as well as dedup. The semantic phase is off (it would read the ambient
/// `OPENROUTER_API_KEY`), and so is the kg.redb compaction.
fn counting_dreamer(embedder: &Arc<CountingEmbedder>) -> Dreamer {
    let config = DreamConfig {
        semantic: SemanticConsolidationConfig {
            enabled: false,
            ..Default::default()
        },
        recall_benchmark_enabled: true,
        compact: false,
        ..DreamConfig::default()
    };
    Dreamer::new(config).with_embedder(Arc::clone(embedder) as Arc<dyn Embedder + Send + Sync>)
}

/// A palace under a leaked tempdir, registered in a fresh registry.
async fn open_palace(name: &str) -> (PalaceRegistry, PathBuf, Arc<PalaceHandle>) {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::mem::forget(dir);
    let registry = PalaceRegistry::new();
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: Utc::now(),
        data_dir: root.join(name),
    };
    let handle = registry
        .create_palace(&root, palace)
        .expect("create palace");
    (registry, root, handle)
}

/// Write `content` through the real write path.
async fn remember(handle: &Arc<PalaceHandle>, content: &str) -> uuid::Uuid {
    handle
        .remember(content.into(), RoomType::Backend, vec![], 0.7)
        .await
        .expect("remember")
}

/// Three distinct drawers: nothing to merge, prune, or content-prune.
async fn seed(handle: &Arc<PalaceHandle>) {
    for content in [
        "The dream loop runs once per idle interval on each resident palace.",
        "Redb stores each palace's drawers in a single file on disk.",
        "The recall path ranks hits by blended lexical and vector score.",
    ] {
        remember(handle, content).await;
    }
}

fn data_dir(handle: &PalaceHandle) -> &Path {
    handle
        .data_dir
        .as_deref()
        .expect("palace handle has a data dir")
}

/// Why: this is the #9391 defect. Pre-fix the second cycle re-embeds the whole
/// palace although nothing changed.
/// What: two cycles on an unchanged palace. The first embeds and records the
/// settled marker; the second makes zero `embed_batch` calls and still
/// reports a completed cycle.
/// Test: itself.
#[tokio::test]
async fn a_second_cycle_on_an_unchanged_palace_embeds_nothing() {
    let (_registry, _root, handle) = open_palace("settled-unchanged").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    let dreamer = counting_dreamer(&embedder);

    let first = dreamer.dream_cycle(&handle).await.expect("first cycle");
    assert_eq!(first.merged + first.pruned + first.content_pruned, 0);
    assert!(embedder.take() > 0, "the first cycle dreams the palace");
    assert!(
        data_dir(&handle).join(settled::FILE_NAME).exists(),
        "a cycle that changed nothing records the settled marker"
    );

    let second = dreamer.dream_cycle(&handle).await.expect("second cycle");
    assert_eq!(
        embedder.take(),
        0,
        "a cycle on an unchanged palace must make no embed_batch call"
    );
    assert_eq!(second.drawers_after, 3, "the skipped cycle still completes");
}

/// Why: the skip must never hide a write. A forget leaves no timestamp, so it
/// is checked as well as an add.
/// What: settles the palace, then remembers a new drawer — the next cycle
/// embeds. Settles again, then forgets a drawer — the next cycle embeds.
/// Test: itself.
#[tokio::test]
async fn a_write_between_cycles_makes_the_next_cycle_embed() {
    let (_registry, _root, handle) = open_palace("settled-written").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    let dreamer = counting_dreamer(&embedder);
    dreamer.dream_cycle(&handle).await.expect("settling cycle");
    embedder.take();

    let added = remember(
        &handle,
        "A write after the last dream run must be dreamed on the next one.",
    )
    .await;
    dreamer.dream_cycle(&handle).await.expect("cycle after add");
    assert!(embedder.take() > 0, "a remembered drawer must be dreamed");

    dreamer
        .dream_cycle(&handle)
        .await
        .expect("re-settling cycle");
    assert_eq!(embedder.take(), 0, "the palace settles again");

    handle.forget(added).await.expect("forget");
    dreamer
        .dream_cycle(&handle)
        .await
        .expect("cycle after forget");
    assert!(embedder.take() > 0, "a forgotten drawer must be dreamed");
}

/// Why: the Fail-Open Check. A marker the cycle cannot read must make it run,
/// never skip the palace forever.
/// What: settles the palace, overwrites `dream_settled.json` with bytes that
/// do not parse, and asserts the next cycle embeds. That cycle rewrites a
/// valid marker, so the one after it skips again.
/// Test: itself.
#[tokio::test]
async fn an_unreadable_marker_makes_the_cycle_run() {
    let (_registry, _root, handle) = open_palace("settled-unreadable").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    let dreamer = counting_dreamer(&embedder);
    dreamer.dream_cycle(&handle).await.expect("settling cycle");
    embedder.take();

    let marker = data_dir(&handle).join(settled::FILE_NAME);
    std::fs::write(&marker, b"{ not json").expect("corrupt the marker");
    dreamer
        .dream_cycle(&handle)
        .await
        .expect("cycle on corrupt marker");
    assert!(
        embedder.take() > 0,
        "an unreadable settled marker must make the cycle run"
    );

    dreamer
        .dream_cycle(&handle)
        .await
        .expect("cycle on healed marker");
    assert_eq!(embedder.take(), 0, "the full cycle rewrote a valid marker");
}

/// Why: a daemon restart must not turn every palace's next cycle into a full
/// re-embed. The marker is a file and the fingerprint is computed from the
/// drawer table reloaded from disk, so both survive.
/// What: settles the palace, drops and idle-evicts its handle, reopens it from
/// disk, and runs a cycle with a new dreamer: zero embed calls.
/// Test: itself.
#[tokio::test]
async fn the_settled_marker_survives_a_palace_reopen() {
    let (registry, root, handle) = open_palace("settled-reopen").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    counting_dreamer(&embedder)
        .dream_cycle(&handle)
        .await
        .expect("settling cycle");
    embedder.take();

    handle.last_accessed.store(0, Ordering::Relaxed);
    let id = handle.id.clone();
    drop(handle);
    assert_eq!(
        registry.evict_idle(std::time::Duration::from_secs(1)),
        1,
        "the palace is evicted to disk"
    );
    let reopened = registry.open_palace(&root, &id).expect("reopen palace");

    counting_dreamer(&embedder)
        .dream_cycle(&reopened)
        .await
        .expect("cycle after reopen");
    assert_eq!(
        embedder.take(),
        0,
        "a reopened, unchanged palace must not be re-embedded"
    );
}

// ── #9391 fix round: the completion gate and the config fold ────────────────

/// Whether `handle`'s data dir holds a settled marker.
fn is_marked(handle: &PalaceHandle) -> bool {
    data_dir(handle).join(settled::FILE_NAME).exists()
}

/// Two drawers with the same text: the dedup pass merges them.
async fn seed_duplicate_pair(handle: &Arc<PalaceHandle>) {
    for _ in 0..2 {
        remember(
            handle,
            "The dream dedup pass merges two drawers that hold the same note.",
        )
        .await;
    }
}

/// The two drawer ids of [`seed_duplicate_pair`].
fn pair_ids(handle: &PalaceHandle) -> Vec<uuid::Uuid> {
    handle.drawers.read().iter().map(|d| d.id).collect()
}

/// Which redb ops [`stall_redb_ops`] stalls.
type StallWhen = Arc<dyn Fn(&BatchWriteOp) -> bool + Send + Sync>;

/// Make every redb `op` matched by `stalls` outlive a 100 ms transaction
/// budget, so that write fails; `None` clears both hooks.
fn stall_redb_ops(handle: &PalaceHandle, stalls: Option<StallWhen>) {
    let store = handle.kg.redb_store();
    let hooks = store.test_hooks();
    match stalls {
        Some(stalls) => {
            *hooks.txn_budget.lock().expect("hook lock") = Some(Duration::from_millis(100));
            *hooks.after_batch_op.lock().expect("hook lock") =
                Some(Arc::new(move |op: &BatchWriteOp| {
                    if stalls(op) {
                        std::thread::sleep(Duration::from_millis(400));
                    }
                }));
        }
        None => {
            *hooks.after_batch_op.lock().expect("hook lock") = None;
            *hooks.txn_budget.lock().expect("hook lock") = None;
        }
    }
}

/// Why (HIGH 1): a failed merge persist was logged and counted as no merge,
/// so the cycle looked unchanged and settled a palace that still held the
/// duplicate pair. One transient redb error then skipped dedup for good.
/// What: stalls the survivor's redb upsert so the merge cannot persist. That
/// cycle must record no marker, and the next cycle, with redb healthy, must
/// embed and merge the pair.
/// Test: itself.
#[tokio::test]
async fn a_failed_merge_persist_does_not_settle_the_palace() {
    let (_registry, _root, handle) = open_palace("settled-persist-fails").await;
    seed_duplicate_pair(&handle).await;
    let pair = pair_ids(&handle);
    let embedder = CountingEmbedder::new();
    let dreamer = counting_dreamer(&embedder);

    stall_redb_ops(
        &handle,
        Some(Arc::new(
            move |op: &BatchWriteOp| matches!(op, BatchWriteOp::UpsertDrawer(d) if pair.contains(&d.id)),
        )),
    );
    let failed = dreamer.dream_cycle(&handle).await.expect("failing cycle");
    stall_redb_ops(&handle, None);
    assert_eq!(failed.merged, 0, "precondition: the merge did not persist");
    assert!(
        !is_marked(&handle),
        "a cycle whose merge failed must not settle the palace"
    );

    embedder.take();
    let retried = dreamer.dream_cycle(&handle).await.expect("retry cycle");
    assert!(embedder.take() > 0, "the next cycle must run dedup again");
    assert_eq!(retried.merged, 1, "the retried cycle merges the pair");
}

/// Why (HIGH 1): a merge whose loser cannot be evicted leaves a duplicate
/// behind, so that cycle must not settle the palace either.
/// What: stalls the loser's redb delete so its evict fails after the merge
/// persisted. That cycle records no marker; the next one embeds.
/// Test: itself.
#[tokio::test]
async fn a_failed_loser_evict_does_not_settle_the_palace() {
    let (_registry, _root, handle) = open_palace("settled-evict-fails").await;
    seed_duplicate_pair(&handle).await;
    let pair = pair_ids(&handle);
    let embedder = CountingEmbedder::new();
    let dreamer = counting_dreamer(&embedder);

    stall_redb_ops(
        &handle,
        Some(Arc::new(
            move |op: &BatchWriteOp| matches!(op, BatchWriteOp::DeleteDrawer(id) if pair.contains(id)),
        )),
    );
    dreamer.dream_cycle(&handle).await.expect("failing cycle");
    stall_redb_ops(&handle, None);
    assert_eq!(
        handle.drawers.read().len(),
        2,
        "precondition: the loser was not evicted"
    );
    assert!(
        !is_marked(&handle),
        "a cycle whose evict failed must not settle the palace"
    );

    embedder.take();
    dreamer.dream_cycle(&handle).await.expect("retry cycle");
    assert!(embedder.take() > 0, "the next cycle must run dedup again");
}

/// A dreamer with the semantic phase on under `model`, embedding with
/// `embedder`. `consolidator` replaces the one built from config when set.
fn semantic_dreamer(
    embedder: &Arc<CountingEmbedder>,
    model: &str,
    consolidator: Option<Arc<SemanticConsolidator>>,
) -> Dreamer {
    let semantic = SemanticConsolidationConfig {
        enabled: true,
        model: model.to_string(),
        ..Default::default()
    };
    let config = DreamConfig {
        semantic: semantic.clone(),
        recall_benchmark_enabled: true,
        compact: false,
        local_model_enabled: true,
        openrouter_api_key: String::new(),
        ..DreamConfig::default()
    };
    let dreamer = match consolidator {
        Some(c) => Dreamer::with_consolidator(config, c),
        None => Dreamer::new(config),
    };
    dreamer.with_embedder(Arc::clone(embedder) as Arc<dyn Embedder + Send + Sync>)
}

/// A consolidator whose backend answers every batch with no actions.
fn no_op_consolidator() -> Arc<SemanticConsolidator> {
    Arc::new(SemanticConsolidator::new(
        Arc::new(MockInference::no_op()),
        SemanticConsolidationConfig {
            enabled: true,
            ..Default::default()
        },
    ))
}

/// An inference backend whose every call fails.
struct FailingInference;

#[async_trait]
impl Inference for FailingInference {
    fn name(&self) -> &str {
        "failing"
    }

    async fn consolidate(&self, _drawers: &[Drawer]) -> Result<Vec<ConsolidationAction>> {
        anyhow::bail!("injected inference failure")
    }
}

/// Why (HIGH 2): a semantic phase whose config fails to build parks with zero
/// counts, which read as "changed nothing". The palace settled, and stayed
/// settled after the key or model was fixed.
/// What: semantic on with a model the local provider rejects. The first cycle
/// parks the phase, the second runs parked; neither records a marker, and the
/// third still embeds.
/// Test: itself.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn a_parked_consolidator_does_not_settle_the_palace() {
    let _key = EnvVarGuard::remove("OPENROUTER_API_KEY");
    let (_registry, _root, handle) = open_palace("settled-parked").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    let dreamer = semantic_dreamer(&embedder, "ollama/anthropic/claude-haiku-4-5", None);

    dreamer
        .dream_cycle(&handle)
        .await
        .expect("config-error cycle");
    assert!(
        dreamer.is_semantic_consolidation_disabled(),
        "precondition: the unusable config parked the phase"
    );
    assert!(
        !is_marked(&handle),
        "a cycle whose semantic config failed must not settle the palace"
    );
    dreamer.dream_cycle(&handle).await.expect("parked cycle");
    assert!(
        !is_marked(&handle),
        "a cycle with a parked semantic phase must not settle the palace"
    );

    embedder.take();
    dreamer.dream_cycle(&handle).await.expect("third cycle");
    assert!(embedder.take() > 0, "the palace must not be skipped");
}

/// Why (HIGH 2): `consolidate` absorbs an inference error and returns an empty
/// result, which read as a pass that found nothing to consolidate.
/// What: an injected consolidator whose backend always fails. The cycle
/// records no marker and the next cycle embeds.
/// Test: itself.
#[tokio::test]
async fn an_inference_error_does_not_settle_the_palace() {
    let (_registry, _root, handle) = open_palace("settled-inference-error").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    let failing = Arc::new(SemanticConsolidator::new(
        Arc::new(FailingInference),
        SemanticConsolidationConfig {
            enabled: true,
            ..Default::default()
        },
    ));
    let dreamer = semantic_dreamer(&embedder, "openai/gpt-4o-mini", Some(failing));

    let stats = dreamer.dream_cycle(&handle).await.expect("cycle");
    assert_eq!(stats.semantic_llm_calls, 0, "precondition: the call failed");
    assert!(
        !is_marked(&handle),
        "a cycle whose inference failed must not settle the palace"
    );

    embedder.take();
    dreamer.dream_cycle(&handle).await.expect("next cycle");
    assert!(embedder.take() > 0, "the next cycle must run again");
}

/// Why (HIGH 2): a palace settled while the semantic phase was off was never
/// consolidated once the phase was turned on.
/// What: settles the palace with the phase off, then cycles a dreamer with it
/// on: that cycle must embed.
/// Test: itself.
#[tokio::test]
async fn enabling_semantic_consolidation_makes_the_next_cycle_run() {
    let (_registry, _root, handle) = open_palace("settled-semantic-on").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    counting_dreamer(&embedder)
        .dream_cycle(&handle)
        .await
        .expect("settling cycle");
    assert!(is_marked(&handle), "precondition: the palace settled");
    embedder.take();

    semantic_dreamer(&embedder, "openai/gpt-4o-mini", Some(no_op_consolidator()))
        .dream_cycle(&handle)
        .await
        .expect("cycle with semantic on");
    assert!(
        embedder.take() > 0,
        "turning the semantic phase on must make the next cycle run"
    );
}

/// Why (HIGH 2): a palace settled under one semantic model must be dreamed
/// again under another.
/// What: a completed semantic pass under model A settles the palace and the
/// next cycle under A skips. A cycle under model B must embed.
/// Test: itself.
#[tokio::test]
async fn changing_the_semantic_model_makes_the_next_cycle_run() {
    let (_registry, _root, handle) = open_palace("settled-semantic-model").await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    let model_a = semantic_dreamer(&embedder, "openai/model-a", Some(no_op_consolidator()));
    model_a.dream_cycle(&handle).await.expect("settling cycle");
    assert!(
        is_marked(&handle),
        "a completed semantic pass that changed nothing settles the palace"
    );
    embedder.take();
    model_a.dream_cycle(&handle).await.expect("settled cycle");
    assert_eq!(embedder.take(), 0, "precondition: model A skips");

    semantic_dreamer(&embedder, "openai/model-b", Some(no_op_consolidator()))
        .dream_cycle(&handle)
        .await
        .expect("cycle under model B");
    assert!(
        embedder.take() > 0,
        "changing the semantic model must make the next cycle run"
    );
}

/// Rewrite drawer `id` in the drawer table and redb with `edit`.
async fn rewrite_drawer(
    handle: &Arc<PalaceHandle>,
    id: uuid::Uuid,
    edit: impl FnOnce(&mut Drawer),
) {
    let mut drawer = handle
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .expect("drawer exists");
    edit(&mut drawer);
    handle
        .kg
        .upsert_drawer(&drawer)
        .await
        .expect("persist edit");
    if let Some(row) = handle.drawers.write().iter_mut().find(|d| d.id == id) {
        *row = drawer;
    }
}

/// A settled palace, its dreamer's embedder, and one of its drawer ids.
async fn settled_palace(name: &str) -> (Arc<PalaceHandle>, Arc<CountingEmbedder>, uuid::Uuid) {
    let (_registry, _root, handle) = open_palace(name).await;
    seed(&handle).await;
    let embedder = CountingEmbedder::new();
    counting_dreamer(&embedder)
        .dream_cycle(&handle)
        .await
        .expect("settling cycle");
    assert!(is_marked(&handle), "precondition: the palace settled");
    embedder.take();
    let id = handle.drawers.read()[0].id;
    (handle, embedder, id)
}

/// Why (MEDIUM): dedup and consolidation skip protected drawers, so a drawer
/// that gains or loses protection changes their input.
/// What: marks one drawer of a settled palace as a `Task`; the next cycle embeds.
/// Test: itself.
#[tokio::test]
async fn a_protected_flag_change_makes_the_next_cycle_embed() {
    let (handle, embedder, id) = settled_palace("settled-protected").await;
    rewrite_drawer(&handle, id, |d| d.drawer_type = DrawerType::Task).await;
    counting_dreamer(&embedder)
        .dream_cycle(&handle)
        .await
        .expect("cycle after protect");
    assert!(
        embedder.take() > 0,
        "a protected-flag change must be dreamed"
    );
}

/// Why (MEDIUM): an in-place content edit keeps the drawer id, so only the
/// content digest shows it.
/// What: rewrites one drawer's text with `set_content`; the next cycle embeds.
/// Test: itself.
#[tokio::test]
async fn a_content_edit_makes_the_next_cycle_embed() {
    let (handle, embedder, id) = settled_palace("settled-content-edit").await;
    rewrite_drawer(&handle, id, |d| {
        d.set_content("An edited drawer body must be dreamed again on the next cycle.")
    })
    .await;
    counting_dreamer(&embedder)
        .dream_cycle(&handle)
        .await
        .expect("cycle after edit");
    assert!(embedder.take() > 0, "a content edit must be dreamed");
}

/// Why (MEDIUM): a new dedup threshold can merge pairs the old one kept.
/// What: cycles a settled palace with a lower `dedup_threshold`; it embeds.
/// Test: itself.
#[tokio::test]
async fn a_dedup_threshold_change_makes_the_next_cycle_embed() {
    let (handle, embedder, _) = settled_palace("settled-threshold").await;
    let mut dreamer = counting_dreamer(&embedder);
    dreamer.config.dedup_threshold = 0.9;
    dreamer
        .dream_cycle(&handle)
        .await
        .expect("cycle with new threshold");
    assert!(
        embedder.take() > 0,
        "a dedup-threshold change must make the next cycle run"
    );
}

/// Why (MEDIUM): only a cycle that changed nothing may settle. A cycle that
/// merged drawers started from a corpus that no longer exists.
/// What: a cycle that merges a duplicate pair records no marker.
/// Test: itself.
#[tokio::test]
async fn a_cycle_that_changed_something_does_not_settle_the_palace() {
    let (_registry, _root, handle) = open_palace("settled-changed").await;
    seed_duplicate_pair(&handle).await;
    let embedder = CountingEmbedder::new();
    let stats = counting_dreamer(&embedder)
        .dream_cycle(&handle)
        .await
        .expect("merging cycle");
    assert_eq!(stats.merged, 1, "precondition: the cycle merged the pair");
    assert!(
        !is_marked(&handle),
        "a cycle that changed the palace must not settle it"
    );
}
