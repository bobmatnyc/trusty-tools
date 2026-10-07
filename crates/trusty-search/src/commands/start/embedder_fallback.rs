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
//! What: `FallbackEmbedderAdapter` wraps the primary (Python) embedder and,
//! on every primary embed failure, checks whether the SAME supervisor that
//! owns the primary has itself permanently given up respawning
//! (`Embedder::supervisor_gave_up`, forwarded from
//! `LazyEmbedderHandle::supervisor_gave_up` → `SupervisorHandle::has_given_up`
//! → `EmbedderSupervisor`'s own `should_give_up` check in
//! `trusty_common::embedder_client::supervisor`). Only once that REAL signal
//! fires does it build a Rust ort embedder via `build_ort_stdio_sidecar` and
//! LATCH to it for the remainder of the process lifetime — a one-way switch,
//! never thrashing back to the (already-proven-unreliable) Python sidecar.
//!
//! Why observe the supervisor's own signal rather than count failures here
//! (review finding, PR #3560 HIGH fix): an earlier version of this adapter
//! counted CONSECUTIVE embed failures at this (request) layer and tripped at
//! a threshold meant to approximate the supervisor's `max_restarts` ceiling
//! (`config.max_restarts + 1`). That undercounted safety under the epic's own
//! multi-flight design: `LazyEmbedderHandle::embed_via` clones whatever
//! client is currently in the slot with no wait for a fresh respawn, so
//! several concurrent in-flight requests against one stale client during a
//! SINGLE crash episode each fail independently and could cross the
//! request-count threshold on the very first crash — while the supervisor's
//! own respawn budget was nowhere near exhausted and might well have
//! recovered the sidecar. Reading the supervisor's actual give-up flag makes
//! the trip condition exact, no matter how many requests happen to be in
//! flight when a crash occurs.
//!
//! The trip is logged exactly once at ERROR with the triggering reason. If
//! the fallback build itself keeps failing (e.g. `trusty-embedderd` is also
//! missing), that distinct failure is ALSO logged exactly once — not on
//! every subsequent request — see `FallbackState::build_failed_logged`
//! (review finding, PR #3560 MEDIUM fix).
//!
//! Supervisor changes are limited to one new observability hook: this
//! adapter itself lives entirely in trusty-search, wrapping `Arc<dyn
//! Embedder>` — `EmbedderSupervisor`'s restart/backoff/give-up DECISION logic
//! (`should_give_up`) is untouched; trusty-common only gained the
//! `SupervisorHandle::has_given_up()` non-blocking readback of that decision.
//!
//! Test: `fallback_does_not_trip_while_supervisor_still_retrying`,
//! `fallback_trips_once_supervisor_gave_up_signal_fires`,
//! `fallback_latches_and_never_reverts`,
//! `fallback_propagates_error_when_build_ort_fails`,
//! `fallback_logs_build_failure_exactly_once`,
//! `fallback_does_not_trip_on_concurrent_failures_before_supervisor_gives_up`
//! in this module's `tests`.

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
    /// Set the first time the trip fires so the loud ERROR log fires exactly
    /// once.
    trip_logged: bool,
    /// Set the first time building the Rust ort fallback itself fails, so
    /// THAT distinct failure is also logged exactly once rather than on
    /// every subsequent request while both backends are broken (review
    /// finding, PR #3560 MEDIUM fix — the original gate only covered
    /// `trip_logged`, leaving this branch to log an ERROR per search request
    /// for the daemon's remaining life whenever the Rust fallback build
    /// itself keeps failing).
    build_failed_logged: bool,
}

/// Wraps a primary embedder with a one-way runtime fallback to a secondary
/// embedder once the primary's OWN supervisor permanently gives up.
///
/// See the module doc for the full rationale. Generic over `Embedder` (not
/// tied to the Python sidecar specifically) so the latch logic itself is
/// tested without a real subprocess on either side — `primary` just needs to
/// implement `Embedder::supervisor_gave_up()` truthfully (the default `false`
/// means "no supervisor to observe", which is the correct, safe answer for
/// non-supervised embedders and never trips the latch).
pub(super) struct FallbackEmbedderAdapter {
    primary: Arc<dyn Embedder>,
    build_fallback: Box<FallbackBuilder>,
    state: Mutex<FallbackState>,
}

impl FallbackEmbedderAdapter {
    pub(super) fn new(
        primary: Arc<dyn Embedder>,
        build_fallback: impl Fn() -> Result<Arc<dyn Embedder>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            primary,
            build_fallback: Box::new(build_fallback),
            state: Mutex::new(FallbackState {
                active: None,
                trip_logged: false,
                build_failed_logged: false,
            }),
        }
    }

    /// `Some(fallback)` if the latch is already tripped.
    fn active_fallback(&self) -> Option<Arc<dyn Embedder>> {
        self.state.lock().unwrap().active.clone()
    }

    /// Record one primary-embedder failure and trip the latch iff the
    /// primary's OWN supervisor has permanently given up
    /// (`Embedder::supervisor_gave_up()`) — see the module doc for why this
    /// replaced an independently-counted request-failure threshold. Returns
    /// `Some(fallback)` once tripped (building it right here); returns `None`
    /// while the supervisor is still retrying (mid-respawn or mid-backoff) OR
    /// if the fallback build itself failed (the caller then propagates the
    /// ORIGINAL primary error — see call sites below).
    fn record_failure_and_maybe_trip(
        &self,
        primary_err: &anyhow::Error,
    ) -> Option<Arc<dyn Embedder>> {
        let mut guard = self.state.lock().unwrap();
        if let Some(fb) = &guard.active {
            return Some(Arc::clone(fb));
        }
        if !self.primary.supervisor_gave_up() {
            // The supervisor is still trying to recover the sidecar (fresh
            // crash not yet detected, mid-respawn, or mid-backoff) — this
            // request's own failure is real and must propagate, but it is
            // NOT proof the sidecar is permanently dead. Do not trip.
            return None;
        }
        if !guard.trip_logged {
            guard.trip_logged = true;
            tracing::error!(
                "TRUSTY_EMBEDDER=python: the supervisor has permanently given \
                 up respawning the Python/MPS sidecar (exceeded its own \
                 restart ceiling; last request error: {primary_err:#}) — \
                 FALLING BACK to the Rust ort stdio sidecar for the remainder \
                 of this daemon's lifetime (one-way switch; restart to retry \
                 the Python/MPS sidecar)."
            );
        }
        match (self.build_fallback)() {
            Ok(fb) => {
                guard.active = Some(Arc::clone(&fb));
                Some(fb)
            }
            Err(build_err) => {
                // Review finding, PR #3560 MEDIUM fix: gate this branch on
                // its OWN logged-once flag, distinct from `trip_logged` — a
                // Rust-ort build that keeps failing (e.g. `trusty-embedderd`
                // is also missing) is a second, rarer failure mode than the
                // primary's, and must not log an ERROR on every subsequent
                // search request for the daemon's remaining life.
                if !guard.build_failed_logged {
                    guard.build_failed_logged = true;
                    tracing::error!(
                        "TRUSTY_EMBEDDER=python fallback: failed to build the \
                         Rust ort fallback embedder ({build_err:#}) — search \
                         requests will keep failing until this is fixed or \
                         the daemon is restarted"
                    );
                }
                None
            }
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
            Ok(v) => Ok(v),
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
            Ok(v) => Ok(v),
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
    use serial_test::serial;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Test embedder that fails its first `fail_count` calls, then succeeds,
    /// with a `supervisor_gave_up()` answer controlled by a shared flag —
    /// stands in for `LazySlotEmbedderAdapter` forwarding
    /// `LazyEmbedderHandle::supervisor_gave_up()`. `calls` counts total
    /// invocations so tests can assert the fallback (not the primary) served
    /// a given request.
    struct FlakyEmbedder {
        fail_count: u32,
        calls: AtomicU32,
        given_up: Arc<AtomicBool>,
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

        fn supervisor_gave_up(&self) -> bool {
            self.given_up.load(Ordering::SeqCst)
        }
    }

    /// Always-failing embedder whose `supervisor_gave_up()` is controlled by
    /// a shared flag — stands in for a genuinely dead sidecar (flag `true`)
    /// or one the supervisor is still actively respawning (flag `false`).
    struct AlwaysFailEmbedder {
        given_up: Arc<AtomicBool>,
    }

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
        fn supervisor_gave_up(&self) -> bool {
            self.given_up.load(Ordering::SeqCst)
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

    /// Core HIGH-fix regression test: however many times the primary fails,
    /// the latch must NEVER trip while `supervisor_gave_up()` is `false` —
    /// every call's own error must still propagate to the caller. This is
    /// the direct behavioural pin for the fix: the OLD threshold-counting
    /// design would have tripped this after `threshold` failures regardless
    /// of what the supervisor was actually doing.
    ///
    /// Issue #3689: `#[serial]` — every test in this module drives
    /// `record_failure_and_maybe_trip`'s `tracing::error!` call sites, which
    /// `fallback_logs_build_failure_exactly_once` counts via a thread-local
    /// `tracing` subscriber. `tracing`'s per-callsite interest cache is
    /// process-global, so an unrelated concurrently-running test hitting the
    /// SAME call site with no subscriber installed can get it cached as
    /// "never interested", silently dropping events the counting test
    /// expects — see that test's doc comment for the full mechanism.
    #[tokio::test]
    #[serial]
    async fn fallback_does_not_trip_while_supervisor_still_retrying() {
        let given_up = Arc::new(AtomicBool::new(false));
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder {
            given_up: Arc::clone(&given_up),
        });
        let adapter = FallbackEmbedderAdapter::new(primary, || {
            Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>)
        });

        for i in 0..20 {
            let r = adapter.embed_batch(&["a"]).await;
            assert!(
                r.is_err(),
                "call {i}: must still propagate the primary's own error while \
                 the supervisor has not given up, no matter how many failures \
                 have accumulated"
            );
        }
    }

    /// Once `supervisor_gave_up()` flips to `true`, the VERY NEXT failing
    /// call must trip the latch and be served (transparently) by the
    /// fallback rather than surfacing the primary's error.
    // Issue #3689: `#[serial]` — see
    // `fallback_does_not_trip_while_supervisor_still_retrying`'s doc comment.
    #[tokio::test]
    #[serial]
    async fn fallback_trips_once_supervisor_gave_up_signal_fires() {
        let given_up = Arc::new(AtomicBool::new(false));
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder {
            given_up: Arc::clone(&given_up),
        });
        let adapter = FallbackEmbedderAdapter::new(primary, || {
            Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>)
        });

        let r1 = adapter.embed_batch(&["a"]).await;
        assert!(
            r1.is_err(),
            "must propagate the primary's error while the supervisor is \
             still retrying"
        );

        given_up.store(true, Ordering::SeqCst);

        let r2 = adapter.embed_batch(&["b"]).await;
        assert!(
            r2.is_ok(),
            "the first failing call AFTER the supervisor gives up must trip \
             and be served by the fallback, not error out: {r2:?}"
        );
        assert_eq!(
            r2.unwrap(),
            vec![vec![9.0_f32; 4]],
            "must be the fallback's output"
        );
    }

    /// Once tripped, EVERY subsequent call — even if the primary would have
    /// started succeeding again, or if `supervisor_gave_up()` somehow flipped
    /// back to `false` — must go to the fallback. One-way latch.
    // Issue #3689: `#[serial]` — see
    // `fallback_does_not_trip_while_supervisor_still_retrying`'s doc comment.
    #[tokio::test]
    #[serial]
    async fn fallback_latches_and_never_reverts() {
        let given_up = Arc::new(AtomicBool::new(true));
        // Primary fails exactly once then would succeed forever after —
        // proving the latch does not "revert" once the primary recovers.
        let primary: Arc<dyn Embedder> = Arc::new(FlakyEmbedder {
            fail_count: 1,
            calls: AtomicU32::new(0),
            given_up: Arc::clone(&given_up),
            label: "primary",
        });
        let adapter = FallbackEmbedderAdapter::new(primary, || {
            Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>)
        });

        let _ = adapter.embed_batch(&["x"]).await;

        // Flip the signal back to false — must not matter once latched.
        given_up.store(false, Ordering::SeqCst);

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
                 a since-recovered primary or a since-reset give-up signal"
            );
        }
    }

    /// If the fallback build itself fails, the caller must still see the
    /// ORIGINAL primary error (not a fallback-construction error, and not a
    /// panic) — and the trip must be retried on the next call rather than
    /// wedging into a permanently-`None` state.
    // Issue #3689: `#[serial]` — see
    // `fallback_does_not_trip_while_supervisor_still_retrying`'s doc comment.
    #[tokio::test]
    #[serial]
    async fn fallback_propagates_error_when_build_ort_fails() {
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder {
            given_up: Arc::new(AtomicBool::new(true)),
        });
        let adapter = FallbackEmbedderAdapter::new(primary, || {
            anyhow::bail!("simulated: trusty-embedderd binary not found")
        });

        let r = adapter.embed_batch(&["a"]).await;
        assert!(
            r.is_err(),
            "when the fallback itself cannot be built, the original primary \
             error must still propagate rather than panicking or hanging"
        );
    }

    /// Review finding (PR #3560 MEDIUM fix): when BOTH backends are broken
    /// (supervisor gave up on the primary AND the Rust ort fallback build
    /// itself keeps failing), the "fallback build failed" ERROR must log
    /// exactly once — not once per search request — even though the build is
    /// still retried (not cached) on every call.
    ///
    /// Why: the original gate only covered `trip_logged`; the `Err(build_err)`
    /// branch logged unconditionally, so this exact "both backends broken"
    /// case — precisely where log discipline matters most, since it can
    /// persist for the daemon's remaining life — spammed one ERROR per
    /// request.
    /// What: a minimal counting `tracing_subscriber::Layer` records every
    /// event observed while driving several failing calls through the
    /// adapter; asserts the count is exactly 2 (the one-time "supervisor
    /// gave up" trip log, plus the one-time "fallback build failed" log) even
    /// though `embed_batch` is called many times and the fallback builder is
    /// invoked on every one of them.
    ///
    /// Issue #3689: this test installs a thread-local `tracing` subscriber
    /// via `tracing::subscriber::with_default` and asserts an EXACT count of
    /// events dispatched through it. `tracing`'s per-callsite interest cache
    /// is process-global, not per-thread: EVERY other test in this module
    /// drives `record_failure_and_maybe_trip`'s SAME `tracing::error!` call
    /// sites (the "supervisor gave up" / "fallback build failed" logs) with
    /// no subscriber installed, and if one of them evaluates a call site's
    /// interest concurrently with this test's subscriber install/teardown,
    /// the cache can end up registering "never interested" — silently
    /// dropping an event this test expects to count (flaked under
    /// --test-threads=16 looping: "expected exactly 2 ... got 1", and
    /// reproduced locally even with only THIS test `#[serial]`-tagged, until
    /// every sibling in the module was too). `#[serial]` (the crate's
    /// existing convention for isolating tests from cross-test shared
    /// process state — see `service::reindex::root_hijack_tests`,
    /// `commands::doctor_checks::tests::doctor_data_dir_returns_non_empty_path`
    /// #3673/#3686), applied to every test in this module (they all share the
    /// same call sites), gives this test exclusive execution so its
    /// interest-cache-affecting subscriber install cannot race a
    /// concurrently-running sibling.
    /// Test: this test.
    #[tokio::test]
    #[serial]
    async fn fallback_logs_build_failure_exactly_once() {
        use tracing_subscriber::layer::SubscriberExt;

        struct CountingLayer(Arc<AtomicU32>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountingLayer {
            fn on_event(
                &self,
                _event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let event_count = Arc::new(AtomicU32::new(0));
        let layer = CountingLayer(Arc::clone(&event_count));
        let subscriber = tracing_subscriber::registry().with(layer);

        let build_attempts = Arc::new(AtomicU32::new(0));
        let build_attempts_for_closure = Arc::clone(&build_attempts);
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder {
            given_up: Arc::new(AtomicBool::new(true)),
        });
        let adapter = FallbackEmbedderAdapter::new(primary, move || {
            build_attempts_for_closure.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("simulated: trusty-embedderd binary also not found")
        });

        tracing::subscriber::with_default(subscriber, || {
            futures::executor::block_on(async {
                for _ in 0..10 {
                    let r = adapter.embed_batch(&["a"]).await;
                    assert!(r.is_err(), "both backends are broken — must error");
                }
            });
        });

        assert!(
            build_attempts.load(Ordering::SeqCst) >= 10,
            "the fallback build must still be RETRIED on every call (not \
             cached) — only its ERROR LOG is gated to once; got {} attempts",
            build_attempts.load(Ordering::SeqCst)
        );
        assert_eq!(
            event_count.load(Ordering::SeqCst),
            2,
            "expected exactly 2 tracing events across 10 failing calls: one \
             'supervisor gave up, falling back' trip log, and one 'fallback \
             build failed' log — not one build-failed log per request"
        );
    }

    /// Search must keep returning results after the fallback trips — the
    /// end-to-end contract this whole module exists for.
    ///
    /// Issue #3689: `#[serial]` — see
    /// `fallback_does_not_trip_while_supervisor_still_retrying`'s doc comment.
    #[tokio::test]
    #[serial]
    async fn search_never_hard_fails_after_fallback_trip() {
        let primary: Arc<dyn Embedder> = Arc::new(AlwaysFailEmbedder {
            given_up: Arc::new(AtomicBool::new(true)),
        });
        let adapter = FallbackEmbedderAdapter::new(primary, || {
            Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>)
        });

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

    // ── Reviewer-required concurrency proof (HIGH fix) ──────────────────

    /// Hang guard for each test-driven step below (#3569). A passing run
    /// never waits it out: every step ends on an event the test observes.
    #[cfg(unix)]
    const CYCLE_HANG_GUARD_SECS: u64 = 120;

    /// How many times the mock in
    /// `fallback_does_not_trip_on_concurrent_failures_before_supervisor_gives_up`
    /// has been spawned by the supervisor (its own counter file; `0` before
    /// the first spawn).
    #[cfg(unix)]
    fn mock_spawns(counter: &std::path::Path) -> u32 {
        std::fs::read_to_string(counter)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Wait until `done` holds, failing at once if the mock was spawned more
    /// than `max_spawns` times.
    ///
    /// Why (#3569): the verdict is decided by spawn counts, not by how long a
    /// loaded host takes. A supervisor that keeps respawning instead of giving
    /// up fails on the first extra spawn; [`CYCLE_HANG_GUARD_SECS`] only ends a
    /// run in which the supervisor stopped making progress.
    #[cfg(unix)]
    async fn await_cycle(
        counter: &std::path::Path,
        max_spawns: u32,
        step: &str,
        mut done: impl FnMut() -> bool,
    ) {
        let progressed = tokio::time::timeout(
            std::time::Duration::from_secs(CYCLE_HANG_GUARD_SECS),
            async {
                loop {
                    let spawns = mock_spawns(counter);
                    assert!(
                        spawns <= max_spawns,
                        "{step}: the supervisor spawned the mock {spawns} times, \
                         more than {max_spawns} — it kept respawning instead of \
                         giving up at max_restarts=1"
                    );
                    if done() {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await;
        assert!(
            progressed.is_ok(),
            "{step}: the supervisor neither advanced nor gave up within \
             {CYCLE_HANG_GUARD_SECS}s"
        );
    }

    /// Run the mock once with `--warm` and wait for its answer.
    ///
    /// Why (#3569, same cause as #9240): the first exec of a freshly written
    /// script is the slow one, and a first spawn whose startup probe timed
    /// out left the old marker-file mock unable to ever reach the supervisor.
    /// Paying that cost here keeps the supervisor's first spawn a warm exec.
    #[cfg(unix)]
    async fn warm_mock(script: &std::path::Path) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(CYCLE_HANG_GUARD_SECS),
            async {
                let mut child = trusty_common::spawn_retry::retry_on_etxtbsy_async(|| {
                    tokio::process::Command::new(script)
                        .arg("--warm")
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::null())
                        .kill_on_drop(true)
                        .spawn()
                })
                .await
                .expect("spawn the mock for its warm-up");
                let mut stdin = child.stdin.take().expect("piped stdin");
                stdin
                    .write_all(b"warm\n")
                    .await
                    .expect("write warm-up line");
                drop(stdin);
                let mut line = String::new();
                BufReader::new(child.stdout.take().expect("piped stdout"))
                    .read_line(&mut line)
                    .await
                    .expect("read warm-up answer");
                let _ = child.wait().await;
                line
            },
        )
        .await
        .expect("the mock never answered its warm-up");
        assert!(answer.contains("embeddings"), "warm-up answered {answer:?}");
    }

    /// Creates the release file on drop, so a respawn the mock is holding
    /// exits even when the test fails before releasing it.
    #[cfg(unix)]
    struct ReleaseOnDrop(std::path::PathBuf);

    #[cfg(unix)]
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"");
        }
    }

    /// The latch must not trip on concurrent failures during one crash
    /// episode, and must trip once the real supervisor gives up.
    ///
    /// Why: the adapter tests above use synthetic fakes and cannot catch the
    /// concurrency bug the OLD threshold-counting design had — several
    /// requests failing at once against one dead client crossed a count
    /// threshold on the first crash, while the supervisor's restart budget
    /// was nowhere near exhausted. This test drives the real
    /// `LazyEmbedderHandle` → `EmbedderSupervisor` → `StdioEmbedderClient`
    /// stack against a mock child process.
    ///
    /// #3569: the test now drives both crash-restart cycles itself instead of
    /// polling for give-up under a wall-clock ceiling. The old mock marked
    /// itself "spawned once" before answering its first startup probe; when
    /// that probe timed out on a loaded host, every later spawn exited at once,
    /// no supervisor ever started, and the 45 s poll expired.
    ///
    /// What: the mock counts its spawns in a file. Spawn 1 answers its
    /// startup probe and exits 1 (crash cycle 1). Every later spawn waits for
    /// a release file the test creates, then exits without answering, so its
    /// probe fails (crash cycle 2). With `max_restarts: 1` the supervisor
    /// must give up after exactly two spawns.
    ///   1. Warm the mock, then fire 8 concurrent `embed_batch` calls. None
    ///      may be served by the fallback.
    ///   2. Await spawn 2 (the supervisor saw cycle 1 and is respawning). The
    ///      supervisor must not have given up, and a call still propagates the
    ///      primary error.
    ///   3. Release spawn 2 and await the supervisor's give-up. A third spawn
    ///      fails the test at once.
    ///   4. A call is now served by the fallback.
    ///
    /// Test: this test.
    ///
    /// Issue #3689: `#[serial]` — see
    /// `fallback_does_not_trip_while_supervisor_still_retrying`'s doc comment.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn fallback_does_not_trip_on_concurrent_failures_before_supervisor_gives_up() {
        use crate::commands::start::embedder::LazySlotEmbedderAdapter;
        use crate::service::embedder_supervisor::{LazyEmbedderHandle, SupervisorConfig};
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create tempdir");
        let script_path = dir.path().join("mock-embedderd-two-cycles.sh");
        let counter_path = dir.path().join("spawns");
        let release_path = dir.path().join("release-cycle-2");
        let _release_on_drop = ReleaseOnDrop(release_path.clone());
        std::fs::write(
            &script_path,
            format!(
                r#"#!/bin/sh
if [ "$1" = "--warm" ]; then
  IFS= read -r line
  printf '{{"jsonrpc":"2.0","result":{{"embeddings":[[0.1]]}},"id":1}}\n'
  exit 0
fi
COUNTER="{counter}"
n=$(( $(cat "$COUNTER" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$COUNTER.tmp" && mv "$COUNTER.tmp" "$COUNTER"
if [ "$n" -gt 1 ]; then
  while [ ! -e "{release}" ]; do sleep 0.02; done
  exit 1
fi
IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
[ -n "$id" ] || id=1
printf '{{"jsonrpc":"2.0","result":{{"embeddings":[[0.1]]}},"id":%s}}\n' "$id"
exit 1
"#,
                counter = counter_path.display(),
                release = release_path.display(),
            ),
        )
        .expect("write mock script");
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod +x");
        warm_mock(&script_path).await;
        assert_eq!(mock_spawns(&counter_path), 0, "the warm-up is not counted");

        let handle = Arc::new(LazyEmbedderHandle::new(
            script_path,
            SupervisorConfig {
                // A ceiling, not a budget: spawn 1 answers at once, and the
                // test releases spawn 2 well inside it.
                startup_timeout_secs: CYCLE_HANG_GUARD_SECS,
                backoff_max_secs: 0,
                max_restarts: 1,
                idle_shutdown_secs: 0,
                ..SupervisorConfig::default()
            },
        ));
        let python_embedder: Arc<dyn Embedder> = Arc::new(LazySlotEmbedderAdapter {
            handle: Arc::clone(&handle),
            is_python: true,
        });
        let adapter = Arc::new(FallbackEmbedderAdapter::new(python_embedder, || {
            Ok(Arc::new(AlwaysOkEmbedder) as Arc<dyn Embedder>)
        }));

        // ── Step 1: 8 concurrent calls against spawn 1 ───────────────────
        //
        // Only one wins the single-flight spawn; all 8 then use the client of
        // a child that exits right after its probe. Cycle 2 is held by the
        // test, so the supervisor cannot give up while these run.
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let adapter = Arc::clone(&adapter);
            tasks.push(tokio::spawn(async move {
                adapter.embed_batch(&["concurrent probe"]).await
            }));
        }
        for (i, t) in tasks.into_iter().enumerate() {
            let r = t.await.expect("task must not panic");
            // An Err is expected (the primary's own failure propagating);
            // the only forbidden outcome is the fallback's output.
            if let Ok(v) = r {
                assert_ne!(
                    v,
                    vec![vec![9.0_f32; 4]],
                    "call {i}: the latch must NOT trip on concurrent failures \
                     during a single crash episode"
                );
            }
        }

        // ── Step 2: the supervisor saw cycle 1 and is respawning ─────────
        await_cycle(&counter_path, 2, "crash cycle 1", || {
            mock_spawns(&counter_path) == 2 || handle.supervisor_gave_up()
        })
        .await;
        assert_eq!(
            mock_spawns(&counter_path),
            2,
            "the supervisor gave up after one crash cycle; max_restarts=1 \
             allows a respawn"
        );
        assert!(
            !handle.supervisor_gave_up(),
            "the supervisor must not give up while the test holds spawn 2"
        );
        let mid = adapter.embed_batch(&["mid-respawn"]).await;
        assert!(
            mid.is_err(),
            "mid-respawn, the primary's error must propagate; got {mid:?}"
        );

        // ── Step 3: release cycle 2 and await the give-up ────────────────
        std::fs::write(&release_path, b"").expect("release spawn 2");
        await_cycle(&counter_path, 2, "crash cycle 2", || {
            handle.supervisor_gave_up()
        })
        .await;

        // ── Step 4: the latch is now tripped ─────────────────────────────
        let r = adapter.embed_batch(&["after give-up"]).await;
        assert_eq!(
            r.unwrap(),
            vec![vec![9.0_f32; 4]],
            "once the real supervisor has given up, the adapter must now be \
             latched to the fallback"
        );
    }
}
