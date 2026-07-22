//! Idle auto-stop daemon loop — opt-in background task (#1816).
//!
//! Why: long-lived idle sessions consume Claude Max rate-limit slots without
//! doing useful work. This module provides a background task that periodically
//! classifies `Active` sessions using the existing `ActivityMonitor` hash-cached
//! LLM verdict, maintains per-session consecutive-hit counters, and calls
//! `SessionManager::stop()` / `decommission()` when the thresholds are crossed.
//! The feature is completely OFF by default (`idle_auto_stop.enabled = false`
//! in `~/.trusty-mpm/config.toml`) so existing installations see zero behavior
//! change. There is a SECOND, teardown-safe gate: `idle_auto_stop.dry_run`
//! (default `true`). While `dry_run` is set the loop classifies sessions and
//! LOGS the stop/decommission it *would* perform but never tears anything down;
//! an operator opts in to real teardown by setting `dry_run = false` (#1783,
//! mirroring trusty-search's report-only-first auto-prune convention #1782).
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
///
/// **Invariant:** callers MUST call [`IdleReaperState::remove`] immediately
/// after receiving `Stop` or `Decommission` before the next poll iteration.
/// `run_one_sweep` enforces this; tests verify it in
/// `counter_cleared_after_stop_fires`.
/// Test: `counter_increments_on_idle`, `counter_resets_on_non_idle`,
/// `stop_fires_at_threshold`, `decommission_fires_at_done_threshold`,
/// `counter_cleared_after_stop_fires`.
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
    tick.tick().await; // discard the immediate t=0 tick so newly-started sessions
    // are not classified before any activity can register.
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
/// action if the decision warrants it — UNLESS `cfg.dry_run` is set, in which
/// case it only logs the action it would take and leaves the session untouched.
/// Test: `sweep_dry_run_does_not_stop`, `sweep_live_stops_idle_session`,
/// `sweep_live_decommissions_done_session`; decision logic via `apply_verdict`.
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
            ReaperDecision::Stop if cfg.dry_run => {
                // Report-only: log the action we WOULD take and leave the counter
                // in place so the next sweep keeps reporting until the operator
                // either acts on the session or clears `dry_run`.
                info!(
                    id = %record.id,
                    name = %record.tmux_name,
                    "idle-reaper (dry-run): would stop session (idle threshold reached); set idle_auto_stop.dry_run=false to enact"
                );
            }
            ReaperDecision::Stop => {
                info!(id = %record.id, name = %record.tmux_name, "idle-reaper: idle threshold reached; stopping session");
                state.remove(&record.id);
                if let Err(e) = manager.stop(&record.id).await {
                    warn!(id = %record.id, "idle-reaper: stop failed: {e}");
                }
            }
            ReaperDecision::Decommission if cfg.dry_run => {
                info!(
                    id = %record.id,
                    name = %record.tmux_name,
                    "idle-reaper (dry-run): would decommission session (done threshold reached); set idle_auto_stop.dry_run=false to enact"
                );
            }
            ReaperDecision::Decommission => {
                info!(id = %record.id, name = %record.tmux_name, "idle-reaper: done threshold reached; decommissioning session");
                state.remove(&record.id);
                if let Err(e) = manager.decommission(&record.id, None).await {
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
        // `dry_run: false` here so the `apply_verdict` decision-logic tests below
        // exercise the enacting path; the sweep-level tests build their own cfg.
        IdleAutoStopConfig {
            enabled: true,
            dry_run: false,
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

    /// Why: enforces the remove-on-fire invariant — after Stop fires, the
    /// loop removes the entry from `IdleReaperState`; the next poll for the
    /// same id starts with a fresh counter at zero, preventing an immediate
    /// re-fire on a stale counter.
    /// What: drives a counter to Stop threshold, calls `state.remove()` (as
    /// `run_one_sweep` does), then applies another idle verdict and asserts
    /// the decision is `Continue` (counter restarted) rather than `Stop`.
    /// Test: this test.
    #[test]
    fn counter_cleared_after_stop_fires() {
        let id = ManagedSessionId::new();
        let mut rs = IdleReaperState::new();
        let c = cfg(2, 5);
        // Drive to Stop.
        let counter = rs.counter_mut(&id);
        assert_eq!(
            apply_verdict(counter, Some("idle"), &c),
            ReaperDecision::Continue
        );
        assert_eq!(
            apply_verdict(counter, Some("idle"), &c),
            ReaperDecision::Stop
        );
        // Simulate run_one_sweep removing the counter before the next tick.
        rs.remove(&id);
        // Next tick: fresh counter — must not immediately re-fire.
        let counter = rs.counter_mut(&id);
        let decision = apply_verdict(counter, Some("idle"), &c);
        assert_eq!(
            decision,
            ReaperDecision::Continue,
            "counter must restart after remove; second idle must not immediately re-fire"
        );
        assert_eq!(
            counter.idle_hits, 1,
            "fresh counter must show exactly 1 idle hit"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Sweep-level end-to-end tests (#1783): idle→stop and done→decommission,
    // plus the report-only (dry_run) safety gate. These drive `run_one_sweep`
    // against a real `SessionManager` (backed by a no-op tmux driver + temp
    // store) so the full classify → decide → act path is exercised.
    // ─────────────────────────────────────────────────────────────

    use crate::session_manager::{FakeNoopTmuxDriver, ManagedSessionState, SessionManager};
    use tempfile::TempDir;

    /// A verdict provider that returns the same canned verdict for every session.
    ///
    /// Why: sweep tests must be deterministic and never touch an LLM or tmux —
    /// this stub lets a test assert exactly what the reaper does for a fixed
    /// classifier verdict.
    /// What: returns `Some(self.0)` for every session name.
    /// Test: used by the `sweep_*` tests below.
    struct FixedVerdictProvider(&'static str);
    impl IdleVerdictProvider for FixedVerdictProvider {
        async fn verdict(&self, _: &str) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    /// Build a manager with a no-op tmux driver and one session in `Active`.
    ///
    /// Why: `run_one_sweep` only considers `Active` sessions; `create` yields a
    /// `Provisioning` record, so we drive it through stop→resume (both public,
    /// both no-ops under the fake driver) to reach `Active` deterministically.
    /// What: returns the `Arc<SessionManager>` and the new session's id.
    /// Test: used by every `sweep_*` test below.
    async fn manager_with_active_session(dir: &TempDir) -> (Arc<SessionManager>, ManagedSessionId) {
        let mgr = SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
            .await
            .expect("manager");
        let rec = mgr
            .create(
                "idle task".into(),
                Some(dir.path().to_path_buf()),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        // Provisioning → Stopped → Active (no-op tmux driver makes this a pure
        // state transition with no real process work).
        mgr.stop(&rec.id).await.expect("stop");
        mgr.resume(&rec.id).await.expect("resume");
        (Arc::new(mgr), rec.id)
    }

    async fn state_of(mgr: &SessionManager, id: &ManagedSessionId) -> ManagedSessionState {
        mgr.list()
            .await
            .into_iter()
            .find(|r| &r.id == id)
            .expect("session present")
            .state
    }

    /// Why: the report-only gate (#1783) must NEVER tear down a session — even
    /// when the idle threshold is crossed — while `dry_run` is set.
    /// What: runs one sweep with an always-idle provider, `dry_run = true`, and
    /// `idle_threshold = 1`; asserts the session stays `Active`.
    /// Test: this test.
    #[tokio::test]
    async fn sweep_dry_run_does_not_stop() {
        let dir = TempDir::new().unwrap();
        let (mgr, id) = manager_with_active_session(&dir).await;
        let mut c = cfg(1, 1);
        c.dry_run = true;
        let provider = FixedVerdictProvider("idle");
        let mut rs = IdleReaperState::new();
        run_one_sweep(&mgr, &c, &provider, &mut rs).await;
        assert_eq!(
            state_of(&mgr, &id).await,
            ManagedSessionState::Active,
            "dry-run sweep must NOT stop the session"
        );
    }

    /// Why: with the report-only gate cleared, a session that crosses the idle
    /// threshold must actually transition to `Stopped` (workspace intact).
    /// What: runs one sweep with an always-idle provider, `dry_run = false`, and
    /// `idle_threshold = 1`; asserts the session is `Stopped`.
    /// Test: this test.
    #[tokio::test]
    async fn sweep_live_stops_idle_session() {
        let dir = TempDir::new().unwrap();
        let (mgr, id) = manager_with_active_session(&dir).await;
        let c = cfg(1, 1); // dry_run = false
        let provider = FixedVerdictProvider("idle");
        let mut rs = IdleReaperState::new();
        run_one_sweep(&mgr, &c, &provider, &mut rs).await;
        assert_eq!(
            state_of(&mgr, &id).await,
            ManagedSessionState::Stopped,
            "live sweep must stop the idle session"
        );
    }

    /// Why: a session the classifier reports as `done` must be decommissioned
    /// (the terminal teardown) once the done threshold is crossed and `dry_run`
    /// is cleared — this is the idle→decommission arm of #1783.
    /// What: runs one sweep with an always-done provider, `dry_run = false`, and
    /// `done_threshold = 1`; asserts the session is `Decommissioned`.
    /// Test: this test.
    #[tokio::test]
    async fn sweep_live_decommissions_done_session() {
        let dir = TempDir::new().unwrap();
        let (mgr, id) = manager_with_active_session(&dir).await;
        let c = cfg(3, 1); // done_threshold = 1, dry_run = false
        let provider = FixedVerdictProvider("done");
        let mut rs = IdleReaperState::new();
        run_one_sweep(&mgr, &c, &provider, &mut rs).await;
        assert_eq!(
            state_of(&mgr, &id).await,
            ManagedSessionState::Decommissioned,
            "live sweep must decommission the done session"
        );
    }
}
