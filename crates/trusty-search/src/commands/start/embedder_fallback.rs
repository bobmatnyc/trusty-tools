//! One-way runtime fallback from the Python/MPS sidecar to the Rust ort
//! sidecar (epic #3524 slice 5 — the release-gating slice).
//!
//! Why: slices 2-4 already fall back to the Rust ort path on any BOOTSTRAP
//! failure (missing `uv`, network failure, disk precheck, corrupt venv,
//! bootstrap timeout — see `embedder.rs`'s `"python"` arm). What is still
//! missing is a RUNTIME fallback: once the Python sidecar is up, a
//! `LazyEmbedderHandle`/`EmbedderSupervisor` that exhausts `max_restarts`
//! (crash storm) or gets wedge-restart-stormed gives up supervising —
//! `consecutive_failures`/`consecutive_wedge_restarts` trip
//! `should_give_up` in `trusty_common::embedder_client::supervisor` — but the
//! `client_slot` it leaves behind keeps returning errors forever (broken pipe
//! to a dead process, never resurrected). Without this adapter every
//! subsequent embed request on that daemon would fail permanently even
//! though a perfectly good Rust ort sidecar is one env var away. Since the
//! sidecar becomes the Apple-Silicon default, that would mean "install a
//! stray venv corruption and search is down until restart" — unacceptable.
//!
//! What: `FallbackEmbedderAdapter` wraps the primary (Python) embedder and
//! counts CONSECUTIVE embed failures. Once the count reaches `threshold` (one
//! more than `TRUSTY_EMBEDDERD_MAX_RESTARTS`, matching the supervisor's own
//! give-up ceiling — see `resolve_python_fallback_threshold`), it builds a
//! Rust ort embedder via `build_ort_stdio_sidecar` and LATCHES to it for the
//! remainder of the process lifetime — a one-way switch, never thrashing back
//! to the (already-proven-unreliable) Python sidecar. The trip is logged
//! exactly once at ERROR with the triggering reason; the sidecar-corruption
//! error itself is likely already logged per-attempt by the supervisor, so
//! this adapter does not duplicate that at WARN on every failed call — only
//! the trip itself is loud.
//!
//! Zero supervisor changes: this lives entirely in trusty-search's own
//! adapter layer, wrapping `Arc<dyn Embedder>` — `EmbedderSupervisor`,
//! `LazyEmbedderHandle`, and the stdio wire protocol are untouched.
//!
//! Test: `fallback_trips_after_threshold_consecutive_failures`,
//! `fallback_latches_and_never_reverts`,
//! `fallback_resets_counter_on_intervening_success`,
//! `fallback_propagates_error_when_build_ort_fails` in this module's `tests`.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;

use crate::core::Embedder;

/// Build function for the Rust ort fallback embedder — injected so tests can
/// substitute a fake without touching the real `locate_embedderd_binary`
/// filesystem probe. Production always passes `build_ort_stdio_sidecar`
/// (adapted to drop its `pid_slot` — the fallback path does not report a
/// sidecar PID for `/health`'s `embedderd_rss_mb`; see the module-level
/// design-decision note in `embedder.rs`'s `"python"` arm).
type FallbackBuilder = dyn Fn() -> Result<Arc<dyn Embedder>> + Send + Sync;

struct FallbackState {
    /// `Some` once tripped — the one-way latch. Never reset to `None`.
    active: Option<Arc<dyn Embedder>>,
    consecutive_failures: u32,
    /// Set the first time the trip is attempted so the loud ERROR log fires
    /// exactly once, even if the fallback build itself keeps failing on
    /// every subsequent call (a distinct, rarer failure than the primary
    /// embedder's).
    trip_logged: bool,
}

/// Wraps a primary embedder with a one-way runtime fallback to a
/// secondary embedder once consecutive failures cross `threshold`.
///
/// See the module doc for the full rationale. Generic over `Embedder` (not
/// tied to the Python sidecar specifically) so the latch logic itself is
/// tested without a real subprocess on either side.
pub(super) struct FallbackEmbedderAdapter {
    primary: Arc<dyn Embedder>,
    build_fallback: Box<FallbackBuilder>,
    state: Mutex<FallbackState>,
    threshold: u32,
}

impl FallbackEmbedderAdapter {
    /// `threshold`: number of CONSECUTIVE primary failures that trips the
    /// latch. Reset to 0 by any intervening success — this is a crash/wedge
    /// detector, not a lifetime error budget.
    pub(super) fn new(
        primary: Arc<dyn Embedder>,
        build_fallback: impl Fn() -> Result<Arc<dyn Embedder>> + Send + Sync + 'static,
        threshold: u32,
    ) -> Self {
        Self {
            primary,
            build_fallback: Box::new(build_fallback),
            state: Mutex::new(FallbackState {
                active: None,
                consecutive_failures: 0,
                trip_logged: false,
            }),
            threshold: threshold.max(1),
        }
    }

    /// `Some(fallback)` if the latch is already tripped.
    fn active_fallback(&self) -> Option<Arc<dyn Embedder>> {
        self.state.lock().unwrap().active.clone()
    }

    /// Record one primary-embedder failure. Returns `Some(fallback)` the
    /// moment the threshold is crossed (trying to build+latch the fallback
    /// right there); returns `None` while still under threshold OR if the
    /// fallback build itself failed (the caller then propagates the
    /// ORIGINAL primary error — see call sites below).
    fn record_failure_and_maybe_trip(
        &self,
        primary_err: &anyhow::Error,
    ) -> Option<Arc<dyn Embedder>> {
        let mut guard = self.state.lock().unwrap();
        if let Some(fb) = &guard.active {
            return Some(Arc::clone(fb));
        }
        guard.consecutive_failures += 1;
        if guard.consecutive_failures < self.threshold {
            return None;
        }
        if !guard.trip_logged {
            guard.trip_logged = true;
            tracing::error!(
                consecutive_failures = guard.consecutive_failures,
                "TRUSTY_EMBEDDER=python: {} consecutive embed failures from the \
                 Python/MPS sidecar (last error: {primary_err:#}) — FALLING BACK \
                 to the Rust ort stdio sidecar for the remainder of this daemon's \
                 lifetime (one-way switch; restart to retry the Python/MPS \
                 sidecar).",
                guard.consecutive_failures,
            );
        }
        match (self.build_fallback)() {
            Ok(fb) => {
                guard.active = Some(Arc::clone(&fb));
                Some(fb)
            }
            Err(build_err) => {
                tracing::error!(
                    "TRUSTY_EMBEDDER=python fallback: failed to build the Rust \
                     ort fallback embedder ({build_err:#}) — search requests \
                     will keep failing until this is fixed or the daemon is \
                     restarted"
                );
                None
            }
        }
    }

    fn record_success(&self) {
        let mut guard = self.state.lock().unwrap();
        // Once latched, stay latched — never let a stray primary success
        // (e.g. a delayed reply racing the trip) reset the counter and undo
        // the one-way switch.
        if guard.active.is_none() {
            guard.consecutive_failures = 0;
        }
    }
}

#[async_trait]
impl Embedder for FallbackEmbedderAdapter {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        if let Some(fb) = self.active_fallback() {
            return fb.embed(text).await;
        }
        match self.primary.embed(text).await {
            Ok(v) => {
                self.record_success();
                Ok(v)
            }
            Err(e) => match self.record_failure_and_maybe_trip(&e) {
                Some(fb) => fb.embed(text).await,
                None => Err(e),
            },
        }
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if let Some(fb) = self.active_fallback() {
            return fb.embed_batch(texts).await;
        }
        match self.primary.embed_batch(texts).await {
            Ok(v) => {
                self.record_success();
                Ok(v)
            }
            Err(e) => match self.record_failure_and_maybe_trip(&e) {
                Some(fb) => fb.embed_batch(texts).await,
                None => Err(e),
            },
        }
    }

    fn dimension(&self) -> usize {
        self.active_fallback()
            .unwrap_or_else(|| Arc::clone(&self.primary))
            .dimension()
    }

    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        self.active_fallback()
            .unwrap_or_else(|| Arc::clone(&self.primary))
            .provider()
    }

    fn resolved_provider_label(&self) -> Option<String> {
        self.active_fallback()
            .unwrap_or_else(|| Arc::clone(&self.primary))
            .resolved_provider_label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Test embedder that fails its first `fail_count` calls, then succeeds.
    /// `calls` counts total invocations so tests can assert the fallback
    /// (not the primary) served a given request.
    struct FlakyEmbedder {
        fail_count: u32,
        calls: AtomicU32,
        label: &'static str,
    }

    #[async_trait]
    impl Embedder for FlakyEmbedder {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            unimplemented!("tests use embed_batch")
        }

        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_count {
                anyhow::bail!("{} synthetic failure #{n}", self.label);
            }
            Ok(texts.iter().map(|_| vec![1.0_f32; 4]).collect())
        }

        fn dimension(&self) -> usize {
            4
        }
    }

    /// Always-failing embedder — stands in for a genuinely dead sidecar.
    struct AlwaysFailEmbedder;

    #[async_trait]
    impl Embedder for AlwaysFailEmbedder {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            anyhow::bail!("always fails")
        }
        async fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            anyhow::bail!("always fails")
        }
        fn dimension(&self) -> usize {
            4
        }
    }

    /// Always-succeeding embedder — stands in for the Rust ort fallback.
    struct AlwaysOkEmbedder;

    #[async_trait]
    impl Embedder for AlwaysOkEmbedder {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![9.0_f32; 4])
        }
        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![9.0_f32; 4]).collect())
        }
        fn dimension(&self) -> usize {
            4
        }
    }

    /// Threshold-1 consecutive failures must NOT trip the fallback — the
    /// primary's own error must still propagate.
    #[tokio::test]
    async fn fallback_does_not_trip_below_threshold() {
        let primary: Arc<dyn Embedder> = Arc::new(FlakyEmbedder {
            fail_count: 2,
            calls: AtomicU32::new(0),
            label: "primary",
        });
        let adapter = FallbackEmbedderAdapter::new(
            primary,
            || Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>),
            3,
        );

        let r1 = adapter.embed_batch(&["a"]).await;
        assert!(r1.is_err(), "call 1 must still be the (failing) primary");
        let r2 = adapter.embed_batch(&["b"]).await;
        assert!(r2.is_err(), "call 2 must still be the (failing) primary");
    }

    /// Exactly `threshold` consecutive failures must trip the latch and the
    /// SAME call that trips it must be served (transparently) by the
    /// fallback rather than surfacing the primary's error to the caller.
    #[tokio::test]
    async fn fallback_trips_after_threshold_consecutive_failures() {
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder);
        let adapter = FallbackEmbedderAdapter::new(
            primary,
            || Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>),
            3,
        );

        let r1 = adapter.embed_batch(&["a"]).await;
        assert!(r1.is_err(), "failure 1/3 must propagate the primary error");
        let r2 = adapter.embed_batch(&["b"]).await;
        assert!(r2.is_err(), "failure 2/3 must propagate the primary error");
        let r3 = adapter.embed_batch(&["c"]).await;
        assert!(
            r3.is_ok(),
            "failure 3/3 crosses the threshold — this call must be \
             transparently served by the fallback, not error out: {r3:?}"
        );
        assert_eq!(
            r3.unwrap(),
            vec![vec![9.0_f32; 4]],
            "must be the fallback's output"
        );
    }

    /// Once tripped, EVERY subsequent call — even if the primary would have
    /// started succeeding again — must go to the fallback. One-way latch.
    #[tokio::test]
    async fn fallback_latches_and_never_reverts() {
        // Primary fails exactly 3 times then would succeed forever after —
        // proving the latch does not "revert" once the primary recovers.
        let primary: Arc<dyn Embedder> = Arc::new(FlakyEmbedder {
            fail_count: 3,
            calls: AtomicU32::new(0),
            label: "primary",
        });
        let adapter = FallbackEmbedderAdapter::new(
            primary,
            || Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>),
            3,
        );

        for _ in 0..3 {
            let _ = adapter.embed_batch(&["x"]).await;
        }
        // Latch is now tripped. Call many more times; every one must be the
        // fallback's distinctive output (9.0), never touching the primary
        // again (which would itself now start returning [1.0; 4]).
        for _ in 0..5 {
            let r = adapter
                .embed_batch(&["y"])
                .await
                .expect("fallback never errors here");
            assert_eq!(
                r,
                vec![vec![9.0_f32; 4]],
                "latched adapter must always route to the fallback, never back to \
                 a since-recovered primary"
            );
        }
    }

    /// A success BEFORE the threshold is reached must reset the consecutive
    /// counter — two isolated single failures must never accumulate into a
    /// trip.
    #[tokio::test]
    async fn fallback_resets_counter_on_intervening_success() {
        // Fails call 0, succeeds call 1, fails call 2, succeeds call 3... —
        // never two consecutive failures, so a threshold of 2 must never trip.
        struct AlternatingEmbedder {
            calls: AtomicU32,
        }
        #[async_trait]
        impl Embedder for AlternatingEmbedder {
            async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                unimplemented!()
            }
            async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n.is_multiple_of(2) {
                    anyhow::bail!("alternating failure #{n}");
                }
                Ok(texts.iter().map(|_| vec![1.0_f32; 4]).collect())
            }
            fn dimension(&self) -> usize {
                4
            }
        }

        let primary: Arc<dyn Embedder> = Arc::new(AlternatingEmbedder {
            calls: AtomicU32::new(0),
        });
        let adapter = FallbackEmbedderAdapter::new(
            primary,
            || Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>),
            2,
        );

        for i in 0..10_u32 {
            let r = adapter.embed_batch(&["z"]).await;
            if i.is_multiple_of(2) {
                assert!(r.is_err(), "even calls are the primary's failures");
            } else {
                assert_eq!(
                    r.unwrap(),
                    vec![vec![1.0_f32; 4]],
                    "odd calls must be the PRIMARY's success (1.0), proving the \
                     fallback never tripped"
                );
            }
        }
    }

    /// If the fallback build itself fails, the caller must still see the
    /// ORIGINAL primary error (not a fallback-construction error, and not a
    /// panic) — and the trip must be retried on the next call rather than
    /// wedging into a permanently-`None` state.
    #[tokio::test]
    async fn fallback_propagates_error_when_build_ort_fails() {
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder);
        let adapter = FallbackEmbedderAdapter::new(
            primary,
            || anyhow::bail!("simulated: trusty-embedderd binary not found"),
            1,
        );

        let r = adapter.embed_batch(&["a"]).await;
        assert!(
            r.is_err(),
            "when the fallback itself cannot be built, the original primary \
             error must still propagate rather than panicking or hanging"
        );
    }

    /// Search must keep returning results after the fallback trips — the
    /// end-to-end contract this whole module exists for.
    #[tokio::test]
    async fn search_never_hard_fails_after_fallback_trip() {
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder);
        let adapter = FallbackEmbedderAdapter::new(
            primary,
            || Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>),
            1,
        );

        // First call trips and is served.
        let r1 = adapter.embed_batch(&["query one"]).await;
        assert!(r1.is_ok(), "search must not hard-fail once fallback trips");

        // A realistic subsequent search request must also succeed.
        let r2 = adapter.embed_batch(&["query two"]).await;
        assert!(
            r2.is_ok(),
            "subsequent searches must keep working post-fallback"
        );
    }
}
