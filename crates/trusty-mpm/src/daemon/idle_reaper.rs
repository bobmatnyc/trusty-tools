//! Idle auto-stop daemon loop — opt-in background task (#1816).
//!
//! Why: long-lived idle sessions consume Claude Max rate-limit slots without
//! doing useful work. This module provides a background task that periodically
//! classifies `Active` sessions using the existing `ActivityMonitor` hash-cached
//! LLM verdict, maintains per-session consecutive-hit counters, and calls
//! `SessionManager::stop()` / `decommission()` when the thresholds are crossed.
//! The feature is completely OFF by default (`idle_auto_stop.enabled = false`
//! in `~/.trusty-mpm/config.toml`) so existing installations see zero behavior
//! change.
//! What: [`idle_reaper_loop`] is the `tokio::spawn` target; [`IdleReaperState`]
//! holds the in-memory per-session counters; [`IdleVerdictProvider`] is the
//! injectable seam that keeps unit tests free from LLM calls.
//! Test: `counter_increments_on_idle`, `counter_resets_on_non_idle`,
//! `stop_fires_at_threshold`, `decommission_fires_at_done_threshold`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::core::config::IdleAutoStopConfig;
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionManager};

// ─────────────────────────────────────────────────────────────
// Verdict provider trait (injectable for tests)
// ─────────────────────────────────────────────────────────────

/// Injectable seam for obtaining an activity verdict for a single session.
///
/// Why: the production implementation calls `ActivityMonitor` (which requires
/// an OpenRouter API key and captures tmux pane content); tests must inject a
/// stub that returns canned verdicts without network I/O.
/// What: one async method returning the lowercased verdict string (`"idle"`,
/// `"done"`, `"working"`, …) for a named tmux session, or `None` if the
/// verdict is unavailable.
/// Test: `MockVerdictProvider` in the `tests` module at the bottom of this file.
pub trait IdleVerdictProvider: Send + Sync {
    /// Return the current activity verdict for `session_name`, or `None`.
    ///
    /// Why: `None` means the session cannot be classified right now (no API key,
    /// transient failure, etc.) and must be left alone — exactly like the
    /// `unknown` verdict from the classifier.
    /// What: delegates to `ActivityMonitor::check` in production; stubs return
    /// a fixed value in tests.
    /// Test: `MockVerdictProvider`.
    fn verdict(
        &self,
        session_name: &str,
    ) -> impl std::future::Future<Output = Option<String>> + Send;
}

// ─────────────────────────────────────────────────────────────
// Per-session hit counters
// ─────────────────────────────────────────────────────────────

/// Per-session consecutive-hit counters for the idle and done verdicts.
///
/// Why: the reaper must fire only after N consecutive idle/done verdicts to avoid
/// killing a session that is momentarily idle between bursts of activity. A
/// single-verdict trigger would be too aggressive; the counter resets on any
/// non-idle/non-done verdict so short pauses never accumulate.
/// What: two independent counters — `idle_hits` for the stop threshold,
/// `done_hits` for the decommission threshold. Both reset to 0 on the
/// complementary verdict class.
/// Test: `counter_increments_on_idle`, `counter_resets_on_non_idle`.
#[derive(Debug, Default, Clone)]
pub struct SessionHitCounter {
    /// Consecutive `idle` verdicts since the last non-idle verdict.
    pub idle_hits: u32,
    /// Consecutive `done` verdicts since the last non-done verdict.
    pub done_hits: u32,
}

/// Outcome of applying one verdict to a [`SessionHitCounter`].
///
/// Why: the reaper loop and unit tests need to know what action the threshold
/// logic decided without coupling to the async I/O side-effects.
/// What: three cases — no action yet (counters incremented), stop threshold
/// reached, or decommission threshold reached.
/// Test: `stop_fires_at_threshold`, `decommission_fires_at_done_threshold`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaperDecision {
    /// Neither threshold crossed; counters updated, no action taken.
    Continue,
    /// Idle threshold reached: call `SessionManager::stop()`.
    Stop,
    /// Done threshold reached: call `SessionManager::decommission()`.
    Decommission,
}

/// Apply one verdict to a counter and return the resulting decision (pure).
///
/// Why: decoupling the threshold logic from async I/O keeps it unit-testable
/// without any stubs or mocks for the session manager itself.
/// What: normalises the verdict, increments/resets counters, then compares
/// against the thresholds. Returns `Stop` or `Decommission` when a threshold
/// is first crossed; `Continue` otherwise. Verdicts outside `idle`/`done`
/// reset both counters.
/// Test: `counter_increments_on_idle`, `counter_resets_on_non_idle`,
/// `stop_fires_at_threshold`, `decommission_fires_at_done_threshold`.
pub fn apply_verdict(
    counter: &mut SessionHitCounter,
    verdict: Option<&str>,
    cfg: &IdleAutoStopConfig,
) -> ReaperDecision {
    use crate::core::sm::prune::normalize_verdict;
    let normalized = verdict.map(normalize_verdict).unwrap_or_default();
    match normalized.as_str() {
        "idle" => {
            counter.idle_hits = counter.idle_hits.saturating_add(1);
            counter.done_hits = 0;
            if counter.idle_hits >= cfg.idle_consecutive_threshold {
                return ReaperDecision::Stop;
            }
        }
        "done" => {
            counter.done_hits = counter.done_hits.saturating_add(1);
            counter.idle_hits = 0;
            if counter.done_hits >= cfg.done_consecutive_threshold {
                return ReaperDecision::Decommission;
            }
        }
        _ => {
            counter.idle_hits = 0;
            counter.done_hits = 0;
        }
    }
    ReaperDecision::Continue
}

// ─────────────────────────────────────────────────────────────
// In-memory reaper state
// ─────────────────────────────────────────────────────────────

/// In-memory state for the idle-reaper loop.
///
/// Why: the per-session counters must survive across loop ticks but do NOT need
/// persistence — a daemon restart resets the grace window, which is acceptable
/// since it only delays (never prevents) the eventual stop.
/// What: a `HashMap` keyed on [`ManagedSessionId`] holding the consecutive-hit
/// counters for each known Active session. Entries are removed when the session
/// is acted on or transitions out of `Active`.
/// Test: `counter_increments_on_idle`.
#[derive(Debug, Default)]
pub struct IdleReaperState {
    counters: HashMap<ManagedSessionId, SessionHitCounter>,
}

impl IdleReaperState {
    /// Create a new, empty reaper state.
    ///
    /// Why: constructed once at daemon start and passed into the loop.
    /// What: zero-initialised `HashMap`.
    /// Test: `new_state_is_empty`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a mutable reference to the counter for `id`, inserting a default.
    ///
    /// Why: the loop calls this for every active session; a missing entry means
    /// the session is newly observed and starts with zero hits.
    /// What: delegates to `HashMap::entry().or_default()`.
    /// Test: `counter_increments_on_idle`.
    pub fn counter_mut(&mut self, id: &ManagedSessionId) -> &mut SessionHitCounter {
        self.counters.entry(*id).or_default()
    }

    /// Remove the counter for `id` (called after stop/decommission).
    ///
    /// Why: once a session is acted on, its counter entry is stale and would
    /// incorrectly persist idle_hits for a future session reusing the same id
    /// (unlikely but defensive).
    /// What: calls `HashMap::remove`.
    /// Test: `counter_removed_after_action`.
    pub fn remove(&mut self, id: &ManagedSessionId) {
        self.counters.remove(id);
    }

    /// Retain only the counters whose session id is in `active_ids`.
    ///
    /// Why: sessions that transition out of `Active` between ticks must not
    /// accumulate phantom idle counts. Pruning at each tick avoids unbounded
    /// growth for long-lived daemons with many stopped/decommissioned sessions.
    /// What: calls `HashMap::retain`.
    /// Test: `prune_removes_stale_counters`.
    pub fn prune_stale(&mut self, active_ids: &std::collections::HashSet<ManagedSessionId>) {
        self.counters.retain(|id, _| active_ids.contains(id));
    }
}

// ─────────────────────────────────────────────────────────────
// The background loop
// ─────────────────────────────────────────────────────────────

/// Spawn the idle-reaper loop as a background task.
///
/// Why: the daemon needs to start the loop at boot when
/// `idle_auto_stop.enabled = true` and must be able to cancel it cleanly on
/// SIGTERM (same cancellation-token pattern as `reap_loop` and
/// `orphan_gc_loop`).
/// What: takes the session manager, config, a verdict provider (production:
/// wraps `ActivityMonitor`; tests: a stub), and a cancellation token. Returns
/// the `JoinHandle` so the daemon can await it on shutdown if needed. The loop
/// itself is completely self-contained and logs all actions via `tracing`.
/// Test: `cancel_token_stops_idle_reaper_cleanly`.
pub async fn idle_reaper_loop<P>(
    manager: Arc<SessionManager>,
    cfg: IdleAutoStopConfig,
    provider: Arc<P>,
    cancel: CancellationToken,
) where
    P: IdleVerdictProvider + 'static,
{
    let interval = std::time::Duration::from_secs(cfg.poll_interval_secs);
    info!(
        poll_interval_secs = cfg.poll_interval_secs,
        idle_threshold = cfg.idle_consecutive_threshold,
        done_threshold = cfg.done_consecutive_threshold,
        "idle-reaper: started"
    );
    let mut state = IdleReaperState::new();
    let mut tick = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("idle-reaper: cancel signal received; exiting");
                break;
            }
            _ = tick.tick() => {
                run_one_sweep(&manager, &cfg, &*provider, &mut state).await;
            }
        }
    }
}

/// Execute one classification sweep over all Active sessions.
///
/// Why: extracted from the loop body so it is independently testable without
/// real timers. Classifies every Active session, updates counters, and fires
/// stop/decommission actions. All individual errors are logged and swallowed so
/// a single bad session never aborts the sweep.
/// What: (1) lists all sessions, (2) prunes stale counters, (3) for each Active
/// session calls the verdict provider, (4) calls `apply_verdict`, (5) fires the
/// action if the decision warrants it.
/// Test: covered indirectly via the public `apply_verdict` unit tests.
async fn run_one_sweep<P: IdleVerdictProvider>(
    manager: &Arc<SessionManager>,
    cfg: &IdleAutoStopConfig,
    provider: &P,
    state: &mut IdleReaperState,
) {
    let sessions = manager.list().await;
    let active_ids: std::collections::HashSet<ManagedSessionId> = sessions
        .iter()
        .filter(|r| matches!(r.state, ManagedSessionState::Active))
        .map(|r| r.id)
        .collect();

    state.prune_stale(&active_ids);

    for record in sessions
        .iter()
        .filter(|r| matches!(r.state, ManagedSessionState::Active))
    {
        let verdict_str = provider.verdict(&record.tmux_name).await;
        let counter = state.counter_mut(&record.id);
        let decision = apply_verdict(counter, verdict_str.as_deref(), cfg);

        match decision {
            ReaperDecision::Continue => {
                debug!(
                    id = %record.id,
                    name = %record.tmux_name,
                    verdict = ?verdict_str,
                    idle_hits = counter.idle_hits,
                    done_hits = counter.done_hits,
                    "idle-reaper: no action"
                );
            }
            ReaperDecision::Stop => {
                info!(id = %record.id, name = %record.tmux_name, "idle-reaper: idle threshold reached; stopping session");
                state.remove(&record.id);
                if let Err(e) = manager.stop(&record.id).await {
                    warn!(id = %record.id, "idle-reaper: stop failed: {e}");
                }
            }
            ReaperDecision::Decommission => {
                info!(id = %record.id, name = %record.tmux_name, "idle-reaper: done threshold reached; decommissioning session");
                state.remove(&record.id);
                if let Err(e) = manager.decommission(&record.id).await {
                    warn!(id = %record.id, "idle-reaper: decommission failed: {e}");
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::IdleAutoStopConfig;

    fn cfg(idle: u32, done: u32) -> IdleAutoStopConfig {
        IdleAutoStopConfig {
            enabled: true,
            poll_interval_secs: 300,
            idle_consecutive_threshold: idle,
            done_consecutive_threshold: done,
        }
    }

    /// Why: the idle counter must increment on each consecutive idle verdict.
    /// What: applies "idle" twice, asserts idle_hits = 2 and decision = Continue.
    /// Test: this test.
    #[test]
    fn counter_increments_on_idle() {
        let mut counter = SessionHitCounter::default();
        let c = cfg(3, 1);
        assert_eq!(
            apply_verdict(&mut counter, Some("idle"), &c),
            ReaperDecision::Continue
        );
        assert_eq!(counter.idle_hits, 1);
        assert_eq!(
            apply_verdict(&mut counter, Some("idle"), &c),
            ReaperDecision::Continue
        );
        assert_eq!(counter.idle_hits, 2);
    }

    /// Why: a non-idle verdict must reset the idle counter so short pauses never
    /// accumulate toward the threshold.
    /// What: increments idle to 2, then applies "working", asserts idle_hits = 0.
    /// Test: this test.
    #[test]
    fn counter_resets_on_non_idle() {
        let mut counter = SessionHitCounter::default();
        let c = cfg(3, 1);
        apply_verdict(&mut counter, Some("idle"), &c);
        apply_verdict(&mut counter, Some("idle"), &c);
        assert_eq!(counter.idle_hits, 2);
        apply_verdict(&mut counter, Some("working"), &c);
        assert_eq!(counter.idle_hits, 0, "idle counter must reset on working");
        assert_eq!(counter.done_hits, 0);
    }

    /// Why: the Stop decision must fire exactly when idle_hits reaches the threshold.
    /// What: applies "idle" `threshold` times; asserts Stop on the last application.
    /// Test: this test.
    #[test]
    fn stop_fires_at_threshold() {
        let mut counter = SessionHitCounter::default();
        let c = cfg(3, 5);
        assert_eq!(
            apply_verdict(&mut counter, Some("idle"), &c),
            ReaperDecision::Continue
        );
        assert_eq!(
            apply_verdict(&mut counter, Some("idle"), &c),
            ReaperDecision::Continue
        );
        assert_eq!(
            apply_verdict(&mut counter, Some("idle"), &c),
            ReaperDecision::Stop
        );
    }

    /// Why: the Decommission decision must fire when done_hits reaches the threshold.
    /// What: applies "done" once with done_threshold = 1; asserts Decommission.
    /// Test: this test.
    #[test]
    fn decommission_fires_at_done_threshold() {
        let mut counter = SessionHitCounter::default();
        let c = cfg(3, 1);
        assert_eq!(
            apply_verdict(&mut counter, Some("done"), &c),
            ReaperDecision::Decommission
        );
    }

    /// Why: `None` verdict (no classifier response) must be treated the same as
    /// `unknown` — counters reset, no action.
    /// What: pre-increments idle to 2, applies None, asserts counters zero and Continue.
    /// Test: this test.
    #[test]
    fn none_verdict_resets_counters() {
        let mut counter = SessionHitCounter::default();
        let c = cfg(3, 1);
        apply_verdict(&mut counter, Some("idle"), &c);
        apply_verdict(&mut counter, Some("idle"), &c);
        let decision = apply_verdict(&mut counter, None, &c);
        assert_eq!(decision, ReaperDecision::Continue);
        assert_eq!(counter.idle_hits, 0, "None verdict must reset idle counter");
    }

    /// Why: the `blocked-on-permission` verdict (in any separator form) must never
    /// trigger a stop — the operator must approve the decision first.
    /// What: applies "blocked-on-permission" with idle_threshold = 1; asserts Continue.
    /// Test: this test.
    #[test]
    fn blocked_verdict_never_stops() {
        let mut counter = SessionHitCounter::default();
        let c = cfg(1, 1);
        let decision = apply_verdict(&mut counter, Some("blocked-on-permission"), &c);
        assert_eq!(decision, ReaperDecision::Continue);
        assert_eq!(counter.idle_hits, 0);
    }

    /// Why: a done verdict must reset the idle counter (and vice versa) so the
    /// two counters are independent.
    /// What: increments idle to 2, applies "done", asserts idle_hits = 0 and
    /// done_hits = 1.
    /// Test: this test.
    #[test]
    fn done_resets_idle_counter() {
        let mut counter = SessionHitCounter::default();
        let c = cfg(5, 2);
        apply_verdict(&mut counter, Some("idle"), &c);
        apply_verdict(&mut counter, Some("idle"), &c);
        assert_eq!(counter.idle_hits, 2);
        apply_verdict(&mut counter, Some("done"), &c);
        assert_eq!(counter.idle_hits, 0, "done must reset idle counter");
        assert_eq!(counter.done_hits, 1);
    }

    /// Why: `prune_stale` must remove entries for sessions that are no longer
    /// Active so the counter map does not grow unboundedly.
    /// What: inserts two counters, prunes with only one id in the active set,
    /// asserts only the active counter survives.
    /// Test: this test.
    #[test]
    fn prune_removes_stale_counters() {
        let a = ManagedSessionId::new();
        let b = ManagedSessionId::new();
        let mut rs = IdleReaperState::new();
        rs.counter_mut(&a).idle_hits = 2;
        rs.counter_mut(&b).idle_hits = 1;
        let active = std::collections::HashSet::from([a]);
        rs.prune_stale(&active);
        assert!(rs.counters.contains_key(&a), "active counter must survive");
        assert!(
            !rs.counters.contains_key(&b),
            "stale counter must be pruned"
        );
    }

    /// Why: the loop must exit cleanly when the cancellation token fires (same
    /// contract as `reap_loop` and `orphan_gc_loop`).
    /// What: spawns the loop with a stub provider and a temp manager, immediately
    /// cancels, awaits the handle, asserts no panic.
    /// Test: this test.
    #[tokio::test]
    async fn cancel_token_stops_idle_reaper_cleanly() {
        use crate::daemon::state::DaemonState;

        struct AlwaysIdleProvider;
        impl IdleVerdictProvider for AlwaysIdleProvider {
            async fn verdict(&self, _: &str) -> Option<String> {
                Some("idle".into())
            }
        }

        let state = DaemonState::shared();
        let mgr = state.session_manager().await;
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let handle = tokio::spawn(idle_reaper_loop(
            mgr,
            IdleAutoStopConfig::default(),
            Arc::new(AlwaysIdleProvider),
            token,
        ));
        cancel.cancel();
        handle
            .await
            .expect("idle_reaper_loop must not panic on cancel");
    }
}
