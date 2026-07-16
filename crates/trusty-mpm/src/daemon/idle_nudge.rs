//! Daemon-side auto-nudge for idle-parked managed sessions (#2621).
//!
//! Why: `core::idle_parking` (shipped in #2610/PR #2620) already flags a
//! `Stop`/`SubagentStop` whose final assistant message parks the task on a
//! background monitor/notification, and forwards `payload.idle_parking` to the
//! daemon. Until now the daemon only recorded the flag; a human still had to
//! notice the stall and send a manual nudge. This module closes that loop: when
//! a parking hook arrives it correlates the Claude session to a managed session,
//! checks the conservative safety rails, and injects a short foreground-block
//! instruction into the parked pane — the same message a human PM was typing by
//! hand. It is opt-in (`[idle_nudge].enabled = false` by default) and bounded by
//! a per-session cap and cooldown so a stuck session is never spammed.
//! What: [`has_live_children`] is the pure "is the session still working?"
//! predicate over its delegation list; [`run_nudge`] is the injectable core —
//! correlate → gate ([`crate::core::idle_nudge::decide_nudge`]) → inject →
//! record — returning a [`NudgeOutcome`] for logging and tests;
//! [`maybe_nudge_parked_session`] is the thin `DaemonState` entry point the
//! `ingest_hook` handler spawns off the hot path. All decision logic lives in
//! the pure `core::idle_nudge`; this module owns only the effects (config load,
//! session-manager correlation, tmux inject) and the structured surfacing logs.
//! Test: `has_live_children_*`, `run_nudge_*` in the inline `tests` module.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{info, warn};

use crate::core::agent::{Delegation, DelegationStatus};
use crate::core::config::MpmConfig;
use crate::core::hook::HookEvent;
use crate::core::idle_nudge::{IdleNudgeConfig, NudgeDecision, NudgeLedger, decide_nudge};
use crate::core::session::SessionId;
use crate::core::sm::control::Submit;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedSessionState, SessionManager};

/// Does this session still have live tracked children? (pure)
///
/// Why: the #2621 design note requires the "no live tracked children"
/// precondition so a session that is genuinely still working — one whose
/// delegated subagents are mid-flight — is never nudged as if it were stalled.
/// A leaf engineer agent (the common dogfooding park) has no live children, so
/// this returns `false` and the nudge proceeds; a PM with a running delegation
/// returns `true` and is left alone.
/// What: `true` iff any delegation is [`DelegationStatus::Queued`] or
/// [`DelegationStatus::Running`]. `Completed`/`Failed`/`Cancelled` delegations
/// are terminal and do not count. An empty list (no tracked delegations at all)
/// is `false` — the conservative default that lets a leaf agent be nudged.
/// Test: `has_live_children_true_for_running`, `has_live_children_false_when_terminal`,
/// `has_live_children_false_when_empty`.
pub fn has_live_children(delegations: &[Delegation]) -> bool {
    delegations.iter().any(|d| {
        matches!(
            d.status,
            DelegationStatus::Queued | DelegationStatus::Running
        )
    })
}

/// The result of one nudge attempt, for structured logging and tests.
///
/// Why: `run_nudge` both acts and must explain what it did (or why it did not);
/// a single enum carries the outcome without a side channel and gives the unit
/// tests a precise assertion target.
/// What: correlation miss, a skip with its [`crate::core::idle_nudge::SkipReason`],
/// a successful nudge, or an inject failure carrying the error text.
/// Test: every `run_nudge_*` test asserts on this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeOutcome {
    /// No Active managed session correlates to the hook's Claude session id —
    /// the parked runtime is unmanaged, so there is no pane to nudge.
    NoManagedSession,
    /// A safety rail suppressed the nudge; `.0` names the first rail that failed.
    Skipped(crate::core::idle_nudge::SkipReason),
    /// The nudge was injected into the pane.
    Nudged,
    /// The nudge decision passed but the tmux inject failed; `.0` is the error.
    InjectFailed(String),
}

/// Correlate → prune → gate → inject → record for one parking hook (injectable
/// core).
///
/// Why: keeping the effectful sequence in one function that takes its
/// collaborators explicitly (manager, ledger, delegations, config) — rather than
/// reaching into `DaemonState` — makes it unit-testable against a
/// `FakeNoopTmuxDriver` exactly like `idle_reaper::run_one_sweep`, with no real
/// tmux and no on-disk config.
/// What: (1) lists all managed sessions and immediately
/// [`NudgeLedger::prune`]s the ledger down to that live set — this is the
/// ONLY production call site that prunes the ledger (#2621 code-critic HIGH
/// 2), so it must run on every attempt regardless of the outcome below, not
/// just on a successful correlation; (2) finds the single Active managed
/// session whose `claude_session_id` matches `claude_session_id` (→
/// [`NudgeOutcome::NoManagedSession`] when none); (3) computes
/// [`has_live_children`] from `delegations`; (4) under the ledger lock,
/// snapshots the session's [`crate::core::idle_nudge::NudgeRecord`] and runs
/// [`decide_nudge`] with `phrase` (the caller only invokes this on a real
/// park, so `phrase` is normally `Some`; `decide_nudge` itself rejects a
/// human-addressed phrase via [`crate::core::idle_parking::is_nudge_eligible`]);
/// (5) on [`NudgeDecision::Nudge`] records the delivery in the ledger *before*
/// releasing the lock (so a concurrent second parking hook cannot
/// double-nudge) and then injects `cfg.message` with [`Submit::Enter`]; (6)
/// returns the [`NudgeOutcome`]. The record-before-inject ordering means a
/// failed inject still consumes one nudge budget slot — a deliberately
/// conservative bias toward fewer nudges. Bumps `last_activity_at` via the
/// manager's normal `inject` path.
/// Test: `run_nudge_injects_when_idle_and_enabled`,
/// `run_nudge_skips_when_disabled`, `run_nudge_skips_when_live_children`,
/// `run_nudge_skips_human_wait_phrase`, `run_nudge_no_managed_session`,
/// `run_nudge_respects_cap`, `run_nudge_prunes_stale_ledger_entries`.
pub async fn run_nudge(
    mgr: &SessionManager,
    ledger: &parking_lot::Mutex<NudgeLedger>,
    delegations: &[Delegation],
    cfg: &IdleNudgeConfig,
    claude_session_id: &str,
    phrase: Option<&str>,
    now: DateTime<Utc>,
) -> NudgeOutcome {
    // 1. Correlate the Claude session id to an Active managed session, and — on
    //    the SAME `mgr.list()` call — prune the ledger to the current live set
    //    (#2621 code-critic HIGH 2: this is the only place `prune` is called in
    //    production; it must run unconditionally, before the correlation
    //    outcome is known, so the ledger never grows unbounded even for
    //    sessions that stop correlating).
    let records = mgr.list().await;
    let live_ids: std::collections::HashSet<uuid::Uuid> = records.iter().map(|r| r.id.0).collect();
    ledger.lock().prune(&live_ids);

    let Some(record) = records.iter().find(|r| {
        matches!(r.state, ManagedSessionState::Active)
            && r.claude_session_id.as_deref() == Some(claude_session_id)
    }) else {
        return NudgeOutcome::NoManagedSession;
    };
    let managed_id = record.id;

    // 2. Precondition: is the session still working?
    let live_children = has_live_children(delegations);

    // 3. Gate under the ledger lock; record the delivery before releasing it so a
    //    concurrent parking hook for the same session cannot double-nudge.
    let decision = {
        let mut guard = ledger.lock();
        let snapshot = guard.snapshot(managed_id.0);
        let decision = decide_nudge(cfg, phrase, live_children, &snapshot, now);
        if decision == NudgeDecision::Nudge {
            guard.record_nudge(managed_id.0, now);
        }
        decision
    };

    match decision {
        NudgeDecision::Skip(reason) => NudgeOutcome::Skipped(reason),
        NudgeDecision::Nudge => match mgr.inject(&managed_id, &cfg.message, Submit::Enter).await {
            Ok(()) => NudgeOutcome::Nudged,
            Err(e) => NudgeOutcome::InjectFailed(e.to_string()),
        },
    }
}

/// Spawn the surface-and-nudge task iff this hook is a flagged park (#2621).
///
/// Why: the `ingest_hook` handler must trigger the nudge without adding latency
/// to (or a failure path onto) the hook response, and without bloating the
/// already-oversized `api.rs`. Concentrating the "is this a park?" gate and the
/// `tokio::spawn` here keeps the handler's footprint to a single call.
/// What: returns immediately unless `event` is `Stop`/`SubagentStop` AND
/// `payload["idle_parking"] == true` (the flag `tm hook` sets via
/// `core::idle_parking`). When it matches, it clones the owned data the detached
/// task needs (`session_id`, optional `idle_parking_phrase`) and spawns
/// [`maybe_nudge_parked_session`]. Fire-and-forget — the task's own best-effort
/// logging is the only observation of its result.
/// Test: the gate is trivial routing; the spawned work is covered by the
/// `run_nudge_*` tests. Exercised end to end by any daemon integration test that
/// posts a parking `Stop` hook.
pub fn spawn_if_parked(
    state: &Arc<DaemonState>,
    event: HookEvent,
    session_id: &str,
    payload: &serde_json::Value,
) {
    if !matches!(event, HookEvent::Stop | HookEvent::SubagentStop) {
        return;
    }
    if payload
        .get("idle_parking")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return;
    }
    let state = Arc::clone(state);
    let session_id = session_id.to_string();
    let phrase = payload
        .get("idle_parking_phrase")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    tokio::spawn(async move {
        maybe_nudge_parked_session(&state, &session_id, phrase.as_deref()).await;
    });
}

/// `DaemonState` entry point: surface a park and auto-nudge if enabled (#2621).
///
/// Why: the `ingest_hook` handler spawns this off the request hot path when a
/// `Stop`/`SubagentStop` arrives with `payload.idle_parking = true`. It performs
/// the daemon-side surfacing (a loud structured `warn!` so the stall is visible
/// in the daemon log / event stream even when nudging is off) and, when the
/// feature is enabled, drives [`run_nudge`] against the live session manager.
/// What: emits the surfacing log; loads `[idle_nudge]` via
/// [`MpmConfig::load_default`]; resolves the session manager and this session's
/// tracked delegations (keyed by the Claude session UUID); calls [`run_nudge`];
/// and logs the [`NudgeOutcome`]. Entirely best-effort — every failure path is
/// logged and swallowed so it can never affect the hook response. A
/// non-UUID `claude_session_id` (or one with no tracked delegations) simply
/// yields an empty delegation list, which is the safe default.
/// Test: exercised indirectly by `run_nudge_*`; the config-load + surfacing shell
/// is side-effect-only (a `tracing` log + `DaemonState` reads).
pub async fn maybe_nudge_parked_session(
    state: &Arc<DaemonState>,
    claude_session_id: &str,
    phrase: Option<&str>,
) {
    // Daemon-side surfacing (#2621 part 1): make the park a first-class, greppable
    // signal in the daemon log regardless of whether nudging is enabled.
    warn!(
        target: "trusty_mpm::idle_parking",
        session = %claude_session_id,
        phrase = phrase.unwrap_or("<unknown>"),
        "idle-parking detected on a managed session turn-end (#2621); a stopped agent cannot be re-woken by a self-spawned monitor"
    );

    let cfg = MpmConfig::load_default().idle_nudge;
    if !cfg.enabled {
        info!(
            target: "trusty_mpm::idle_parking",
            session = %claude_session_id,
            "auto-nudge disabled ([idle_nudge].enabled = false); surfaced only"
        );
        return;
    }

    let delegations = uuid::Uuid::parse_str(claude_session_id)
        .map(|u| state.delegations_for(SessionId(u)))
        .unwrap_or_default();

    let mgr = state.session_manager().await;
    let outcome = run_nudge(
        &mgr,
        state.nudge_ledger(),
        &delegations,
        &cfg,
        claude_session_id,
        phrase,
        Utc::now(),
    )
    .await;

    match &outcome {
        NudgeOutcome::Nudged => info!(
            target: "trusty_mpm::idle_parking",
            session = %claude_session_id,
            "auto-nudged idle-parked managed session (#2621)"
        ),
        NudgeOutcome::Skipped(reason) => info!(
            target: "trusty_mpm::idle_parking",
            session = %claude_session_id,
            reason = reason.tag(),
            "auto-nudge skipped (#2621)"
        ),
        NudgeOutcome::NoManagedSession => info!(
            target: "trusty_mpm::idle_parking",
            session = %claude_session_id,
            "auto-nudge skipped: no Active managed session correlates to this Claude session (#2621)"
        ),
        NudgeOutcome::InjectFailed(e) => warn!(
            target: "trusty_mpm::idle_parking",
            session = %claude_session_id,
            error = %e,
            "auto-nudge inject failed (#2621)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::{Delegation, ModelTier};
    use crate::core::idle_nudge::SkipReason;
    use crate::core::session::SessionId;
    use crate::session_manager::{FakeNoopTmuxDriver, ManagedSessionId};
    use tempfile::TempDir;

    /// A machine-wait phrase (nudge-eligible), the standard "genuinely parked"
    /// input for these tests. Must match a `core::idle_parking::PARKING_PHRASES`
    /// entry that is NOT in `HUMAN_ADDRESSED_PHRASES`.
    const MACHINE_PHRASE: &str = "i'll wait for the";
    /// A human-addressed phrase (NOT nudge-eligible) for `run_nudge_skips_human_wait_phrase`.
    const HUMAN_PHRASE: &str = "let me know once";

    fn enabled_cfg() -> IdleNudgeConfig {
        IdleNudgeConfig {
            enabled: true,
            max_nudges_per_session: 2,
            cooldown_secs: 300,
            message: "resume in the foreground now".into(),
        }
    }

    fn delegation_with(status: DelegationStatus) -> Delegation {
        let mut d = Delegation::new(
            SessionId::new(),
            None,
            "rust-engineer",
            ModelTier::Sonnet,
            "x",
        );
        d.status = status;
        d
    }

    #[test]
    fn has_live_children_true_for_running() {
        assert!(has_live_children(&[delegation_with(
            DelegationStatus::Running
        )]));
        assert!(has_live_children(&[delegation_with(
            DelegationStatus::Queued
        )]));
    }

    #[test]
    fn has_live_children_false_when_terminal() {
        assert!(!has_live_children(&[
            delegation_with(DelegationStatus::Completed),
            delegation_with(DelegationStatus::Failed),
            delegation_with(DelegationStatus::Cancelled),
        ]));
    }

    #[test]
    fn has_live_children_false_when_empty() {
        assert!(!has_live_children(&[]));
    }

    /// Build a manager with one Active managed session correlated to a Claude
    /// session id. Mirrors the `idle_reaper` sweep-test scaffold: create →
    /// stop → resume drives the record to Active under the no-op tmux driver.
    async fn manager_with_active_correlated(
        dir: &TempDir,
        claude_id: &str,
    ) -> (Arc<SessionManager>, ManagedSessionId) {
        let mgr = SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
            .await
            .expect("manager");
        let rec = mgr
            .create(
                "parked task".into(),
                Some(dir.path().to_path_buf()),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        mgr.stop(&rec.id).await.expect("stop");
        mgr.resume(&rec.id).await.expect("resume");
        mgr.set_claude_session_id(&rec.id, claude_id)
            .await
            .expect("set claude id");
        (Arc::new(mgr), rec.id)
    }

    #[tokio::test]
    async fn run_nudge_injects_when_idle_and_enabled() {
        let dir = TempDir::new().unwrap();
        let claude_id = uuid::Uuid::new_v4().to_string();
        let (mgr, id) = manager_with_active_correlated(&dir, &claude_id).await;
        let ledger = parking_lot::Mutex::new(NudgeLedger::new());
        let cfg = enabled_cfg();
        let now = Utc::now();

        let outcome = run_nudge(
            &mgr,
            &ledger,
            &[],
            &cfg,
            &claude_id,
            Some(MACHINE_PHRASE),
            now,
        )
        .await;
        assert_eq!(outcome, NudgeOutcome::Nudged);
        // The ledger must record exactly one delivery for the managed session.
        assert_eq!(ledger.lock().snapshot(id.0).count, 1);
    }

    #[tokio::test]
    async fn run_nudge_skips_when_disabled() {
        let dir = TempDir::new().unwrap();
        let claude_id = uuid::Uuid::new_v4().to_string();
        let (mgr, id) = manager_with_active_correlated(&dir, &claude_id).await;
        let ledger = parking_lot::Mutex::new(NudgeLedger::new());
        let mut cfg = enabled_cfg();
        cfg.enabled = false;

        let outcome = run_nudge(
            &mgr,
            &ledger,
            &[],
            &cfg,
            &claude_id,
            Some(MACHINE_PHRASE),
            Utc::now(),
        )
        .await;
        assert_eq!(outcome, NudgeOutcome::Skipped(SkipReason::Disabled));
        assert_eq!(
            ledger.lock().snapshot(id.0).count,
            0,
            "no delivery recorded"
        );
    }

    #[tokio::test]
    async fn run_nudge_skips_when_live_children() {
        let dir = TempDir::new().unwrap();
        let claude_id = uuid::Uuid::new_v4().to_string();
        let (mgr, _id) = manager_with_active_correlated(&dir, &claude_id).await;
        let ledger = parking_lot::Mutex::new(NudgeLedger::new());
        let cfg = enabled_cfg();
        let children = [delegation_with(DelegationStatus::Running)];

        let outcome = run_nudge(
            &mgr,
            &ledger,
            &children,
            &cfg,
            &claude_id,
            Some(MACHINE_PHRASE),
            Utc::now(),
        )
        .await;
        assert_eq!(outcome, NudgeOutcome::Skipped(SkipReason::HasLiveChildren));
    }

    #[tokio::test]
    async fn run_nudge_skips_human_wait_phrase() {
        // #2621 code-critic HIGH 1: a session parked on a HUMAN-addressed wait
        // ("let me know once…") must never be nudged end to end, even though it
        // correlates cleanly and has no live children.
        let dir = TempDir::new().unwrap();
        let claude_id = uuid::Uuid::new_v4().to_string();
        let (mgr, id) = manager_with_active_correlated(&dir, &claude_id).await;
        let ledger = parking_lot::Mutex::new(NudgeLedger::new());
        let cfg = enabled_cfg();

        let outcome = run_nudge(
            &mgr,
            &ledger,
            &[],
            &cfg,
            &claude_id,
            Some(HUMAN_PHRASE),
            Utc::now(),
        )
        .await;
        assert_eq!(outcome, NudgeOutcome::Skipped(SkipReason::HumanWait));
        assert_eq!(
            ledger.lock().snapshot(id.0).count,
            0,
            "a human-wait skip must not consume a nudge budget slot"
        );
    }

    #[tokio::test]
    async fn run_nudge_no_managed_session() {
        let dir = TempDir::new().unwrap();
        // Correlate the manager to a DIFFERENT claude id than we nudge with.
        let (mgr, _id) =
            manager_with_active_correlated(&dir, &uuid::Uuid::new_v4().to_string()).await;
        let ledger = parking_lot::Mutex::new(NudgeLedger::new());
        let cfg = enabled_cfg();

        let outcome = run_nudge(
            &mgr,
            &ledger,
            &[],
            &cfg,
            &uuid::Uuid::new_v4().to_string(),
            Some(MACHINE_PHRASE),
            Utc::now(),
        )
        .await;
        assert_eq!(outcome, NudgeOutcome::NoManagedSession);
    }

    #[tokio::test]
    async fn run_nudge_respects_cap() {
        let dir = TempDir::new().unwrap();
        let claude_id = uuid::Uuid::new_v4().to_string();
        let (mgr, id) = manager_with_active_correlated(&dir, &claude_id).await;
        let ledger = parking_lot::Mutex::new(NudgeLedger::new());
        let mut cfg = enabled_cfg();
        cfg.max_nudges_per_session = 1;
        cfg.cooldown_secs = 0; // isolate the cap from the cooldown

        // First nudge lands.
        let now = Utc::now();
        assert_eq!(
            run_nudge(
                &mgr,
                &ledger,
                &[],
                &cfg,
                &claude_id,
                Some(MACHINE_PHRASE),
                now
            )
            .await,
            NudgeOutcome::Nudged
        );
        // Second (cooldown elapsed) is blocked by the per-session cap.
        let outcome = run_nudge(
            &mgr,
            &ledger,
            &[],
            &cfg,
            &claude_id,
            Some(MACHINE_PHRASE),
            now + chrono::Duration::seconds(1),
        )
        .await;
        assert_eq!(outcome, NudgeOutcome::Skipped(SkipReason::CapExhausted));
        assert_eq!(ledger.lock().snapshot(id.0).count, 1, "cap holds at 1");
    }

    #[tokio::test]
    async fn run_nudge_prunes_stale_ledger_entries() {
        // #2621 code-critic HIGH 2: `run_nudge` must prune the ledger against the
        // CURRENT managed-session set on every call, not just accumulate entries
        // for the lifetime of the daemon. Build a ledger with a stale entry (a
        // UUID no managed session will ever correlate to) alongside a real,
        // correlated session; after one `run_nudge` call the stale entry must be
        // gone while the real session's own record survives.
        let dir = TempDir::new().unwrap();
        let claude_id = uuid::Uuid::new_v4().to_string();
        let (mgr, id) = manager_with_active_correlated(&dir, &claude_id).await;
        let ledger = parking_lot::Mutex::new(NudgeLedger::new());
        let stale_id = uuid::Uuid::new_v4();
        let now = Utc::now();
        ledger.lock().record_nudge(stale_id, now);
        ledger
            .lock()
            .record_nudge(id.0, now - chrono::Duration::seconds(10_000));

        let cfg = enabled_cfg();
        let outcome = run_nudge(
            &mgr,
            &ledger,
            &[],
            &cfg,
            &claude_id,
            Some(MACHINE_PHRASE),
            now,
        )
        .await;
        assert_eq!(outcome, NudgeOutcome::Nudged);

        let guard = ledger.lock();
        assert_eq!(
            guard.snapshot(stale_id),
            crate::core::idle_nudge::NudgeRecord::default(),
            "stale ledger entry for a non-existent session must be pruned"
        );
        assert_eq!(
            guard.snapshot(id.0).count,
            2,
            "the real session's record survives pruning and reflects this nudge"
        );
    }
}
