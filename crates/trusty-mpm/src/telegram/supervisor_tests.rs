//! Unit tests for the Telegram bot supervisor (#1499).
//!
//! Why: lives in a recognized `_tests.rs` sibling so `supervisor.rs` stays well
//! under the 500-SLOC production cap while sharing its private items via
//! `use super::*`. The tests inject a counting factory so restart, backoff, and
//! give-up behaviour are verified deterministically with no real Telegram
//! network. Backoff delays use a millisecond-scale `fast_policy` so the real
//! sleeps complete near-instantly (no `tokio` `test-util` paused-time feature
//! is required), and the cancellation test fires shutdown before its long
//! backoff can elapse.
//! Test: this *is* the test module.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn classify_missing_token_is_permanent() {
    let err = anyhow::anyhow!("TELEGRAM_BOT_TOKEN is required (or pass --check)");
    assert_eq!(classify_error(&err), BotExit::Permanent);
    let invalid = anyhow::anyhow!("Not a valid bot token");
    assert_eq!(classify_error(&invalid), BotExit::Permanent);
}

#[test]
fn classify_other_is_transient() {
    let conflict = anyhow::anyhow!("terminated by other getUpdates request (409 Conflict)");
    assert_eq!(classify_error(&conflict), BotExit::Transient);
    let network = anyhow::anyhow!("error sending request: connection reset");
    assert_eq!(classify_error(&network), BotExit::Transient);
}

#[test]
fn backoff_grows_and_caps() {
    let policy = BackoffPolicy {
        base: Duration::from_millis(500),
        factor: 2,
        max: Duration::from_secs(30),
        healthy_run: Duration::from_secs(60),
    };
    // 0 and 1 failures both wait the base delay.
    assert_eq!(policy.delay_for(0), Duration::from_millis(500));
    assert_eq!(policy.delay_for(1), Duration::from_millis(500));
    // Geometric growth: 500ms, 1s, 2s, 4s, 8s, 16s …
    assert_eq!(policy.delay_for(2), Duration::from_secs(1));
    assert_eq!(policy.delay_for(3), Duration::from_secs(2));
    assert_eq!(policy.delay_for(6), Duration::from_secs(16));
    // … then saturates at the 30s cap and never exceeds it.
    assert_eq!(policy.delay_for(7), Duration::from_secs(30));
    assert_eq!(policy.delay_for(100), Duration::from_secs(30));
}

#[test]
fn default_policy_is_bounded() {
    let p = BackoffPolicy::default();
    assert_eq!(p.base, Duration::from_millis(500));
    assert_eq!(p.max, Duration::from_secs(30));
    assert_eq!(p.healthy_run, Duration::from_secs(60));
    // No computed delay ever exceeds the cap, even for an absurd failure count.
    assert!(p.delay_for(u32::MAX) <= p.max);
}

#[test]
fn healthy_run_resets_transient_count() {
    // The pure reset rule (#1499 follow-up): many short transient failures must
    // escalate the consecutive-failure count (driving backoff up the geometric
    // schedule), but a transient exit that follows a healthy-duration run must
    // reset the count to 1 so the very next backoff is back at `base`.
    let policy = BackoffPolicy {
        base: Duration::from_millis(1),
        factor: 2,
        max: Duration::from_millis(8),
        healthy_run: Duration::from_millis(50),
    };
    let short = Duration::from_millis(0);
    let healthy = Duration::from_millis(50);

    // Short runs escalate: 0 → 1 → 2 → … so the backoff exponent climbs.
    assert_eq!(policy.next_transient_count(0, short), 1);
    assert_eq!(policy.next_transient_count(1, short), 2);
    assert_eq!(policy.next_transient_count(7, short), 8);
    // Escalation pins the backoff at the cap once the exponent is high enough.
    assert_eq!(policy.delay_for(8), policy.max);

    // A transient exit after a healthy run resets the count to 1, regardless of
    // how high the prior count had climbed …
    assert_eq!(policy.next_transient_count(8, healthy), 1);
    assert_eq!(policy.next_transient_count(u32::MAX, healthy), 1);
    // … and a count of 1 maps backoff back to the floor (`base`), proving the
    // delay is no longer pinned at the cap.
    assert_eq!(policy.delay_for(1), policy.base);

    // A run exactly at the threshold counts as healthy (>=, not >).
    assert_eq!(
        policy.next_transient_count(5, policy.healthy_run),
        1,
        "a run lasting exactly healthy_run must reset the counter"
    );
    // Just-under-threshold does NOT reset — it keeps escalating.
    assert_eq!(
        policy.next_transient_count(5, policy.healthy_run - Duration::from_millis(1)),
        6,
        "a sub-threshold run must keep the backoff escalating"
    );
}

#[tokio::test]
async fn healthy_run_resets_transient_backoff() {
    // End-to-end proof through `supervise`: the bot flaps transiently several
    // times with near-instant runs (escalating backoff), then runs "healthily"
    // (longer than `healthy_run`) before exiting transiently once more, then
    // stops gracefully. We assert on the OBSERVED gap between the start of the
    // post-healthy restart and its predecessor: after the reset it must be ~base,
    // not the escalated/capped delay the prior flaps had built up to.
    let policy = BackoffPolicy {
        base: Duration::from_millis(2),
        factor: 4,
        // A high cap so escalated delays are clearly distinguishable from `base`.
        max: Duration::from_millis(400),
        // Small enough that a deliberately delayed run trips it without a slow test.
        healthy_run: Duration::from_millis(20),
    };

    // attempt index → exit. Indices 0,1,2 flap fast (escalating: delay_for(1)=2ms,
    // delay_for(2)=8ms, delay_for(3)=32ms). Index 3 runs healthily then exits
    // transiently (must reset → next delay back to base=2ms). Index 4 is graceful.
    let starts: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let s = Arc::clone(&starts);
    let healthy_for = policy.healthy_run;
    let factory = move || {
        let a = Arc::clone(&a);
        let s = Arc::clone(&s);
        async move {
            let n = a.fetch_add(1, Ordering::SeqCst);
            s.lock().unwrap().push(tokio::time::Instant::now());
            if n == 3 {
                // Run long enough to count as healthy, then exit transiently.
                tokio::time::sleep(healthy_for + Duration::from_millis(5)).await;
                BotExit::Transient
            } else if n < 3 {
                BotExit::Transient
            } else {
                BotExit::Graceful
            }
        }
    };

    supervise(factory, policy, CancellationToken::new()).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 5);

    let starts = starts.lock().unwrap();
    assert_eq!(starts.len(), 5);

    // Gap before attempt 3 (the healthy run's restart) reflects the ESCALATED
    // backoff after three flaps: delay_for(3) = base*factor^2 = 2ms*16 = 32ms.
    let escalated_gap = starts[3].duration_since(starts[2]);
    assert!(
        escalated_gap >= Duration::from_millis(25),
        "pre-reset backoff should be escalated (>=~32ms), saw {escalated_gap:?}"
    );

    // Gap before attempt 4 = (healthy run duration ~25ms) + (post-reset backoff).
    // The backoff component must be back at base (~2ms), NOT the prior escalated
    // delay_for(4)=128ms. We subtract the known healthy run to isolate the backoff
    // and assert it is small — proving the counter reset to the floor.
    let post_healthy_total = starts[4].duration_since(starts[3]);
    let backoff_component = post_healthy_total.saturating_sub(healthy_for);
    assert!(
        backoff_component < Duration::from_millis(20),
        "post-healthy backoff must reset to ~base, isolated backoff was {backoff_component:?} \
         (total {post_healthy_total:?})"
    );
}

/// A fast policy with millisecond-scale delays so real backoff sleeps in the
/// restart/give-up tests complete near-instantly. `healthy_run` is set well above
/// the near-instant run durations these tests produce, so the healthy-run reset
/// never fires unless a test deliberately delays its run (see
/// `healthy_run_resets_transient_backoff`).
fn fast_policy() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(1),
        factor: 2,
        max: Duration::from_millis(8),
        healthy_run: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn restarts_after_transient_then_succeeds() {
    // The bot "fails" transiently twice, then exits gracefully. The supervisor
    // must restart it across BOTH transient failures (3 total attempts) and then
    // stop — proving a transient conflict self-heals instead of killing the bot.
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let factory = move || {
        let a = Arc::clone(&a);
        async move {
            let n = a.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                BotExit::Transient
            } else {
                BotExit::Graceful
            }
        }
    };

    supervise(factory, fast_policy(), CancellationToken::new()).await;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "expected 2 restarts after transient failures, then a graceful exit"
    );
}

#[tokio::test]
async fn gives_up_after_permanent_budget() {
    // A permanently-failing bot must be retried only up to the permanent budget
    // and then abandoned — never an infinite tight loop.
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let factory = move || {
        let a = Arc::clone(&a);
        async move {
            a.fetch_add(1, Ordering::SeqCst);
            BotExit::Permanent
        }
    };

    supervise(factory, fast_policy(), CancellationToken::new()).await;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        MAX_PERMANENT_FAILURES as usize,
        "permanent failures must give up at the budget, not loop forever"
    );
}

#[tokio::test]
async fn transient_resets_permanent_counter() {
    // A transient success between permanent failures must reset the permanent
    // counter, so a flapping bot is not prematurely abandoned. Sequence:
    // Permanent, Permanent, Transient, Permanent, Permanent, Graceful.
    // Without the reset the 3rd permanent (index 4) would trip the give-up; with
    // it, all 6 attempts run.
    let script = Arc::new([
        BotExit::Permanent,
        BotExit::Permanent,
        BotExit::Transient,
        BotExit::Permanent,
        BotExit::Permanent,
        BotExit::Graceful,
    ]);
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let s = Arc::clone(&script);
    let factory = move || {
        let a = Arc::clone(&a);
        let s = Arc::clone(&s);
        async move {
            let n = a.fetch_add(1, Ordering::SeqCst);
            s[n]
        }
    };

    supervise(factory, fast_policy(), CancellationToken::new()).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn stops_on_cancellation() {
    // When shutdown is already requested, the supervisor must not run the bot at
    // all — it exits immediately, never blocking graceful shutdown.
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let factory = move || {
        let a = Arc::clone(&a);
        async move {
            a.fetch_add(1, Ordering::SeqCst);
            BotExit::Transient
        }
    };

    let token = CancellationToken::new();
    token.cancel();
    supervise(factory, fast_policy(), token).await;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "a pre-cancelled supervisor must not start the bot"
    );
}

#[tokio::test]
async fn backoff_sleep_is_cancellable() {
    // A shutdown that arrives DURING a backoff window must end the supervisor
    // promptly rather than waiting out the (here, capped) delay. We run one
    // transient failure to enter a backoff, cancel, and assert the supervisor
    // returns having made exactly one attempt.
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let token = CancellationToken::new();
    let cancel_token = token.clone();

    // A policy whose backoff is an hour long: without the cancellation bailout
    // the supervisor would block here for an hour, so a prompt return proves the
    // backoff sleep is interrupted by shutdown.
    let policy = BackoffPolicy {
        base: Duration::from_secs(3600),
        factor: 2,
        max: Duration::from_secs(3600),
        healthy_run: Duration::from_secs(60),
    };

    let factory = move || {
        let a = Arc::clone(&a);
        let cancel_token = cancel_token.clone();
        async move {
            let n = a.fetch_add(1, Ordering::SeqCst);
            // On the first attempt, request shutdown so the supervisor enters its
            // backoff already cancelled and must bail out of the sleep.
            if n == 0 {
                cancel_token.cancel();
            }
            BotExit::Transient
        }
    };

    supervise(factory, policy, token).await;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "supervisor must exit out of the backoff sleep on cancellation"
    );
}
