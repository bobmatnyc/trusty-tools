//! Stops that arrived before the event naming their agent (#4142).
//!
//! Why: `PostToolUse` is installed `async: true` and `SubagentStop`
//! synchronously (`core::standalone::hooks`), so each runs as its own `tm hook`
//! process and their arrival order at the daemon is a race. `on_subagent_stop`
//! resolves a delegation only by `agent_id`, and only `on_launched` teaches one,
//! so a stop that wins that race matched nothing, terminalized nothing, and was
//! discarded — leaving the delegation `Running` until the 6 h staleness sweep
//! reaped it as a phantom in-flight agent.
//!
//! What: a bounded, TTL'd ledger of `(session, agent_id) -> terminal status`.
//! `on_subagent_stop` records here instead of discarding; `on_launched` consults
//! it the moment it learns an `agent_id` and applies the status the stop already
//! determined. Both arrival orders therefore converge on the same record.
//!
//! # This buffers evidence — it never manufactures any
//!
//! The key stored is the exact `agent_id` the stop quoted, so a later lookup
//! either finds the same delegation the in-order path would have found or finds
//! nothing. There is no "most recent Running" guess anywhere in this file, and
//! adding one would reintroduce the false all-clear the whole delegation tracker
//! exists to prevent.
//!
//! # Every loss path degrades to the pre-#4142 behavior
//!
//! Both bounds drop entries: [`PENDING_STOP_CAP`] on pressure, and
//! [`PENDING_STOP_TTL_SECS`] on age. A dropped entry costs the recovery, so the
//! delegation stays `Running` and the staleness sweep reaps it exactly as it did
//! before this module existed. Nothing here can terminalize a delegation no stop
//! named, which is why bounding the buffer is safe to do bluntly.
//! Test: the `#[cfg(test)]` suite below, plus the `#4142` arms of
//! `delegation_tracker_tests`.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use crate::core::agent::DelegationStatus;
use crate::core::session::SessionId;

/// Most unresolved stops held at once.
///
/// Why: the buffer must not become the leak it exists to close. An entry is
/// ~60 bytes and only a stop whose `PostToolUse` has not yet landed occupies
/// one, so the steady-state population is the number of subagents mid-race —
/// single digits. 256 is far above any real concurrency while still capping the
/// worst case (a daemon that never sees another `PostToolUse`) at a few tens of
/// kilobytes.
/// Test: `the_ledger_evicts_its_oldest_entry_at_capacity`.
pub(crate) const PENDING_STOP_CAP: usize = 256;

/// How long an unresolved stop stays useful.
///
/// Why: the window this bridges is one async hook's scheduling delay, which is
/// milliseconds — `PostToolUse` fires ~1 ms after launch and is merely
/// descheduled, never deferred by design. Fifteen minutes is orders of magnitude
/// beyond that, so it cannot expire a stop that was going to be matched, while
/// still evicting one whose `PostToolUse` was dropped outright (a failed hook
/// POST) rather than holding it forever.
/// Test: `an_expired_entry_is_pruned`.
pub(crate) const PENDING_STOP_TTL_SECS: i64 = 15 * 60;

/// One `SubagentStop` still waiting for its `agent_id` to be taught.
#[derive(Debug, Clone)]
struct PendingStop {
    session: SessionId,
    agent_id: String,
    /// The status the stop determined — `Failed` for `SubagentStopFailure`,
    /// else `Completed`. Stored rather than recomputed so the deferred path
    /// applies exactly what the in-order path would have.
    status: DelegationStatus,
    seen_at: DateTime<Utc>,
}

/// The bounded ledger itself.
///
/// Why: held by [`crate::daemon::state::DaemonState`] because it is daemon-wide
/// correlation state, and behind a `Mutex<VecDeque>` rather than a `DashMap`
/// because capacity eviction needs insertion order and the population is small
/// enough that a linear scan is cheaper than hashing. Neither of its two callers
/// is on the `PreToolUse` hot path.
/// What: insertion-ordered, oldest at the front, keyed by `(session, agent_id)`.
/// Test: the suite below.
#[derive(Debug, Default)]
pub struct PendingStops {
    entries: parking_lot::Mutex<VecDeque<PendingStop>>,
}

impl PendingStops {
    /// Remember a stop nothing could resolve yet.
    ///
    /// What: replaces any entry already holding this key — a redelivered stop
    /// restates the same fact and must not consume a second slot — otherwise
    /// appends, then evicts from the front until the ledger is within
    /// [`PENDING_STOP_CAP`].
    /// Test: `a_recorded_stop_is_visible`, `re_recording_a_key_replaces_it`,
    /// `the_ledger_evicts_its_oldest_entry_at_capacity`.
    pub(crate) fn record(
        &self,
        session: SessionId,
        agent_id: &str,
        status: DelegationStatus,
        now: DateTime<Utc>,
    ) {
        let mut entries = self.entries.lock();
        entries.retain(|e| !(e.session == session && e.agent_id == agent_id));
        entries.push_back(PendingStop {
            session,
            agent_id: agent_id.to_string(),
            status,
            seen_at: now,
        });
        while entries.len() > PENDING_STOP_CAP {
            if let Some(dropped) = entries.pop_front() {
                tracing::warn!(
                    agent_id = dropped.agent_id,
                    cap = PENDING_STOP_CAP,
                    "delegation: dropped the oldest unresolved SubagentStop to keep the \
                     ledger bounded (#4142); its delegation now relies on the staleness \
                     sweep, exactly as it did before recovery existed"
                );
            }
        }
    }

    /// The status a stop already determined for this agent, if one is waiting.
    ///
    /// Why this reads rather than takes: the caller clears the entry only once
    /// the terminalization it authorises has actually landed. Taking here would
    /// discard the stop on any path that then failed to write — the silent-loss
    /// shape this module exists to remove.
    /// Test: `a_recorded_stop_is_visible`,
    /// `an_unmatched_post_tool_use_does_not_consume_the_pending_stop`.
    pub(crate) fn peek(&self, session: SessionId, agent_id: &str) -> Option<DelegationStatus> {
        self.entries
            .lock()
            .iter()
            .find(|e| e.session == session && e.agent_id == agent_id)
            .map(|e| e.status)
    }

    /// Forget a stop whose delegation is now terminal.
    /// Test: `clearing_removes_only_the_named_key`.
    pub(crate) fn clear(&self, session: SessionId, agent_id: &str) {
        self.entries
            .lock()
            .retain(|e| !(e.session == session && e.agent_id == agent_id));
    }

    /// Drop every entry older than [`PENDING_STOP_TTL_SECS`], returning how many.
    ///
    /// Why: driven by the 60 s reap loop through
    /// [`DaemonState::sweep_delegations`](crate::daemon::state::DaemonState::sweep_delegations),
    /// never by a hook — the same placement, and for the same reason, as the
    /// delegation staleness sweep it rides along with.
    /// Test: `an_expired_entry_is_pruned`.
    pub(crate) fn prune_at(&self, now: DateTime<Utc>) -> usize {
        let ttl = chrono::Duration::seconds(PENDING_STOP_TTL_SECS);
        let mut entries = self.entries.lock();
        let before = entries.len();
        entries.retain(|e| now - e.seen_at <= ttl);
        before - entries.len()
    }

    /// How many stops are waiting.
    ///
    /// Why `cfg(test)`: production reads the ledger only through [`Self::peek`],
    /// which asks about one key. Depth is what the bound and the TTL assert, and
    /// nothing but a test needs it — exposing it unconditionally would be dead
    /// code claiming to be an API.
    /// Test: `the_ledger_evicts_its_oldest_entry_at_capacity`,
    /// `the_delegation_sweep_ages_the_deferred_stop_ledger`.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId::new()
    }

    #[test]
    fn a_recorded_stop_is_visible() {
        let led = PendingStops::default();
        let s = sid();
        led.record(s, "aid1", DelegationStatus::Completed, Utc::now());
        assert_eq!(led.peek(s, "aid1"), Some(DelegationStatus::Completed));
        assert_eq!(led.peek(s, "aid2"), None, "another agent must not match");
        assert_eq!(
            led.peek(sid(), "aid1"),
            None,
            "another session must not match"
        );
    }

    #[test]
    fn a_failure_stop_keeps_its_status() {
        let led = PendingStops::default();
        let s = sid();
        led.record(s, "aid1", DelegationStatus::Failed, Utc::now());
        assert_eq!(led.peek(s, "aid1"), Some(DelegationStatus::Failed));
    }

    #[test]
    fn re_recording_a_key_replaces_it() {
        let led = PendingStops::default();
        let s = sid();
        led.record(s, "aid1", DelegationStatus::Completed, Utc::now());
        led.record(s, "aid1", DelegationStatus::Failed, Utc::now());
        assert_eq!(led.len(), 1, "a redelivered stop must not consume a slot");
        assert_eq!(led.peek(s, "aid1"), Some(DelegationStatus::Failed));
    }

    #[test]
    fn clearing_removes_only_the_named_key() {
        let led = PendingStops::default();
        let s = sid();
        led.record(s, "aid1", DelegationStatus::Completed, Utc::now());
        led.record(s, "aid2", DelegationStatus::Completed, Utc::now());
        led.clear(s, "aid1");
        assert_eq!(led.peek(s, "aid1"), None);
        assert_eq!(led.peek(s, "aid2"), Some(DelegationStatus::Completed));
    }

    #[test]
    fn the_ledger_evicts_its_oldest_entry_at_capacity() {
        let led = PendingStops::default();
        let s = sid();
        let now = Utc::now();
        for i in 0..PENDING_STOP_CAP + 10 {
            led.record(s, &format!("aid{i}"), DelegationStatus::Completed, now);
        }
        assert_eq!(led.len(), PENDING_STOP_CAP, "the buffer must stay bounded");
        assert_eq!(led.peek(s, "aid0"), None, "the oldest goes first");
        let newest = format!("aid{}", PENDING_STOP_CAP + 9);
        assert_eq!(led.peek(s, &newest), Some(DelegationStatus::Completed));
    }

    #[test]
    fn an_expired_entry_is_pruned() {
        let led = PendingStops::default();
        let s = sid();
        let now = Utc::now();
        led.record(s, "old", DelegationStatus::Completed, now);
        led.record(
            s,
            "fresh",
            DelegationStatus::Completed,
            now + chrono::Duration::seconds(PENDING_STOP_TTL_SECS),
        );
        let dropped = led.prune_at(now + chrono::Duration::seconds(PENDING_STOP_TTL_SECS + 1));
        assert_eq!(dropped, 1);
        assert_eq!(led.peek(s, "old"), None);
        assert_eq!(led.peek(s, "fresh"), Some(DelegationStatus::Completed));
    }
}
