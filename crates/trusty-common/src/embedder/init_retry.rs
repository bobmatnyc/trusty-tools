//! Bounded retry for a transient embedding-model init failure.
//!
//! Why: `TextEmbedding::try_new` reaches HuggingFace when the model is not on
//! disk, and some of its failures recover on their own. Two embedders share
//! this recovery: trusty-agents' (#812) and [`FastEmbedder`](super::FastEmbedder),
//! which otherwise answered a sibling's held blob lock by loading a different
//! model (#10662).
//! What: [`retry_transient_init`] is the loop; `init_or_fallback` adds the rule
//! that a transient failure never selects a different model.
//! Test: the `tests` module below; none of it needs a model or the network.

use anyhow::Result;
use std::time::Duration;

/// Bounded retry schedule for a transient model-fetch failure during init.
///
/// Why: some model-fetch failures are transient rather than terminal, such as
/// HTTP 429 from the hub's rate limiter, or hf-hub's per-blob `Lock acquisition
/// failed` when another process is mid-download of the same file. See #812.
/// What: attempt count plus an exponential backoff, capped. A zero
/// `base_backoff` disables sleeping entirely, which is what the unit tests use.
/// The default is 5 attempts with at most 13.25 s of sleep between them.
/// Test: `retry_recovers_after_transient_failures`,
/// `retry_stops_at_max_attempts`, `backoff_grows_and_is_capped`,
/// `default_policy_sleeps_a_bounded_total`.
#[derive(Debug, Clone, Copy)]
pub struct InitRetryPolicy {
    max_attempts: u32,
    base_backoff: Duration,
    max_backoff: Duration,
}

impl Default for InitRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_backoff: Duration::from_millis(750),
            max_backoff: Duration::from_secs(15),
        }
    }
}

/// Classify an init error as transient (worth retrying) or terminal.
///
/// Why: retrying a missing-file or corrupt-model error just multiplies the
/// wall-clock cost of a failure that will never succeed. The hub's rate limit,
/// a sibling's blob lock, a server 5xx and a network failure can each recover
/// on their own, and #10662 needs none of them to select a different model.
/// What: lowercases the full `anyhow` chain (`{err:#}`) and looks for the
/// signatures both #812 recurrences produced, the neighbouring 5xx and timeout
/// responses from the same endpoint, and the network failures `ureq` 3.3.0
/// reports through `hf-hub`. The status-code needles are anchored on their
/// surrounding words so a digit sequence inside a SHA or a byte count cannot
/// match. A 401, 403 or 404 matches no needle, so it stays terminal.
/// Test: `transient_classifier_matches_observed_signatures`,
/// `transient_classifier_rejects_terminal_errors`, the `ureq_*_is_transient`
/// and `ureq_http_*_stays_terminal` tests, `a_coreml_hang_stays_terminal`.
fn is_transient_init_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}").to_ascii_lowercase();
    // #812: the first two needles are the exact strings the two 2026-09-01
    // pre-publish gate runs failed on.
    const TRANSIENT: &[&str] = &[
        "status: 429",
        "lock acquisition failed",
        "too many requests",
        "rate limit",
        "status: 500",
        "status: 502",
        "status: 503",
        "status: 504",
        "timed out",
        // #10662: `ureq`'s `Timeout`. The colon keeps the CoreML hang text's
        // `TRUSTY_COREML_INIT_TIMEOUT_SECS` hint terminal.
        "timeout: ",
        "connection reset",
        "too many retries",
        // #10662: `ureq` network failures, so an offline host does not switch
        // models after one attempt.
        "host not found",
        "connection failed",
        "connection refused",
        "body data reading stalled",
    ];
    TRANSIENT.iter().any(|needle| msg.contains(needle))
}

/// Backoff before retry `attempt` (1-based), including per-process jitter.
///
/// Why: Gate 4 runs under `cargo nextest`, which gives every test its own
/// PROCESS. A backoff identical across processes re-synchronises them onto the
/// same blob lock on the next attempt, so the jitter is what actually breaks
/// the contention up. The pid supplies it without a `rand` dependency.
/// What: `base * 2^(attempt-1)`, clamped to `max_backoff`, plus 0-499ms keyed
/// on the pid. A zero `base_backoff` short-circuits to zero, jitter included.
/// Test: `backoff_grows_and_is_capped`, `zero_base_backoff_never_sleeps`.
fn backoff_for(policy: InitRetryPolicy, attempt: u32) -> Duration {
    if policy.base_backoff.is_zero() {
        return Duration::ZERO;
    }
    let factor = 1u32 << (attempt.saturating_sub(1)).min(16);
    let raw = policy
        .base_backoff
        .saturating_mul(factor)
        .min(policy.max_backoff);
    raw + Duration::from_millis(u64::from(std::process::id() % 500))
}

/// Run `attempt` until it succeeds, exhausts `policy.max_attempts`, or fails
/// terminally.
///
/// Why: #812 — a contended or rate-limited model fetch recovers on its own, so
/// every embedder init should wait for it rather than fail its caller.
/// What: a bounded loop that blocks the calling thread while it sleeps. A
/// terminal error returns immediately with its context intact; a transient one
/// sleeps `backoff_for` and retries; the final attempt's error is returned as-is
/// so the caller sees the real failure rather than a wrapper.
/// Test: `retry_recovers_after_transient_failures`, `retry_stops_at_max_attempts`,
/// `retry_does_not_retry_a_terminal_error`.
pub fn retry_transient_init<T>(
    policy: InitRetryPolicy,
    mut attempt: impl FnMut(u32) -> Result<T>,
) -> Result<T> {
    let max = policy.max_attempts.max(1);
    let mut n = 1u32;
    loop {
        match attempt(n) {
            Ok(value) => return Ok(value),
            Err(err) => {
                if n >= max || !is_transient_init_error(&err) {
                    return Err(err);
                }
                let wait = backoff_for(policy, n);
                tracing::warn!(
                    attempt = n,
                    max_attempts = max,
                    backoff_ms = wait.as_millis() as u64,
                    error = %format!("{err:#}"),
                    "transient embedder init failure; retrying"
                );
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                n += 1;
            }
        }
    }
}

/// Init `model` under [`retry_transient_init`]; only a terminal failure may
/// take the caller's `on_terminal` path to another model or provider.
///
/// Why: #10662 — two daemons loading different models produce vectors that
/// cannot be compared, so a lock held by a sibling must not choose the model.
/// What: a success returns as-is. An error still transient after the retries
/// returns a named error and never runs `on_terminal`. A terminal error goes to
/// `on_terminal` unretried, which is the fallback behaviour from before #10662.
/// Test: `a_transient_lock_error_never_reaches_the_fallback_model`,
/// `exhausted_transient_retries_fail_by_name_without_advancing`,
/// `a_terminal_error_still_reaches_the_fallback`.
pub(super) fn init_or_fallback<T>(
    policy: InitRetryPolicy,
    model: &str,
    attempt: impl FnMut(u32) -> Result<T>,
    on_terminal: impl FnOnce(anyhow::Error) -> Result<T>,
) -> Result<T> {
    match retry_transient_init(policy, attempt) {
        Err(err) if is_transient_init_error(&err) => Err(anyhow::anyhow!(
            "embedder init failed: {model} still hit a transient error after {} attempts; \
             refusing to load a different model, whose vectors would not compare (#10662): {err:#}",
            policy.max_attempts.max(1)
        )),
        Err(err) => on_terminal(err),
        ok => ok,
    }
}

/// Outcome of a single execution-provider init attempt, used to decide
/// whether to fall back to CPU and whether to poison `COREML_KNOWN_BAD`.
///
/// Why: separates the *decision* (fall back? poison the cache?) from the
/// mechanics of spawning a guard thread and racing it against a timeout, so
/// the decision is a pure function testable without any real ORT/CoreML
/// runtime (issue #2111).
/// What: the three ways a bounded init attempt can resolve.
/// Test: `decide_fallback` tests in `mod.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitOutcome {
    /// The provider initialised successfully within the bound.
    Success,
    /// The provider returned an `Err` quickly (pre-existing #763 behaviour).
    FastError,
    /// The provider did not return within `coreml_init_timeout` — the
    /// underlying OS thread is presumed permanently blocked.
    Hung,
}

/// Decide whether an [`InitOutcome`] should fall back to CPU, and whether
/// `COREML_KNOWN_BAD` should be set as a result.
///
/// Why: a fast `Err` (e.g. a transient CoreML registration failure) does not
/// prove the provider will misbehave on the next attempt, so the pre-existing
/// #763 behaviour retries CoreML on the next full `FastEmbedder::new()`. A
/// `Hung` outcome, by contrast, means an OS thread is permanently stuck
/// inside ORT/CoreML with no cancellation point — retrying would leak one
/// more blocked thread per attempt, so it must be remembered for the rest of
/// the process (issue #2111).
/// What: returns `(fall_back_to_cpu, mark_known_bad)`.
/// Test: `fallback_decision_success_keeps_provider`,
/// `fallback_decision_fast_error_falls_back_without_poisoning`,
/// `fallback_decision_hang_falls_back_and_poisons` in `mod.rs`.
pub(super) fn decide_fallback(outcome: InitOutcome) -> (bool, bool) {
    match outcome {
        InitOutcome::Success => (false, false),
        InitOutcome::FastError => (true, false),
        InitOutcome::Hung => (true, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A policy that retries the same number of times but never sleeps.
    fn instant_policy(max_attempts: u32) -> InitRetryPolicy {
        InitRetryPolicy {
            max_attempts,
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        }
    }

    /// The two error strings the 2026-09-01 pre-publish gate runs actually
    /// failed on, verbatim from the job logs (#812).
    const GATE_429: &str = "Failed to retrieve model.onnx: request error: http status: 429";
    const GATE_LOCK: &str = "Failed to retrieve model.onnx: Lock acquisition failed: \
         /home/runner/.cache/trusty-agents/models/models--Qdrant--all-MiniLM-L6-v2-onnx/blobs/9bbecc17.lock";

    /// #10662: the fp32 attempt fails on a held lock, then the lock frees.
    /// Pre-fix, `FastEmbedder` loaded INT8 on the first failure.
    #[test]
    fn a_transient_lock_error_never_reaches_the_fallback_model() {
        let attempts = Cell::new(0u32);
        let got = init_or_fallback(
            instant_policy(5),
            "fp32",
            |n| {
                attempts.set(n);
                if n < 3 {
                    Err(anyhow::anyhow!(GATE_LOCK))
                } else {
                    Ok("fp32")
                }
            },
            |_| Ok("int8"),
        )
        .expect("a lock that frees must yield the primary model");
        assert_eq!(got, "fp32", "a transient lock error changed the model");
        assert_eq!(attempts.get(), 3);
    }

    /// #10662 Fail-Open Check: the lock outlasts every retry. The result must
    /// be a named error, never the fallback model.
    #[test]
    fn exhausted_transient_retries_fail_by_name_without_advancing() {
        let attempts = Cell::new(0u32);
        let advanced = Cell::new(false);
        let err = init_or_fallback(
            instant_policy(4),
            "fp32",
            |n| {
                attempts.set(n);
                Err::<&str, _>(anyhow::anyhow!(GATE_LOCK))
            },
            |_| {
                advanced.set(true);
                Ok("int8")
            },
        )
        .expect_err("exhausted retries must not produce a model");
        assert!(
            !advanced.get(),
            "exhausted retries advanced to the fallback"
        );
        assert_eq!(attempts.get(), 4, "expected the attempt budget to be spent");
        let msg = format!("{err:#}");
        for needle in ["embedder init failed", "fp32", "4 attempts", "refusing"] {
            assert!(msg.contains(needle), "missing {needle:?} in: {msg}");
        }
        assert!(msg.contains("Lock acquisition failed"), "cause lost: {msg}");
    }

    /// #10662 preserved behaviour: a terminal error falls back at once, and the
    /// fallback sees that error.
    #[test]
    fn a_terminal_error_still_reaches_the_fallback() {
        let attempts = Cell::new(0u32);
        let seen = Cell::new(false);
        let got = init_or_fallback(
            instant_policy(5),
            "fp32",
            |n| {
                attempts.set(n);
                Err::<&str, _>(anyhow::anyhow!("invalid ONNX protobuf"))
            },
            |err| {
                seen.set(format!("{err:#}").contains("invalid ONNX protobuf"));
                Ok("int8")
            },
        )
        .expect("a terminal error must reach the fallback");
        assert_eq!(got, "int8");
        assert_eq!(attempts.get(), 1, "a terminal error must not be retried");
        assert!(seen.get(), "the fallback must receive the terminal error");
    }

    /// #10662: a presumed-hung CoreML init must stay terminal, or the #2111
    /// CPU fallback would stop firing. Text from `try_new_bounded`.
    #[test]
    fn a_coreml_hang_stays_terminal() {
        assert_terminal_reaches_fallback(
            "CoreML EP init did not complete within 60s (issue #2111) — presumed hung; \
             the stuck OS thread is abandoned and CoreML will be skipped for the rest of \
             this process (falling back to CPU). Set TRUSTY_COREML_INIT_TIMEOUT_SECS \
             to raise the bound if this host's CoreML cold-compile legitimately needs more time.",
        );
    }

    /// `text` must spend the whole attempt budget and end in the named error,
    /// never the fallback model.
    #[track_caller]
    fn assert_transient_never_falls_back(text: &str) {
        let attempts = Cell::new(0u32);
        let got = init_or_fallback(
            instant_policy(3),
            "fp32",
            |n| {
                attempts.set(n);
                Err::<&str, _>(anyhow::anyhow!(text.to_string()))
            },
            |_| Ok("int8"),
        );
        let Err(err) = got else {
            panic!("{text:?} was classified terminal and loaded the other model");
        };
        assert_eq!(attempts.get(), 3, "{text:?} did not spend its retries");
        assert!(
            format!("{err:#}").contains("embedder init failed"),
            "{text:?} did not fail by name: {err:#}"
        );
    }

    /// `text` must take the fallback path on its first attempt.
    #[track_caller]
    fn assert_terminal_reaches_fallback(text: &str) {
        let attempts = Cell::new(0u32);
        let got = init_or_fallback(
            instant_policy(3),
            "fp32",
            |n| {
                attempts.set(n);
                Err::<&str, _>(anyhow::anyhow!(text.to_string()))
            },
            |_| Ok("int8"),
        );
        assert_eq!(attempts.get(), 1, "{text:?} was retried");
        assert_eq!(got.ok(), Some("int8"), "{text:?} did not reach the fallback");
    }

    // #10662: the texts below are what `ureq` 3.3.0 renders (`error.rs:232-241,
    // 276`, `timings.rs:227`, `unversioned/transport/tcp.rs:123`) inside the
    // wrappers `hf-hub` 0.5.0 (`api/sync.rs:180, 188`) and `fastembed` add.
    #[test]
    fn ureq_connect_timeout_is_transient() {
        assert_transient_never_falls_back(
            "Failed to retrieve model.onnx: request error: timeout: connect",
        );
    }

    /// An offline host with only the other model cached lands here.
    #[test]
    fn ureq_host_not_found_is_transient() {
        assert_transient_never_falls_back(
            "Failed to retrieve model.onnx: request error: host not found",
        );
    }

    #[test]
    fn ureq_connection_failed_is_transient() {
        assert_transient_never_falls_back(
            "Failed to retrieve model.onnx: request error: connection failed",
        );
    }

    #[test]
    fn ureq_http_500_is_transient() {
        assert_transient_never_falls_back(
            "Failed to retrieve model.onnx: request error: http status: 500",
        );
    }

    #[test]
    fn ureq_connection_refused_is_transient() {
        assert_transient_never_falls_back(
            "Failed to retrieve model.onnx: request error: io: Connection refused",
        );
    }

    /// A body read fails inside `std::io::copy`, so hf-hub renders it as I/O.
    #[test]
    fn ureq_body_stall_is_transient() {
        assert_transient_never_falls_back(
            "Failed to retrieve model.onnx: I/O error body data reading stalled",
        );
    }

    #[test]
    fn ureq_http_401_stays_terminal() {
        assert_terminal_reaches_fallback(
            "Failed to retrieve model.onnx: request error: http status: 401",
        );
    }

    #[test]
    fn ureq_http_403_stays_terminal() {
        assert_terminal_reaches_fallback(
            "Failed to retrieve model.onnx: request error: http status: 403",
        );
    }

    #[test]
    fn ureq_http_404_stays_terminal() {
        assert_terminal_reaches_fallback(
            "Failed to retrieve model.onnx: request error: http status: 404",
        );
    }

    /// #10662: the bound a daemon start waits on. hf-hub spends about 5 s per
    /// locked attempt, so five attempts plus this sleep stay well inside the
    /// 180 s `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` default.
    #[test]
    fn default_policy_sleeps_a_bounded_total() {
        let policy = InitRetryPolicy::default();
        assert_eq!(policy.max_attempts, 5);
        let slept: Duration = (1..policy.max_attempts)
            .map(|n| backoff_for(policy, n))
            .sum();
        assert!(slept >= Duration::from_millis(11_250), "got {slept:?}");
        assert!(slept < Duration::from_millis(13_250), "got {slept:?}");
    }

    #[test]
    fn transient_classifier_matches_observed_signatures() {
        for msg in [
            GATE_429,
            GATE_LOCK,
            "hub returned 429 Too Many Requests",
            "request error: http status: 503",
            "operation timed out",
            "Too many retries: connection reset by peer",
        ] {
            assert!(
                is_transient_init_error(&anyhow::anyhow!(msg.to_string())),
                "expected transient classification for: {msg}"
            );
        }
    }

    #[test]
    fn transient_classifier_rejects_terminal_errors() {
        for msg in [
            "No such file or directory (os error 2)",
            "request error: http status: 404",
            "invalid ONNX protobuf: unexpected end of stream",
            // A SHA or byte count containing the digits must not match the
            // status-code needles, which is why those are anchored on "status:".
            "corrupt blob 429004299142 (429 bytes)",
        ] {
            assert!(
                !is_transient_init_error(&anyhow::anyhow!(msg.to_string())),
                "expected terminal classification for: {msg}"
            );
        }
    }

    #[test]
    fn transient_classifier_reads_the_whole_anyhow_chain() {
        // The real failure arrives as a `.context()`-wrapped chain, with the
        // 429 buried two levels down — `{err:#}` is what surfaces it.
        let err = anyhow::anyhow!(GATE_429)
            .context("failed to initialize fastembed TextEmbedding (AllMiniLML6V2)")
            .context("init FastEmbedder");
        assert!(is_transient_init_error(&err));
    }

    #[test]
    fn retry_recovers_after_transient_failures() {
        // The lock-contention path: two processes lose the race, the third
        // attempt finds the blob already written and succeeds.
        let attempts = Cell::new(0u32);
        let got = retry_transient_init(instant_policy(5), |n| {
            attempts.set(n);
            if n < 3 {
                Err(anyhow::anyhow!(GATE_LOCK))
            } else {
                Ok("model")
            }
        })
        .expect("a transient failure must not be fatal");
        assert_eq!(got, "model");
        assert_eq!(attempts.get(), 3, "expected exactly three attempts");
    }

    #[test]
    fn retry_stops_at_max_attempts() {
        let attempts = Cell::new(0u32);
        let err = retry_transient_init(instant_policy(4), |n| {
            attempts.set(n);
            Err::<(), _>(anyhow::anyhow!(GATE_429))
        })
        .expect_err("an always-429 init must eventually give up");
        assert_eq!(attempts.get(), 4, "expected the attempt budget to be spent");
        assert!(
            format!("{err:#}").contains("429"),
            "the final attempt's own error must survive, got: {err:#}"
        );
    }

    #[test]
    fn retry_does_not_retry_a_terminal_error() {
        let attempts = Cell::new(0u32);
        let err = retry_transient_init(instant_policy(5), |n| {
            attempts.set(n);
            Err::<(), _>(anyhow::anyhow!("No such file or directory (os error 2)"))
        })
        .expect_err("a terminal error must propagate");
        assert_eq!(attempts.get(), 1, "a terminal error must not be retried");
        assert!(format!("{err:#}").contains("No such file"));
    }

    #[test]
    fn retry_runs_at_least_once_with_a_zero_attempt_budget() {
        let attempts = Cell::new(0u32);
        let policy = InitRetryPolicy {
            max_attempts: 0,
            ..instant_policy(0)
        };
        retry_transient_init(policy, |n| {
            attempts.set(n);
            Ok::<_, anyhow::Error>(())
        })
        .expect("a zero budget must still attempt once");
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let policy = InitRetryPolicy {
            max_attempts: 8,
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(400),
        };
        // Jitter is 0-499ms on top of the exponential term, so assert on the
        // bracket rather than an exact value.
        let a1 = backoff_for(policy, 1);
        let a3 = backoff_for(policy, 3);
        assert!(a1 >= Duration::from_millis(100) && a1 < Duration::from_millis(600));
        assert!(a3 >= a1, "backoff must not shrink as attempts grow");
        for n in 3..=8 {
            assert!(
                backoff_for(policy, n) < Duration::from_millis(900),
                "attempt {n} exceeded max_backoff + max jitter"
            );
        }
    }

    #[test]
    fn zero_base_backoff_never_sleeps() {
        for n in 1..=10 {
            assert_eq!(backoff_for(instant_policy(10), n), Duration::ZERO);
        }
    }
}
