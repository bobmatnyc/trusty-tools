//! Regression tests for #9391: a dream cycle on an unchanged palace does no
//! embedding work.
//!
//! Why: the idle loop re-embedded every drawer of an unchanged palace every
//! 300 s, and each run raised the daemon's RSS by 0.8–1.7 GB. The skip is
//! invisible in `DreamStats` — an unchanged palace merges nothing either way —
//! so these tests count the embedder's calls.
//! What: a counting embedder injected with `Dreamer::with_embedder`; one test
//! per branch of the skip decision in `settled`.
//! Test: itself.

use super::config::DreamConfig;
use super::dreamer::Dreamer;
use super::settled;
use crate::embedder::MockEmbedder;
use crate::memory_core::PalaceRegistry;
use crate::memory_core::embed::{EMBED_DIM, Embedder};
use crate::memory_core::palace::{Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::semantic_consolidation::SemanticConsolidationConfig;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
