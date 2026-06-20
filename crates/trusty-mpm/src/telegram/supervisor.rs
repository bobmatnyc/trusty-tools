//! Restart-on-exit supervision for the Telegram bot task (#1499).
//!
//! Why: the daemon used to launch the bot with a bare `tokio::spawn` whose body
//! logged once and ENDED on any error. A long-poll `getUpdates` 409 Conflict (a
//! duplicate consumer), a transient network blip, or any other error therefore
//! killed the bot for the rest of the daemon's life with no recovery — the
//! Telegram surface went dark while the daemon looked healthy, which is exactly
//! the silent-death symptom in #1499. This module wraps the bot's run loop in a
//! supervisor that restarts it with bounded exponential backoff, logs every
//! restart and every give-up, refuses to tight-loop on a permanent
//! misconfiguration (e.g. a missing/invalid token), and stops cleanly the moment
//! the daemon signals graceful shutdown.
//! What: [`supervise`] drives a caller-supplied async factory: it runs one bot
//! attempt, classifies the outcome ([`BotExit`]), and either restarts after a
//! backoff or gives up. [`BackoffPolicy`] computes the delay schedule; both are
//! unit-tested with an injected factory so no real Telegram network is involved.
//! Test: `supervisor::tests` — restart-after-transient, exponential growth +
//! cap, give-up after the permanent-failure budget, and cancellation-safety.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// How a single bot attempt ended, telling the supervisor how to react.
///
/// Why: not every exit deserves the same response. A transient error (conflict,
/// network) should restart quickly; a permanent misconfiguration (no token)
/// should back off hard and eventually give up rather than hammer the same
/// doomed startup every few hundred milliseconds.
/// What: the three outcomes the supervisor distinguishes.
/// Test: `classify_*` and the restart/give-up tests assert per-variant behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotExit {
    /// The bot's dispatcher returned normally (graceful stop). Do not restart.
    Graceful,
    /// A transient failure (e.g. a 409 long-poll conflict, a network error).
    /// Restart after a short, growing backoff — this is expected to self-heal.
    Transient,
    /// A permanent misconfiguration (e.g. missing/invalid token). Restarting at
    /// the same cadence would tight-loop forever, so the supervisor backs off to
    /// the cap and gives up after a small budget of attempts.
    Permanent,
}

/// Classify an `anyhow::Error` from the bot's run loop into a [`BotExit`].
///
/// Why: the supervisor's reaction hinges on whether a failure is worth retrying.
/// A missing token (the bot's `run` returns a "TELEGRAM_BOT_TOKEN is required"
/// error) is permanent — retrying cannot help until the operator fixes config —
/// whereas a `getUpdates` conflict or a socket error is transient and expected
/// to clear on its own.
/// What: returns [`BotExit::Permanent`] when the error message names a
/// missing/invalid token (the only configuration error `run` raises before it
/// starts polling); every other error is [`BotExit::Transient`].
/// Test: `classify_missing_token_is_permanent`, `classify_other_is_transient`.
pub fn classify_error(err: &anyhow::Error) -> BotExit {
    let msg = err.to_string().to_ascii_lowercase();
    let permanent = msg.contains("token is required")
        || msg.contains("invalid token")
        || msg.contains("not a valid bot token")
        || msg.contains("unauthorized");
    if permanent {
        BotExit::Permanent
    } else {
        BotExit::Transient
    }
}

/// Bounded exponential-backoff schedule for bot restarts.
///
/// Why: restart-on-exit must not become a busy loop. A geometric schedule
/// (base, base·factor, base·factor², … capped) self-heals a transient conflict
/// in well under a second while a persistent fault settles to one attempt per
/// `max` interval instead of thousands per second.
/// What: holds the first delay, the per-step multiplier, and the ceiling, and
/// computes the delay for a given consecutive-failure count via [`delay_for`].
/// Test: `backoff_grows_and_caps`, `backoff_resets_to_base`.
#[derive(Debug, Clone, Copy)]
pub struct BackoffPolicy {
    /// Delay before the first restart.
    pub base: Duration,
    /// Multiplier applied per consecutive failure.
    pub factor: u32,
    /// Upper bound on any single delay.
    pub max: Duration,
}

impl Default for BackoffPolicy {
    /// The production schedule: 500ms → 1s → 2s → … capped at 30s.
    ///
    /// Why: 500ms recovers a momentary `getUpdates` conflict almost immediately
    /// (the typical case when a second consumer is shutting down) while the 30s
    /// cap keeps a stubborn fault to two attempts per minute.
    /// What: `base = 500ms`, `factor = 2`, `max = 30s`.
    /// Test: `default_policy_is_bounded`.
    fn default() -> Self {
        Self {
            base: Duration::from_millis(500),
            factor: 2,
            max: Duration::from_secs(30),
        }
    }
}

impl BackoffPolicy {
    /// Delay before the restart that follows `failures` consecutive failures.
    ///
    /// Why: the supervisor needs a pure, testable mapping from "how many times
    /// has it failed in a row" to "how long to wait" so the growth and cap are
    /// verifiable without sleeping.
    /// What: returns `base · factor^(failures-1)` saturated at `max`; a
    /// `failures` of 0 or 1 yields `base`. Overflow saturates to `max`.
    /// Test: `backoff_grows_and_caps`.
    pub fn delay_for(&self, failures: u32) -> Duration {
        let exp = failures.saturating_sub(1);
        let mut delay = self.base;
        for _ in 0..exp {
            match delay.checked_mul(self.factor) {
                Some(d) if d <= self.max => delay = d,
                _ => return self.max,
            }
        }
        delay.min(self.max)
    }
}

/// Maximum consecutive *permanent* failures before the supervisor gives up.
///
/// Why: a permanent misconfiguration cannot be retried into success, but a
/// couple of attempts cover the case where the operator is mid-edit on
/// `.env.local`; beyond that, continuing to spawn a doomed task only spams logs.
/// What: after this many back-to-back permanent exits the supervisor logs a
/// clear give-up message and returns.
/// Test: `gives_up_after_permanent_budget`.
const MAX_PERMANENT_FAILURES: u32 = 3;

/// Supervise a Telegram bot run loop, restarting it on exit with backoff.
///
/// Why: see the module docs — a single transient error must not permanently kill
/// the bot, and the supervisor must remain shutdown-safe (it must stop promptly
/// when the daemon is shutting down and never block graceful shutdown).
/// What: repeatedly calls `factory` to produce one bot attempt, races it against
/// `shutdown`, and reacts to its [`BotExit`]:
/// - [`BotExit::Graceful`] → return (the bot stopped cleanly).
/// - [`BotExit::Transient`] → log and restart after [`BackoffPolicy::delay_for`],
///   resetting the permanent-failure counter.
/// - [`BotExit::Permanent`] → log clearly and restart after the *capped* delay;
///   after [`MAX_PERMANENT_FAILURES`] in a row, give up and return.
///
/// The backoff sleep itself is cancellable, so a shutdown during a backoff window
/// returns immediately rather than waiting out the delay.
/// Test: `restarts_after_transient_then_succeeds`, `gives_up_after_permanent_budget`,
/// `stops_on_cancellation` and `backoff_sleep_is_cancellable`.
pub async fn supervise<F, Fut>(factory: F, policy: BackoffPolicy, shutdown: CancellationToken)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = BotExit>,
{
    let mut transient_failures: u32 = 0;
    let mut permanent_failures: u32 = 0;

    loop {
        if shutdown.is_cancelled() {
            tracing::info!("telegram supervisor: shutdown requested — not restarting bot");
            return;
        }

        // Run one attempt, but abandon it the instant shutdown is signalled so a
        // graceful stop is never blocked by an in-flight bot dispatcher.
        let exit = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                tracing::info!("telegram supervisor: shutdown during bot run — exiting");
                return;
            }
            exit = factory() => exit,
        };

        match exit {
            BotExit::Graceful => {
                tracing::info!("telegram bot stopped gracefully — supervisor exiting");
                return;
            }
            BotExit::Transient => {
                permanent_failures = 0;
                transient_failures = transient_failures.saturating_add(1);
                let delay = policy.delay_for(transient_failures);
                tracing::warn!(
                    "telegram bot exited (transient, attempt {transient_failures}); \
                     restarting in {delay:?}"
                );
                if !sleep_or_cancel(delay, &shutdown).await {
                    tracing::info!("telegram supervisor: shutdown during backoff — exiting");
                    return;
                }
            }
            BotExit::Permanent => {
                transient_failures = 0;
                permanent_failures = permanent_failures.saturating_add(1);
                if permanent_failures >= MAX_PERMANENT_FAILURES {
                    tracing::error!(
                        "telegram bot exited with a permanent error {permanent_failures} \
                         times in a row — giving up. Fix TELEGRAM_BOT_TOKEN (and bot config) \
                         then restart the daemon to re-enable the Telegram surface."
                    );
                    return;
                }
                // Back off to the cap on a permanent fault so we never tight-loop.
                let delay = policy.max;
                tracing::error!(
                    "telegram bot exited with a permanent error (attempt {permanent_failures}/\
                     {MAX_PERMANENT_FAILURES}); retrying in {delay:?} in case config was just \
                     updated"
                );
                if !sleep_or_cancel(delay, &shutdown).await {
                    tracing::info!("telegram supervisor: shutdown during backoff — exiting");
                    return;
                }
            }
        }
    }
}

/// Sleep for `delay`, returning early if `shutdown` fires.
///
/// Why: a backoff window must not delay graceful shutdown — a 30s cap would
/// otherwise hold the daemon open for up to 30s after SIGTERM.
/// What: races a timer against the cancellation token; returns `true` when the
/// full delay elapsed (caller should proceed to restart) and `false` when
/// shutdown interrupted it (caller should exit).
/// Test: `backoff_sleep_is_cancellable`.
async fn sleep_or_cancel(delay: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
