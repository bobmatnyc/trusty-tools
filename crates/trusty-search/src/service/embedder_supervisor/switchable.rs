//! Atomically-swappable embedder backend (epic #3524 slice 6, PR 1/5).
//!
//! Why: the embed-pool workers (`service/embed_pool.rs:204,223`) each capture
//! their own `Arc::clone` of the embedder at construction time. Writing a new
//! `Arc<dyn Embedder>` into `SearchAppState::embedder_slot` therefore never
//! reaches an already-running worker — the slot and the pool's captured
//! clones silently diverge. A later slice needs to hot-swap the active
//! backend (ort -> python, or python -> ort on fallback) without restarting
//! the daemon or re-spawning the pool, so every existing owner needs a level
//! of indirection that can be repointed after construction.
//!
//! What: `SwitchableEmbedder` wraps the real backend behind
//! `arc_swap::ArcSwap` and implements `crate::core::Embedder` itself,
//! delegating every call to whichever backend is currently installed. Every
//! existing owner (the `embedder_slot`, the embed-pool workers, the restore
//! path) holds an `Arc<dyn Embedder>` that is actually the `SwitchableEmbedder`
//! — swapping the inner backend via [`SwitchableEmbedder::swap_to`] is
//! instantly visible to all of them with zero call-site changes.
//!
//! This PR is a pure refactor: `build_embedder()` (`commands/start/embedder.rs`)
//! now wraps whatever it builds in a `SwitchableEmbedder` before returning it,
//! but nothing yet calls `swap_to` — the active backend never changes at
//! runtime until a later slice wires up the hot-swap orchestrator. Every
//! existing code path behaves identically to before this PR.
//!
//! ## Why `ArcSwap<Arc<dyn Embedder>>` and not `ArcSwap<dyn Embedder>`
//!
//! `arc_swap::ArcSwap<T>` is `ArcSwapAny<Arc<T>>` — the outer `Arc` is always
//! there; `T` is the payload type stored *inside* that `Arc`. Making `T = dyn
//! Embedder` (i.e. `ArcSwap<dyn Embedder>`, meaning `ArcSwapAny<Arc<dyn
//! Embedder>>`) is exactly what we want conceptually, but this crate's MSRV /
//! trait-object ergonomics make the extra indirection (`T = Arc<dyn
//! Embedder>`, i.e. `ArcSwapAny<Arc<Arc<dyn Embedder>>>`) the cleaner,
//! definitely-compiles choice: it needs no unsized-coercion gymnastics at the
//! `ArcSwap::new` call site, `.load_full()` returns a plain, easy-to-clone
//! `Arc<Arc<dyn Embedder>>` that auto-derefs through both layers to reach the
//! trait's methods, and swaps are just as cheap (one atomic pointer store) —
//! the second `Arc` layer costs one extra pointer-sized allocation per swap,
//! never per read. Reads are wait-free either way: `load()`/`load_full()`
//! never block a concurrent `store()`.
//!
//! Test: `dimension_is_constant_384`, `active_reflects_last_swap`,
//! `concurrent_embed_batch_survives_swap_wait_free` in the `tests` submodule
//! below.

use std::sync::Arc;

use anyhow::Result;
use arc_swap::ArcSwap;
use async_trait::async_trait;

use crate::core::Embedder;
use trusty_common::embedder::ExecutionProvider;

/// Which concrete embedding backend is currently installed behind a
/// [`SwitchableEmbedder`].
///
/// Why: `/health` (PR-2) and the hot-swap orchestrator (PR-3) both need to
/// know *which* backend is live, not just that *an* embedder is live.
/// What: one variant per backend `build_embedder()` can construct today.
/// Test: `active_reflects_last_swap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// The default Rust ONNX-runtime stdio sidecar (`trusty-embedderd`).
    Ort,
    /// The opt-in Python/MPS sidecar (`trusty-embedderd-py`, epic #3524).
    Python,
    /// A manually-managed remote sidecar (HTTP or UDS).
    Remote,
    /// In-process `FastEmbedder` (explicit escape hatch — tests/debugging).
    InProcess,
    /// Candle Metal backend (feature-gated).
    Candle,
}

/// Where a backend transition currently stands.
///
/// Why: the Python/MPS arm eagerly bootstraps a `uv`-managed venv before it
/// can serve a single request; the hot-swap orchestrator (PR-3) needs a way
/// to represent "bootstrap in progress" / "bootstrap failed, still on ort" so
/// `/health` (PR-2) can surface it instead of a bare backend name.
/// What: `NotApplicable` is the state every backend has in THIS PR (no
/// orchestrator exists yet to drive bootstrap). The other variants are
/// unused until PR-3 wires up the orchestrator; they exist now so
/// `ActiveBackend`'s shape does not change again in that PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapState {
    /// No bootstrap step applies to the currently-installed backend (every
    /// backend in this PR — set by `build_embedder()` unconditionally).
    NotApplicable,
    /// A bootstrap (e.g. venv build) is in progress for the NEXT backend.
    Bootstrapping,
    /// The bootstrap completed and the backend is ready to serve.
    Ready,
    /// The bootstrap failed.
    Failed,
    /// The bootstrap failed and the orchestrator fell back to the ort backend.
    FellBackToOrt,
}

/// Descriptive snapshot of the currently-installed embedder backend.
///
/// Why: `SwitchableEmbedder::provider()` needs *some* source of truth for
/// which execution provider is active, and `/health` (PR-2) needs the same
/// information plus the backend kind, model name, and bootstrap status in
/// one place. Storing it alongside the live backend (rather than
/// recomputing it per call) makes both cheap.
/// What: plain data, swapped in lockstep with the backend it describes via
/// [`SwitchableEmbedder::swap_to`].
/// Test: `active_reflects_last_swap`.
#[derive(Debug, Clone)]
pub struct ActiveBackend {
    /// Which backend implementation is live.
    pub kind: BackendKind,
    /// The execution provider that backend resolved (CPU / CoreML / CUDA / …).
    pub provider: ExecutionProvider,
    /// Human-readable model identifier (e.g. `"all-MiniLM-L6-v2"`).
    pub model: String,
    /// Whether the active model is the INT8-quantized variant.
    pub quantized: bool,
    /// Bootstrap status — `NotApplicable` for every backend in this PR.
    pub bootstrap: BootstrapState,
}

/// Wraps a real `Arc<dyn Embedder>` behind an atomically-swappable slot.
///
/// Why: see the module docs — every existing owner of the embedder Arc
/// (embed-pool workers, `embedder_slot`, restore) needs to transparently
/// observe a backend swap without re-fetching a fresh Arc from anywhere.
/// Wrapping the real backend in this struct and handing out
/// `Arc<SwitchableEmbedder>` (coerced to `Arc<dyn Embedder>`) everywhere
/// means [`Self::swap_to`] is the only place that ever needs to change once
/// a hot-swap orchestrator exists (PR-3).
/// What: two independent `ArcSwap` cells — one for the live backend, one for
/// the descriptive [`ActiveBackend`] snapshot — updated together by
/// [`Self::swap_to`]. Reads (`embed`/`embed_batch`/`provider`) never take a
/// lock: `ArcSwap::load`/`load_full` is wait-free with respect to a
/// concurrent `store`.
/// Test: `concurrent_embed_batch_survives_swap_wait_free` proves in-flight
/// calls never error/panic across a swap.
pub struct SwitchableEmbedder {
    inner: ArcSwap<Arc<dyn Embedder>>,
    active: ArcSwap<ActiveBackend>,
}

impl SwitchableEmbedder {
    /// Wrap `inner` as the initial backend, described by `active`.
    ///
    /// Why: `build_embedder()` calls this once per backend arm, immediately
    /// after constructing the raw `Arc<dyn Embedder>`, so every call site
    /// downstream only ever sees the `SwitchableEmbedder`.
    /// What: stores both `inner` and `active` in fresh `ArcSwap` cells.
    /// Test: `dimension_is_constant_384`, `active_reflects_last_swap`.
    pub fn new(inner: Arc<dyn Embedder>, active: ActiveBackend) -> Self {
        Self {
            inner: ArcSwap::new(Arc::new(inner)),
            active: ArcSwap::new(Arc::new(active)),
        }
    }

    /// Atomically replace the live backend and its descriptive snapshot.
    ///
    /// Why: the single mutation point a future hot-swap orchestrator (PR-3)
    /// calls. Not invoked anywhere in this PR — introduced now so the
    /// plumbing compiles and is unit-tested ahead of the orchestrator landing.
    /// What: two independent `store()` calls (backend, then its description).
    /// A reader racing this call always observes either the fully-old or
    /// fully-new backend for `embed`/`embed_batch` — `ArcSwap::store` is a
    /// single atomic pointer write, so there is no torn intermediate state
    /// within either field; the two fields are not swapped as one atomic
    /// unit, so a reader could in principle observe the new backend paired
    /// with the still-old `active` snapshot (or vice versa) for one instant.
    /// That is acceptable here: `active` is descriptive metadata for
    /// `/health`, not a correctness input to `embed`/`embed_batch`.
    /// Test: `active_reflects_last_swap`,
    /// `concurrent_embed_batch_survives_swap_wait_free`.
    pub fn swap_to(&self, new_inner: Arc<dyn Embedder>, new_active: ActiveBackend) {
        self.inner.store(Arc::new(new_inner));
        self.active.store(Arc::new(new_active));
    }

    /// Snapshot the currently-installed backend description.
    ///
    /// Why: `/health` (PR-2) and the hot-swap orchestrator (PR-3) need to
    /// read which backend is live without blocking a concurrent `swap_to`.
    /// What: `ArcSwap::load_full` — wait-free, returns a cheap `Arc` clone.
    /// Test: `active_reflects_last_swap`.
    pub fn active(&self) -> Arc<ActiveBackend> {
        self.active.load_full()
    }

    /// Update only the [`BootstrapState`] of the descriptive snapshot,
    /// leaving the live backend (`inner`) untouched (epic #3524 slice 6,
    /// PR 3/5).
    ///
    /// Why: the graceful-default background orchestrator serves on ort
    /// while a python bootstrap runs; on final bootstrap/probe failure it
    /// must record `Failed` (so `/health` stops reporting `Bootstrapping`
    /// forever) while explicitly staying on the still-installed ort
    /// backend — i.e. `inner` must NOT change. [`Self::swap_to`] always
    /// replaces both fields together, so it is the wrong tool here.
    /// What: reads the current [`ActiveBackend`], clones every field except
    /// `bootstrap`, and stores a fresh snapshot with `bootstrap` replaced.
    ///
    /// Concurrency note (code-critic LOW, epic #3524 slice 6 PR-3 review):
    /// this is a non-atomic read-modify-write on `active` — `self.active()`
    /// then `self.active.store(..)` — NOT a compare-and-swap. Two concurrent
    /// callers (or a concurrent [`Self::swap_to`]) can race: the read in one
    /// call may be stale by the time its store lands, silently clobbering
    /// whatever the other caller/`swap_to` just wrote. Safe today ONLY
    /// because the graceful-default orchestrator (PR-3) is the sole writer
    /// of `bootstrap` transitions and never runs concurrently with itself
    /// (one orchestrator task per daemon lifetime, swap-in only). PR-4
    /// (swap-back-on-death) introduces a SECOND writer of `active` racing
    /// this one — at that point this method must become a real CAS loop
    /// (`ArcSwap::rcu` or a compare-and-swap retry) instead of a plain
    /// load-then-store.
    /// Test: `set_bootstrap_state_leaves_inner_untouched`.
    pub fn set_bootstrap_state(&self, new_state: BootstrapState) {
        let current = self.active();
        self.active.store(Arc::new(ActiveBackend {
            kind: current.kind,
            provider: current.provider,
            model: current.model.clone(),
            quantized: current.quantized,
            bootstrap: new_state,
        }));
    }

    /// Snapshot the currently-installed backend itself.
    ///
    /// Why: shared by `embed`/`embed_batch` so both go through the same
    /// wait-free read path.
    /// What: `ArcSwap::load_full` on the inner cell.
    fn current(&self) -> Arc<Arc<dyn Embedder>> {
        self.inner.load_full()
    }
}

#[async_trait]
impl Embedder for SwitchableEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let backend = self.current();
        backend.embed(text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let backend = self.current();
        backend.embed_batch(texts).await
    }

    /// Constant 384 — every backend `build_embedder()` can construct today
    /// embeds into the same 384-dim space (`trusty_common::embedder::EMBED_DIM`),
    /// so there is no need to read through to the live backend for this.
    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }

    fn provider(&self) -> ExecutionProvider {
        self.active.load().provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal in-memory `Embedder` used only to exercise `SwitchableEmbedder`
    /// without touching any real ONNX/sidecar machinery.
    struct FakeEmbedder {
        calls: AtomicUsize,
    }

    impl FakeEmbedder {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Embedder for FakeEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![text.len() as f32; trusty_common::embedder::EMBED_DIM])
        }

        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            // Yield so concurrent callers interleave with `swap_to` calls
            // rather than running to completion back-to-back.
            tokio::task::yield_now().await;
            Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32; trusty_common::embedder::EMBED_DIM])
                .collect())
        }

        fn dimension(&self) -> usize {
            trusty_common::embedder::EMBED_DIM
        }
    }

    fn test_active(kind: BackendKind) -> ActiveBackend {
        ActiveBackend {
            kind,
            provider: ExecutionProvider::Cpu,
            model: "test-model".to_string(),
            quantized: false,
            bootstrap: BootstrapState::NotApplicable,
        }
    }

    /// `dimension()` must be the constant 384 regardless of which backend is
    /// installed — both real backends embed into the same space.
    #[tokio::test]
    async fn dimension_is_constant_384() {
        let fake: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new());
        let sw = SwitchableEmbedder::new(fake, test_active(BackendKind::Ort));
        assert_eq!(sw.dimension(), 384);
        assert_eq!(sw.dimension(), trusty_common::embedder::EMBED_DIM);
    }

    /// `active()` must reflect the most recent `swap_to` call.
    #[tokio::test]
    async fn active_reflects_last_swap() {
        let fake_a: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new());
        let sw = SwitchableEmbedder::new(fake_a, test_active(BackendKind::Ort));
        assert_eq!(sw.active().kind, BackendKind::Ort);
        assert_eq!(sw.provider(), ExecutionProvider::Cpu);

        let fake_b: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new());
        sw.swap_to(fake_b, test_active(BackendKind::Python));

        assert_eq!(sw.active().kind, BackendKind::Python);
        assert_eq!(sw.active().model, "test-model");
        assert_eq!(sw.active().bootstrap, BootstrapState::NotApplicable);
    }

    /// `set_bootstrap_state` must update only `active.bootstrap`, leaving the
    /// installed backend (`inner`) — and every other `ActiveBackend` field —
    /// unchanged. This is the mechanism the graceful-default orchestrator
    /// (PR-3) uses to mark a failed python bootstrap `Failed` while staying
    /// on the still-installed ort backend.
    #[tokio::test]
    async fn set_bootstrap_state_leaves_inner_untouched() {
        let fake = Arc::new(FakeEmbedder::new());
        let fake_dyn: Arc<dyn Embedder> = Arc::clone(&fake) as Arc<dyn Embedder>;
        let mut active = test_active(BackendKind::Ort);
        active.bootstrap = BootstrapState::Bootstrapping;
        let sw = SwitchableEmbedder::new(fake_dyn, active);

        sw.set_bootstrap_state(BootstrapState::Failed);

        let after = sw.active();
        assert_eq!(after.kind, BackendKind::Ort);
        assert_eq!(after.bootstrap, BootstrapState::Failed);
        assert_eq!(after.model, "test-model");

        // The backend itself must still be the original fake — prove it by
        // observing the call counter increment on a fresh embed call.
        sw.embed("probe").await.expect("embed must still work");
        assert_eq!(fake.calls.load(Ordering::Relaxed), 1);
    }

    /// Proves the wait-free-swap contract under load: many concurrent
    /// `embed_batch` callers run while `swap_to` repeatedly replaces the
    /// backend. No call may ever error, panic, or return a wrong-dimension
    /// vector — a torn/blocking swap would show up as one of those.
    #[tokio::test]
    async fn concurrent_embed_batch_survives_swap_wait_free() {
        let fake_a: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new());
        let sw = Arc::new(SwitchableEmbedder::new(
            fake_a,
            test_active(BackendKind::Ort),
        ));

        let mut handles = Vec::new();
        for i in 0..50 {
            let sw = Arc::clone(&sw);
            handles.push(tokio::spawn(async move {
                let text = format!("text-{i}");
                let refs: [&str; 1] = [text.as_str()];
                let result = sw
                    .embed_batch(&refs)
                    .await
                    .expect("embed_batch must never error across a swap");
                assert_eq!(result.len(), 1, "one vector per input text");
                assert_eq!(result[0].len(), 384, "vector must keep the constant dim");
            }));
        }

        // Swap the backend repeatedly while the above tasks are in flight.
        for _ in 0..10 {
            let fake_b: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new());
            sw.swap_to(fake_b, test_active(BackendKind::Python));
            tokio::task::yield_now().await;
        }

        for h in handles {
            h.await.expect("embed task panicked");
        }

        // The last swap_to call in the loop above installed Python.
        assert_eq!(sw.active().kind, BackendKind::Python);
    }
}
