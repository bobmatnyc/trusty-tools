//! Activity monitor: LLM-backed session state classification with caching.
//!
//! Why: the operator dashboard and circuit-breaker need to know whether a
//! session is working, blocked, or idle. Polling the LLM on every tick is too
//! expensive; the monitor hashes pane content and skips the LLM when content
//! is unchanged.
//! What: [`ActivityMonitor`] manages a per-session [`ActivityCache`] map and
//! delegates classification to an [`LlmClassifier`] trait. The production impl
//! lives in [`super::classifier`] — #4427 moved it out so both files stay under
//! the 500-SLOC cap, and so this file is purely cache/policy with no provider
//! knowledge.
//! Test: `monitor_cache_hit_skips_llm`, `monitor_cache_miss_calls_llm`,
//! `missing_api_key_degrades_to_unknown_verdict`.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::Instant;

use chrono::Utc;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::cache::{ActivityCache, ActivityState, ActivityVerdict, CheckMetrics, CostTally};

/// A per-session sidecar snapshot the coordinator-context handler reads cheaply.
///
/// Why: `build_coordinator_context` runs on every coordinator/context poll and
/// must stay cheap — it can NOT take the async cache `Mutex` (it is synchronous)
/// nor trigger a fresh LLM call for N sessions. This sidecar lets it read the
/// *latest already-computed* summary and a live in-flight flag with a tiny,
/// non-`await` synchronous lock — the same value `/activity` last produced
/// (#1275, DOC-16 §6.2).
/// What: the last genuine LLM-produced summary (absent until one exists) and a
/// `summarizing` flag that is true only while an inference call for the session
/// is actually running.
/// Test: `cached_summary_reflects_llm_verdict`, `summarizing_flag_is_false_when_idle`.
#[derive(Debug, Default, Clone)]
struct SummarySlot {
    /// The most recent genuine LLM-produced summary for the session, if any.
    ///
    /// Populated only from a real classifier verdict — the degraded
    /// "OPENROUTER_API_KEY not configured" placeholder is NEVER stored here, so
    /// an unconfigured daemon leaves this `None` (regression-safe, D1).
    last_summary: Option<String>,
    /// True while an inference call for this session is currently in flight.
    summarizing: bool,
}

/// Errors that can arise during an activity check.
///
/// Why: the monitor and its callers need structured error types to distinguish
/// configuration problems (missing API key) from transient failures — only the
/// former degrades to an `Unknown` verdict instead of failing the check.
/// What: one variant per failure class.
/// Test: `missing_api_key_degrades_to_unknown_verdict` covers the degraded-key
/// path; `classifier_error_propagates` covers the failing path.
#[derive(Debug, Error)]
pub enum ActivityError {
    /// The LLM call failed for an unspecified reason.
    #[error("LLM classification error: {0}")]
    Llm(String),

    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// No inference credential could be resolved for the configured model.
    ///
    /// #4427: the check is no longer env-only — the classifier now resolves
    /// credentials through the shared `trusty_common::inference` ladder (env >
    /// `.env.local` > secure store). The variant name and message are unchanged
    /// so every caller's degrade branch and log line keep working.
    #[error("OPENROUTER_API_KEY is not configured")]
    MissingApiKey,
}

/// Trait for LLM-backed activity classification.
///
/// Why: the monitor must be testable without making real HTTP calls; the trait
/// lets tests inject a stub classifier.
/// What: one async `classify` method that returns a verdict and token counts.
/// Test: `MockClassifier` in the test section implements this.
pub trait LlmClassifier: Send + Sync {
    /// Classify the activity state from pane text.
    ///
    /// Why: the monitor calls this when the content hash has changed and a
    /// fresh LLM verdict is needed.
    /// What: sends `pane_text` to the LLM with a classification prompt and
    /// returns `(verdict, input_tokens, output_tokens)`.
    /// Test: `monitor_cache_miss_calls_llm`.
    fn classify(
        &self,
        pane_text: &str,
    ) -> impl Future<Output = Result<(ActivityVerdict, u32, u32), ActivityError>> + Send;
}

/// Result of a single activity check.
///
/// Why: callers need the full picture — the verdict, cost metrics, whether the
/// cache was hit, and the session's cumulative tally — in one struct.
/// What: bundles all four items returned by [`ActivityMonitor::check`].
/// Test: asserted by `monitor_cache_hit_skips_llm`,
/// `monitor_cache_miss_calls_llm`.
#[derive(Debug)]
pub struct ActivityCheckResult {
    /// The activity verdict (may be from cache or fresh from the LLM).
    pub verdict: ActivityVerdict,
    /// Per-check metrics for this specific call.
    pub cost: CheckMetrics,
    /// True if the verdict was served from the content-hash cache.
    pub cache_hit: bool,
    /// Session's cumulative cost tally after this check.
    pub tally: CostTally,
}

/// Central activity monitoring service.
///
/// Why: a single shared monitor that manages per-session caches avoids
/// creating one LLM client per session and gives a unified view of activity
/// costs across all sessions.
/// What: stores a `HashMap<String, ActivityCache>` behind an async `Mutex`;
/// each `check()` call hashes the pane tail, consults the cache, and either
/// returns the cached verdict or calls the LLM classifier.
/// Test: `monitor_cache_hit_skips_llm`, `monitor_cache_miss_calls_llm`.
pub struct ActivityMonitor<C: LlmClassifier> {
    /// Per-session caches, keyed by session_id string.
    cache: Mutex<HashMap<String, ActivityCache>>,
    /// Per-session sidecar (latest summary + in-flight flag), keyed by session_id.
    ///
    /// Why: held behind a *synchronous* `std::sync::Mutex` so the coordinator
    /// context handler can read it without `.await` and without contending with
    /// the async classification cache; the lock is only ever held for the
    /// microseconds it takes to clone a `String` or read a `bool`, never across
    /// an `.await` point.
    /// Test: `cached_summary_reflects_llm_verdict`.
    summaries: StdMutex<HashMap<String, SummarySlot>>,
    /// LLM classifier used when a cache miss occurs.
    llm: C,
    /// Model name recorded in per-check metrics.
    model: String,
}

impl<C: LlmClassifier> std::fmt::Debug for ActivityMonitor<C> {
    /// Why: `DaemonState` derives `Debug` and holds `Arc<ActivityMonitor<…>>`;
    /// the generic `C` may not implement `Debug` so we provide a manual impl.
    /// What: prints only the model name (the cache and classifier have no useful
    /// debug form and may reference runtime handles).
    /// Test: compile-time only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityMonitor")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl<C: LlmClassifier> ActivityMonitor<C> {
    /// Construct a monitor with the given LLM classifier.
    ///
    /// Why: constructing the monitor with a dependency-injected classifier
    /// keeps it testable without real HTTP calls.
    /// What: initialises an empty cache map and stores the classifier.
    /// Test: used by every monitor unit test.
    pub fn new(llm: C, model: impl Into<String>) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            summaries: StdMutex::new(HashMap::new()),
            llm,
            model: model.into(),
        }
    }

    /// Lock the synchronous sidecar mutex, recovering from poisoning.
    ///
    /// Why: a panic while the `summaries` lock is held would poison it and make
    /// every later `.lock()` return `Err`. The sidecar holds only derived,
    /// regenerable state (the latest summary + an in-flight bool), so panicking
    /// the whole daemon over a poisoned sidecar would be a worse failure than
    /// continuing. We therefore recover the inner guard — but log a `warn!` so
    /// the recovery is visible in the daemon's stderr logs instead of silently
    /// masking a panic that happened elsewhere.
    /// What: returns the locked guard; on poison, emits one `warn!` and returns
    /// the recovered guard (`PoisonError::into_inner`).
    /// Test: exercised by every sidecar accessor test
    /// (`cached_summary_reflects_llm_verdict`, `summarizing_true_during_inference`).
    fn lock_summaries(&self) -> std::sync::MutexGuard<'_, HashMap<String, SummarySlot>> {
        self.summaries.lock().unwrap_or_else(|e| {
            warn!("activity monitor: summaries mutex poisoned, recovering");
            e.into_inner()
        })
    }

    /// Return the latest cached LLM-produced summary for a session, if any.
    ///
    /// Why: the coordinator-context poll (`build_coordinator_context`) needs the
    /// most recent already-computed summary for every session WITHOUT triggering
    /// a fresh LLM call — the per-poll cost must stay bounded regardless of how
    /// many sessions exist (#1275). This reads the sidecar populated by the last
    /// `/activity` call; it never performs inference.
    /// What: synchronously locks the sidecar map and clones the stored summary;
    /// returns `None` when no genuine summary has been produced yet (including
    /// when inference is unconfigured — the degraded placeholder is never stored).
    /// Test: `cached_summary_reflects_llm_verdict`, `cached_summary_none_without_check`.
    pub fn cached_summary(&self, session_id: &str) -> Option<String> {
        let slots = self.lock_summaries();
        slots.get(session_id).and_then(|s| s.last_summary.clone())
    }

    /// Return whether an inference call for a session is currently in flight.
    ///
    /// Why: the TUI blinks a bullet while a session is actively summarizing
    /// (DOC-16 §3.3, D1); the daemon exposes that in-progress signal honestly so
    /// the blink clears the instant the call finishes.
    /// What: synchronously reads the sidecar's `summarizing` flag; `false` when
    /// the session is unknown or no call is running (so an unconfigured daemon
    /// that never starts a real call reports `false`).
    /// Test: `summarizing_flag_is_false_when_idle`, `summarizing_true_during_inference`.
    pub fn is_summarizing(&self, session_id: &str) -> bool {
        let slots = self.lock_summaries();
        slots.get(session_id).is_some_and(|s| s.summarizing)
    }

    /// Set the in-flight `summarizing` flag for a session.
    ///
    /// Why: the cache-miss path flips this true before the LLM call and back to
    /// false after (via [`SummarizingGuard`]) so the sidecar reflects reality
    /// even if the call errors or panics mid-flight.
    /// What: synchronously upserts the session's slot and writes `flag`.
    /// Test: covered by `summarizing_true_during_inference`.
    fn set_summarizing(&self, session_id: &str, flag: bool) {
        let mut slots = self.lock_summaries();
        slots.entry(session_id.to_owned()).or_default().summarizing = flag;
    }

    /// Store a genuine LLM-produced summary for a session.
    ///
    /// Why: the sidecar must surface the *same* text `/activity` returns so the
    /// TUI summary bullet (§4.3) and the cheap coordinator poll agree. Only real
    /// verdicts are stored — the degraded placeholder is filtered out by the
    /// caller so an unconfigured daemon leaves `last_summary` `None`.
    /// What: synchronously upserts the session's slot and writes `summary`.
    /// Test: `cached_summary_reflects_llm_verdict`.
    fn store_summary(&self, session_id: &str, summary: String) {
        let mut slots = self.lock_summaries();
        slots.entry(session_id.to_owned()).or_default().last_summary = Some(summary);
    }

    /// Check the activity state of a session.
    ///
    /// Why: the daemon polls this on a configurable interval; the cache
    /// eliminates LLM calls when nothing has changed in the pane.
    /// What: hashes the last 60 lines of `pane_text`; looks up the per-session
    /// cache and returns the cached verdict with `cache_hit: true` when the hash
    /// matches the last seen hash; otherwise calls `llm.classify(pane_text)`,
    /// updates the cache, and returns the new verdict with `cache_hit: false`.
    /// Test: `monitor_cache_hit_skips_llm`, `monitor_cache_miss_calls_llm`.
    pub async fn check(
        &self,
        session_id: &str,
        pane_text: &str,
    ) -> Result<ActivityCheckResult, ActivityError> {
        let pane_tail = last_n_lines(pane_text, 60);
        let hash = sha256_hex(pane_tail.as_bytes());
        let start = Instant::now();

        let mut caches = self.cache.lock().await;
        let entry = caches
            .entry(session_id.to_owned())
            .or_insert_with(|| ActivityCache::new(&self.model));

        if entry.check_unchanged(&hash) {
            // Cache hit — reuse the last verdict.
            let verdict = entry
                .last_verdict()
                .cloned()
                .unwrap_or_else(|| ActivityVerdict {
                    state: ActivityState::Unknown,
                    summary: "no prior verdict in cache".into(),
                    confidence: 0.0,
                });
            let metrics = CheckMetrics {
                session_id: session_id.to_owned(),
                at: Utc::now(),
                model: self.model.clone(),
                input_tokens: 0,
                output_tokens: 0,
                latency_ms: start.elapsed().as_millis() as u64,
                cache_hit: true,
                verdict_state: verdict.state.clone(),
            };
            let tally = entry.tally().clone();
            entry.update_cache_hit(&hash, metrics.clone());
            debug!(session = %session_id, "activity check: cache hit");
            return Ok(ActivityCheckResult {
                verdict,
                cost: metrics,
                cache_hit: true,
                tally,
            });
        }

        // Cache miss — call the LLM. Mark the session as actively summarizing for
        // the duration of the call so the coordinator-context poll can blink the
        // bullet (D1); the guard clears the flag on every exit path (success,
        // error, or `?` early-return), so a crashed call never leaves a session
        // stuck "summarizing".
        drop(caches);
        let _summarizing = SummarizingGuard::new(self, session_id);
        let classify_result = self.llm.classify(&pane_tail).await;

        let (verdict, input_tokens, output_tokens) = match classify_result {
            Ok(r) => r,
            Err(ActivityError::MissingApiKey) => {
                warn!("activity monitor: OPENROUTER_API_KEY not configured; returning Unknown");
                (
                    ActivityVerdict {
                        state: ActivityState::Unknown,
                        summary: "OPENROUTER_API_KEY not configured".into(),
                        confidence: 0.0,
                    },
                    0,
                    0,
                )
            }
            Err(e) => return Err(e),
        };

        // Persist the summary in the sidecar ONLY when a genuine verdict came
        // back — an `Unknown` verdict (missing key / unconfigured inference) is a
        // degraded placeholder, not a real summary, so it must not leak into
        // `last_summary` (D1: unconfigured inference ⇒ `last_summary` stays None).
        if verdict.state != ActivityState::Unknown {
            self.store_summary(session_id, verdict.summary.clone());
        }

        let latency_ms = start.elapsed().as_millis() as u64;
        let metrics = CheckMetrics {
            session_id: session_id.to_owned(),
            at: Utc::now(),
            model: self.model.clone(),
            input_tokens,
            output_tokens,
            latency_ms,
            cache_hit: false,
            verdict_state: verdict.state.clone(),
        };

        let mut caches = self.cache.lock().await;
        let entry = caches
            .entry(session_id.to_owned())
            .or_insert_with(|| ActivityCache::new(&self.model));
        entry.update_llm_hit(&hash, verdict.clone(), metrics.clone());
        let tally = entry.tally().clone();
        debug!(session = %session_id, state = ?verdict.state, "activity check: LLM verdict");
        Ok(ActivityCheckResult {
            verdict,
            cost: metrics,
            cache_hit: false,
            tally,
        })
    }
}

/// RAII guard that holds a session's `summarizing` flag true for its lifetime.
///
/// Why: the in-flight signal must be honest — it has to clear the moment the
/// inference call completes, *including* when the call returns an error or the
/// surrounding future is dropped. An RAII guard ties the flag's lifetime to the
/// call's scope so no early-return (`?`) or panic can leave a session wedged in
/// the "summarizing" state.
/// What: sets `summarizing = true` on construction and `false` on `Drop`. It
/// borrows the monitor (not generic over `C` beyond the bound it needs) and the
/// session id.
/// Test: `summarizing_true_during_inference`, `summarizing_clears_after_check`.
struct SummarizingGuard<'a, C: LlmClassifier> {
    monitor: &'a ActivityMonitor<C>,
    session_id: &'a str,
}

impl<'a, C: LlmClassifier> SummarizingGuard<'a, C> {
    /// Mark the session as actively summarizing and return the guard.
    fn new(monitor: &'a ActivityMonitor<C>, session_id: &'a str) -> Self {
        monitor.set_summarizing(session_id, true);
        Self {
            monitor,
            session_id,
        }
    }
}

impl<C: LlmClassifier> Drop for SummarizingGuard<'_, C> {
    /// Clear the in-flight flag when the inference call's scope ends.
    fn drop(&mut self) {
        self.monitor.set_summarizing(self.session_id, false);
    }
}

/// Hash `bytes` with SHA-256 and return the lowercase hex string.
///
/// Why: pane-content hashing must be deterministic and collision-resistant;
/// SHA-256 provides both properties.
/// What: runs `sha2::Sha256` on the bytes and hex-encodes the digest.
/// Test: `sha256_hex_is_deterministic`.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Return the last `n` lines of `text` as a single string.
///
/// Why: hashing the full pane buffer is wasteful; the last 60 lines capture
/// the recent terminal activity that the LLM should classify.
/// What: splits on newlines, takes the last `n`, and re-joins.
/// Test: `last_n_lines_short`, `last_n_lines_long`.
fn last_n_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

// Required for the async fn in trait on stable Rust with edition 2024.
use std::future::Future;

#[cfg(test)]
mod tests {
    use super::*;

    struct MockClassifier {
        verdict: ActivityVerdict,
        call_count: Mutex<u32>,
    }

    impl MockClassifier {
        fn new(state: ActivityState) -> Self {
            Self {
                verdict: ActivityVerdict {
                    state,
                    summary: "mock".into(),
                    confidence: 1.0,
                },
                call_count: Mutex::new(0),
            }
        }

        async fn calls(&self) -> u32 {
            *self.call_count.lock().await
        }
    }

    impl LlmClassifier for MockClassifier {
        async fn classify(
            &self,
            _pane_text: &str,
        ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
            *self.call_count.lock().await += 1;
            Ok((self.verdict.clone(), 50, 10))
        }
    }

    #[tokio::test]
    async fn monitor_cache_miss_calls_llm() {
        let classifier = MockClassifier::new(ActivityState::Working);
        let monitor = ActivityMonitor::new(classifier, "test-model");
        let result = monitor.check("s1", "some pane content").await.unwrap();
        assert_eq!(result.verdict.state, ActivityState::Working);
        assert!(!result.cache_hit);
        assert_eq!(monitor.llm.calls().await, 1);
    }

    #[tokio::test]
    async fn monitor_cache_hit_skips_llm() {
        let classifier = MockClassifier::new(ActivityState::Idle);
        let monitor = ActivityMonitor::new(classifier, "test-model");
        let pane = "unchanged content";
        let r1 = monitor.check("s1", pane).await.unwrap();
        assert!(!r1.cache_hit);
        let r2 = monitor.check("s1", pane).await.unwrap();
        assert!(r2.cache_hit);
        assert_eq!(monitor.llm.calls().await, 1);
    }

    #[tokio::test]
    async fn cached_summary_none_without_check() {
        // No check has run yet → the sidecar has no summary for the session.
        let classifier = MockClassifier::new(ActivityState::Working);
        let monitor = ActivityMonitor::new(classifier, "test-model");
        assert_eq!(monitor.cached_summary("s1"), None);
        assert!(!monitor.is_summarizing("s1"));
    }

    #[tokio::test]
    async fn cached_summary_reflects_llm_verdict() {
        // A genuine (non-Unknown) verdict populates the sidecar with the same
        // summary text the `/activity` endpoint would return.
        let classifier = MockClassifier::new(ActivityState::Working);
        let monitor = ActivityMonitor::new(classifier, "test-model");
        monitor.check("s1", "pane").await.unwrap();
        assert_eq!(monitor.cached_summary("s1").as_deref(), Some("mock"));
    }

    #[tokio::test]
    async fn cached_summary_skips_unknown_verdict() {
        // An Unknown verdict (e.g. unconfigured inference) is a degraded
        // placeholder — it must NOT be stored as a real summary (D1).
        let classifier = MockClassifier::new(ActivityState::Unknown);
        let monitor = ActivityMonitor::new(classifier, "test-model");
        monitor.check("s1", "pane").await.unwrap();
        assert_eq!(monitor.cached_summary("s1"), None);
    }

    #[tokio::test]
    async fn summarizing_flag_is_false_when_idle() {
        // Outside an in-flight call the flag is false, even after a completed check.
        let classifier = MockClassifier::new(ActivityState::Working);
        let monitor = ActivityMonitor::new(classifier, "test-model");
        assert!(!monitor.is_summarizing("s1"));
        monitor.check("s1", "pane").await.unwrap();
        // The guard dropped when `check` returned, so the flag is clear again.
        assert!(!monitor.is_summarizing("s1"));
    }

    #[tokio::test]
    async fn summarizing_true_during_inference() {
        // While the classifier is mid-call, `is_summarizing` reports true; once
        // the call resolves it flips back to false. A blocking classifier lets us
        // observe the flag at the in-flight instant deterministically (offline).
        use std::sync::Arc;
        use tokio::sync::Notify;

        struct BlockingClassifier {
            entered: Arc<Notify>,
            release: Arc<Notify>,
        }
        impl LlmClassifier for BlockingClassifier {
            async fn classify(
                &self,
                _pane_text: &str,
            ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
                self.entered.notify_one();
                self.release.notified().await;
                Ok((
                    ActivityVerdict {
                        state: ActivityState::Working,
                        summary: "blocked".into(),
                        confidence: 1.0,
                    },
                    1,
                    1,
                ))
            }
        }

        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let monitor = Arc::new(ActivityMonitor::new(
            BlockingClassifier {
                entered: entered.clone(),
                release: release.clone(),
            },
            "test-model",
        ));

        // Subscribe to the "entered" notification BEFORE spawning the task that
        // fires it. `Notify::notified()` only registers the waiter once the
        // future is polled; `enable()` registers it eagerly here so the
        // classifier's `notify_one()` (which races with the spawn) cannot be
        // lost. Without this, a `notify_one()` that fires before we start
        // awaiting could be dropped, hanging the test forever.
        let entered_sub = entered.notified();
        tokio::pin!(entered_sub);
        entered_sub.as_mut().enable();

        let m = monitor.clone();
        let handle = tokio::spawn(async move { m.check("s1", "pane").await });

        // Wait until the classifier is inside the call, then observe the flag.
        entered_sub.await;
        assert!(monitor.is_summarizing("s1"));

        // Release the call and let it finish; the flag must clear.
        release.notify_one();
        handle.await.unwrap().unwrap();
        assert!(!monitor.is_summarizing("s1"));
        assert_eq!(monitor.cached_summary("s1").as_deref(), Some("blocked"));
    }

    #[tokio::test]
    async fn monitor_different_sessions_independent_caches() {
        let classifier = MockClassifier::new(ActivityState::Working);
        let monitor = ActivityMonitor::new(classifier, "test-model");
        monitor.check("s1", "content A").await.unwrap();
        monitor.check("s2", "content B").await.unwrap();
        // Both are misses; LLM called twice.
        assert_eq!(monitor.llm.calls().await, 2);
    }

    #[test]
    fn sha256_hex_is_deterministic() {
        let h1 = sha256_hex(b"hello");
        let h2 = sha256_hex(b"hello");
        assert_eq!(h1, h2);
        assert_ne!(h1, sha256_hex(b"world"));
    }

    #[test]
    fn last_n_lines_short() {
        let text = "a\nb\nc";
        assert_eq!(last_n_lines(text, 10), "a\nb\nc");
    }

    #[test]
    fn last_n_lines_long() {
        let text = (0..100)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = last_n_lines(&text, 60);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines.len(), 60);
        assert_eq!(lines[0], "40");
    }

    /// A classifier that always fails with a caller-chosen error.
    ///
    /// Why: the monitor treats `MissingApiKey` and every other `ActivityError`
    /// differently — one degrades, one propagates. Proving that split needs a
    /// classifier that fails on demand.
    /// What: returns a clone of the configured error from every `classify`.
    /// Test: `missing_api_key_degrades_to_unknown_verdict`,
    /// `classifier_error_propagates`.
    struct FailingClassifier {
        /// Rendered once per call into the configured variant.
        missing_key: bool,
    }

    impl LlmClassifier for FailingClassifier {
        async fn classify(
            &self,
            _pane_text: &str,
        ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
            if self.missing_key {
                Err(ActivityError::MissingApiKey)
            } else {
                Err(ActivityError::Llm("provider exploded".into()))
            }
        }
    }

    /// Why (#4427): an unconfigured daemon must keep serving `/activity` with a
    /// degraded `Unknown` verdict rather than failing the check — and the
    /// placeholder must NOT be stored as a real summary (D1). Nothing covered
    /// this arm before the inference-adapter migration.
    /// Test: itself.
    #[tokio::test]
    async fn missing_api_key_degrades_to_unknown_verdict() {
        let monitor = ActivityMonitor::new(FailingClassifier { missing_key: true }, "test-model");
        let result = monitor
            .check("s1", "pane")
            .await
            .expect("degrades, not errors");
        assert_eq!(result.verdict.state, ActivityState::Unknown);
        assert_eq!(result.verdict.summary, "OPENROUTER_API_KEY not configured");
        assert_eq!(
            (result.cost.input_tokens, result.cost.output_tokens),
            (0, 0)
        );
        assert_eq!(monitor.cached_summary("s1"), None);
        assert!(!monitor.is_summarizing("s1"));
    }

    /// Why: every non-credential failure must propagate so the caller sees a
    /// real error instead of a fabricated verdict — the counterpart to the
    /// degrade arm above.
    /// Test: itself.
    #[tokio::test]
    async fn classifier_error_propagates() {
        let monitor = ActivityMonitor::new(FailingClassifier { missing_key: false }, "test-model");
        let err = monitor.check("s1", "pane").await.expect_err("propagates");
        assert!(matches!(err, ActivityError::Llm(_)), "got {err:?}");
        // The in-flight guard still cleared on the error path.
        assert!(!monitor.is_summarizing("s1"));
    }
}
