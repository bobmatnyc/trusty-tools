//! Stale a quiet `Running` delegation the OS shows is gone (#6556).
//!
//! Why: `SubagentStop` is the only signal that closes a `Running` record, and
//! it can be lost. Before this pass a lost stop left the record `Running` until
//! the six-hour `RUNNING_STALE_AFTER_SECS` sweep. Owner ruling 2026-10-11
//! (superseding 2026-09-15): a `Running` record goes `Stale` after a
//! configurable quiet bound, default 15 minutes, when there is no hook event
//! within the bound AND no live OS evidence of the agent.
//!
//! What: [`quiet_verdict`] decides one record; [`stale_quiet_delegations_at`]
//! applies it to the map with the probe and the clock injected;
//! [`stale_quiet_delegations`] is the reap-loop entry with the real probe
//! ([`probe_live_evidence`]) off the async runtime. The six-hour sweep is
//! untouched and still bounds every record this pass declines.
//!
//! # Fail direction
//!
//! Closed. Only [`LiveEvidence::Clear`] — every probe ran and found nothing —
//! writes `Stale`. A probe that finds the agent, or cannot answer, leaves the
//! record `Running`.
//!
//! # Unisolated agents stay Undeterminable
//!
//! A subagent with no tree of its own runs inside its dispatcher's `claude`
//! process in the shared checkout, so no OS probe can speak for it (see
//! `delegation_repair_probe`'s stated gap). The pass never probes it and never
//! stales it; the six-hour sweep remains its only bound (owner ruling).
//!
//! # A late stop revives
//!
//! The pass writes `Stale`, never a terminal status and never `ended_at`, and
//! `on_subagent_stop` matches on `!is_terminal()`, so a stop that arrives later
//! ends the record exactly as it would end a `Running` one.
//!
//! Test: `delegation_quiet_stale_tests.rs`.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use super::delegation_repair_probe::{LiveEvidence, probe_live_evidence};
use crate::core::agent::{Delegation, DelegationStatus};
use crate::daemon::state::DaemonState;

/// Default quiet bound, in seconds (owner ruling 2026-10-11: 15 minutes).
pub(crate) const DEFAULT_QUIET_STALE_SECS: i64 = 15 * 60;

/// Environment variable that overrides [`DEFAULT_QUIET_STALE_SECS`].
pub(crate) const QUIET_STALE_ENV: &str = "TRUSTY_MPM_DELEGATION_QUIET_STALE_SECS";

/// Largest accepted override: one year, far inside `chrono::Duration`'s range.
const MAX_QUIET_STALE_SECS: i64 = 365 * 24 * 60 * 60;

/// Parse a raw [`QUIET_STALE_ENV`] value into seconds.
///
/// Why: env vars are process-global, so the policy is a pure function a test
/// can call without mutating the environment.
/// What: a trimmed positive integer up to [`MAX_QUIET_STALE_SECS`]; `None`
/// when unset. `Err` carries the rejected text for the caller's warning.
/// Test: `the_quiet_bound_parses_overrides_and_rejects_nonsense`.
pub(crate) fn parse_quiet_stale_secs(raw: Option<&str>) -> Result<Option<i64>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    raw.trim()
        .parse::<i64>()
        .ok()
        .filter(|n| (1..=MAX_QUIET_STALE_SECS).contains(n))
        .map(Some)
        .ok_or_else(|| raw.to_string())
}

/// The quiet bound this daemon runs with.
///
/// Why: the owner ruling makes the bound configurable; the daemon's other
/// sweep knobs are env overrides (`TRUSTY_MPM_ORPHAN_GC_INTERVAL_SECS`).
/// What: [`QUIET_STALE_ENV`] when it parses, else the default. A value that
/// does not parse is logged at WARN and the default applies.
/// Test: parsing is covered by
/// `the_quiet_bound_parses_overrides_and_rejects_nonsense`.
pub(crate) fn quiet_stale_bound() -> Duration {
    let secs = match parse_quiet_stale_secs(std::env::var(QUIET_STALE_ENV).ok().as_deref()) {
        Ok(Some(secs)) => secs,
        Ok(None) => DEFAULT_QUIET_STALE_SECS,
        Err(raw) => {
            tracing::warn!(
                "{QUIET_STALE_ENV}={raw:?} is not a whole number of seconds in \
                 1..={MAX_QUIET_STALE_SECS}; using the default {DEFAULT_QUIET_STALE_SECS} s (#6556)"
            );
            DEFAULT_QUIET_STALE_SECS
        }
    };
    Duration::seconds(secs)
}

/// What the quiet-bound pass decides about one record (#6556).
///
/// Test: every test in `delegation_quiet_stale_tests.rs` asserts on this or on
/// the status it produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuietVerdict {
    /// Not `Running`, or heard from within the bound: nothing to decide.
    NotQuiet,
    /// Quiet, but the agent is not standing in a tree of its own, so no OS
    /// probe can speak for it. Left `Running`.
    Undeterminable,
    /// Quiet, isolated, and the probe found no live evidence.
    Stale,
    /// Quiet and isolated, but the probe found the agent or could not answer.
    Running(String),
}

/// The latest moment this record was heard from.
///
/// What: the newest of `last_event_at`, `started_at` and `created_at`.
fn last_heard(d: &Delegation) -> DateTime<Utc> {
    d.last_event_at
        .into_iter()
        .chain(d.started_at)
        .fold(d.created_at, DateTime::max)
}

/// Is the agent standing, right now, in a tree of its own?
///
/// What: `worktree_path` is recorded and differs from the dispatcher's `cwd`,
/// and the agent's latest hook `cwd` lies inside it. An agent that walked back
/// out into the shared checkout is unisolated again.
fn standing_in_own_tree(d: &Delegation) -> bool {
    let (Some(tree), Some(here)) = (d.worktree_path.as_deref(), d.last_agent_cwd.as_deref()) else {
        return false;
    };
    d.cwd.as_deref() != Some(tree) && here.starts_with(tree)
}

/// Decide one record (#6556).
///
/// Why: the whole policy in one pure function, so each arm is testable with
/// the probe and the clock injected.
/// What, in order: a record that is not `Running`, or was heard from within
/// `bound` of `now`, is [`QuietVerdict::NotQuiet`]; one not standing in its own
/// tree is [`QuietVerdict::Undeterminable`] and `probe` is never called;
/// otherwise `probe` decides — `Clear` is [`QuietVerdict::Stale`], `Held` and
/// `Undeterminable` are [`QuietVerdict::Running`].
/// Test: `a_quiet_isolated_record_reads_stale_only_without_live_evidence`,
/// `an_unisolated_quiet_record_stays_undeterminable`,
/// `a_custom_quiet_bound_is_honoured`.
pub(crate) fn quiet_verdict(
    d: &Delegation,
    now: DateTime<Utc>,
    bound: Duration,
    probe: impl FnOnce(&Delegation) -> LiveEvidence,
) -> QuietVerdict {
    if d.status != DelegationStatus::Running || now - last_heard(d) <= bound {
        return QuietVerdict::NotQuiet;
    }
    if !standing_in_own_tree(d) {
        return QuietVerdict::Undeterminable;
    }
    match probe(d) {
        LiveEvidence::Clear => QuietVerdict::Stale,
        LiveEvidence::Held(why) => QuietVerdict::Running(why),
        // #6556: fail closed — an unanswered probe never stales.
        LiveEvidence::Undeterminable(why) => {
            QuietVerdict::Running(format!("the liveness probe could not answer: {why}"))
        }
    }
}

/// Apply [`quiet_verdict`] to every record, with the probe and clock injected.
///
/// Why: the probe shells out (`lsof`, `git`), so it runs on a snapshot and
/// never under a map shard lock.
/// What: for each `Stale` verdict, writes `Stale` only if the record is still
/// `Running` and has not been heard from since the snapshot — a hook event or
/// a stop that landed during the probe wins. Returns how many it staled.
/// Test: `a_quiet_isolated_record_reads_stale_only_without_live_evidence`,
/// `a_quiet_staled_record_ends_like_a_running_one_on_its_stop`.
pub(crate) fn stale_quiet_delegations_at(
    state: &DaemonState,
    now: DateTime<Utc>,
    bound: Duration,
    probe: impl Fn(&Delegation) -> LiveEvidence,
) -> usize {
    let mut staled = 0;
    for d in state.all_delegations() {
        match quiet_verdict(&d, now, bound, &probe) {
            QuietVerdict::Stale => {
                let heard = last_heard(&d);
                let mut wrote = false;
                state.mutate_delegation(d.id, |r| {
                    if r.status == DelegationStatus::Running && last_heard(r) == heard {
                        r.status = DelegationStatus::Stale;
                        wrote = true;
                    }
                });
                if wrote {
                    staled += 1;
                    tracing::info!(
                        agent_id = d.agent_id.as_deref().unwrap_or("<unknown>"),
                        delegation = ?d.id,
                        "delegation: no hook event within the quiet bound and no live OS \
                         evidence of the agent — marked stale (#6556)"
                    );
                }
            }
            QuietVerdict::Running(why) => tracing::debug!(
                delegation = ?d.id,
                "delegation: quiet past the bound but kept running — {why} (#6556)"
            ),
            QuietVerdict::NotQuiet | QuietVerdict::Undeterminable => {}
        }
    }
    staled
}

/// The reap-loop entry: [`stale_quiet_delegations_at`] with the real probe.
///
/// Why: the probe blocks on child processes, so it runs on the blocking pool
/// rather than stalling the async runtime's reap tick.
/// What: returns how many records it staled; a panicked or cancelled blocking
/// task is logged at WARN and counts as none.
/// Test: the policy is covered through [`stale_quiet_delegations_at`].
pub(crate) async fn stale_quiet_delegations(state: &Arc<DaemonState>, bound: Duration) -> usize {
    let held = Arc::clone(state);
    let task = tokio::task::spawn_blocking(move || {
        stale_quiet_delegations_at(&held, Utc::now(), bound, probe_live_evidence)
    });
    match task.await {
        Ok(staled) => staled,
        Err(e) => {
            tracing::warn!("delegation: the quiet-bound pass did not finish ({e}) (#6556)");
            0
        }
    }
}

#[cfg(test)]
#[path = "delegation_quiet_stale_tests.rs"]
mod tests;
