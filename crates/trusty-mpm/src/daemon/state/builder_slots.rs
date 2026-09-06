//! Machine-wide builder-slot leases, claimed and released on [`DaemonState`]
//! (#6892).
//!
//! Why: the daemon is the only process on the machine that sees every session's
//! delegations, so it is the only one that can answer "how many builders are
//! running HERE" — the question a per-session rule structurally cannot ask. It
//! is also the only place the answer and the claim can be made indivisible,
//! which is what stops two dispatches issued in one PM turn from both seeing a
//! free slot and both taking it. Same shape as
//! [`DaemonState::claim_shared_tree_dispatch`](crate::daemon::state::DaemonState::claim_shared_tree_dispatch),
//! and for the same reason (#5324).
//!
//! What the lease IS: the delegation record the tracker would have written
//! anyway. There is no second kind of state and no separate expiry to clean up
//! — a builder holds a slot for exactly as long as its delegation is live, and
//! [`builder_lease`] is the predicate that decides when that stops being true.
//!
//! **A DENIED dispatch releases too, and that release is this module's own
//! (#6892 critic round).** The three signals below all describe an agent that
//! ran. A dispatch the cap refuses never runs at all, and yet the guard's
//! preceding shared-tree or worktree-grant claim has already recorded it as
//! `Running` — those claim BY recording. Nothing downstream would ever close
//! that record, so [`DaemonState::release_denied_builder_dispatch`] closes it
//! inside the same critical section as the refusal.
//!
//! **Three independent releases, whichever fires first.** A `SubagentStop` or
//! the staleness sweep moves the record out of
//! [`DelegationStatus::is_live`](crate::core::agent::DelegationStatus::is_live);
//! the dispatching session's PID being confirmed dead releases it without
//! waiting for any signal from the agent; and
//! [`BUILDER_LEASE_TTL_SECS`] releases it regardless of both. All three exist
//! because each covers a hole the others leave — see each variant of
//! [`BuilderLease`].
//! Test: the `#[cfg(test)]` suite below.

use serde::{Deserialize, Serialize};

use crate::core::agent::Delegation;
use crate::core::dispatch_isolation::agent_is_builder;
use crate::core::session::SessionId;

use super::core::DaemonState;

/// How long a builder may hold a slot before the lease is released regardless
/// of every other signal.
///
/// Why 45 minutes, and why not
/// [`RUNNING_STALE_AFTER_SECS`](super::sessions::RUNNING_STALE_AFTER_SECS):
/// that constant is six hours and is calibrated for a VANISHED SUBAGENT — an
/// agent that may legitimately still be running a long CI wait. This clock
/// measures something else, a build. A `cargo` run that has held one of a
/// handful of machine-wide slots for three quarters of an hour is either
/// finished and unreported or wedged, and in both cases holding the slot for
/// another five hours starves every other session on the machine of the thing
/// this cap exists to ration. 45 minutes sits above the longest ordinary
/// workspace build observed here (~17 minutes for a full CI leg) with room to
/// spare, and well below the six-hour window.
///
/// Releasing early is survivable in a way holding late is not: the worst case is
/// an admitted (N+1)th builder on a machine sized for N, which is the state
/// every machine was in before this cap existed. The worst case of holding is a
/// machine that admits nothing.
/// What: seconds, measured from `started_at` (falling back to `created_at`).
/// Test: `a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive`,
/// `a_lease_inside_the_ttl_is_still_held`.
pub const BUILDER_LEASE_TTL_SECS: i64 = 45 * 60;

/// One builder currently holding a slot, as the deny message names it.
///
/// Why: a deny that cannot say WHICH builders are running reads as arbitrary and
/// gets retried identically. Every field here is already on the
/// [`Delegation`] record — no new state, and nothing to keep in sync.
/// What: the agent name, the session that dispatched it, and how long it has
/// been running. `elapsed_secs` is what makes a wedged holder visible: a lease
/// at 44 minutes tells the reader something a name alone does not.
/// Test: `builder_holders_report_the_agent_session_and_elapsed_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuilderHolder {
    /// The builder agent's name.
    pub agent: String,
    /// The session that dispatched it.
    pub session: SessionId,
    /// How long it has been running, in seconds.
    pub elapsed_secs: i64,
}

/// Whether one builder-class delegation still holds its slot, and if not, why.
///
/// Why: the three releases must be distinguishable, not merely summed to a
/// boolean. `tm doctor` warns on exactly one of them —
/// [`Self::ReleasedByTtl`] — because a lease that only the TTL could end is a
/// lease whose owner never reported and whose PID could not be checked, which
/// is a signal about the harness rather than about the build.
/// What: [`Self::Held`] carries the elapsed seconds the holder is reported
/// with. The three released variants are ordered by the strength of the
/// evidence behind them, and [`builder_lease`] returns the first that applies.
/// Test: `a_terminal_delegation_holds_nothing`,
/// `a_lease_is_released_when_the_owner_pid_is_dead`,
/// `a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive`,
/// `a_lease_inside_the_ttl_is_still_held`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderLease {
    /// The builder is running and the slot is taken. Carries elapsed seconds.
    Held(i64),
    /// The delegation reached a terminal or stale status — the ordinary end.
    ReleasedByStatus,
    /// The dispatching session's process is confirmed gone. This is the fast
    /// path: a killed PM takes its agents with it and none of them emits a
    /// `SubagentStop`, so without this the slots stay held until the TTL.
    ReleasedByDeadOwner,
    /// Neither of the above could answer and the lease outlived
    /// [`BUILDER_LEASE_TTL_SECS`]. This is the backstop, and the one `tm
    /// doctor` reports: reaching it means nothing else was able to end the
    /// lease.
    ReleasedByTtl,
}

impl BuilderLease {
    /// Is the slot still taken?
    ///
    /// Test: `a_lease_inside_the_ttl_is_still_held`.
    #[must_use]
    pub fn is_held(self) -> bool {
        matches!(self, Self::Held(_))
    }
}

/// Classify one delegation's builder lease.
///
/// Why: pure — it takes the owner's liveness as an argument rather than probing
/// for it — so all four outcomes are assertable without killing a process or
/// waiting 45 minutes, and the I/O lives in
/// [`DaemonState::builder_slot_holders`] next door.
/// What: in order — a non-live status releases; a `Some(false)` owner releases;
/// an elapsed time at or past [`BUILDER_LEASE_TTL_SECS`] releases; anything else
/// is [`BuilderLease::Held`] with that elapsed time. `owner_alive` is `None`
/// when the owning session records no PID, which is INCONCLUSIVE rather than
/// dead: a session the daemon never learned a PID for must not have its builds
/// released on a guess, so it falls through to the TTL. Elapsed is measured from
/// `started_at`, falling back to `created_at` for a record that never learned
/// one, and is clamped at zero so a clock skew cannot manufacture a lease that
/// is instantly past its TTL.
/// Test: `a_terminal_delegation_holds_nothing`,
/// `a_lease_is_released_when_the_owner_pid_is_dead`,
/// `a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive`,
/// `a_lease_inside_the_ttl_is_still_held`,
/// `an_unknown_owner_pid_does_not_release_the_lease`.
#[must_use]
pub fn builder_lease(
    delegation: &Delegation,
    owner_alive: Option<bool>,
    now: chrono::DateTime<chrono::Utc>,
) -> BuilderLease {
    if !delegation.status.is_live() {
        return BuilderLease::ReleasedByStatus;
    }
    if owner_alive == Some(false) {
        return BuilderLease::ReleasedByDeadOwner;
    }
    let started = delegation.started_at.unwrap_or(delegation.created_at);
    let elapsed = (now - started).num_seconds().max(0);
    if elapsed >= BUILDER_LEASE_TTL_SECS {
        return BuilderLease::ReleasedByTtl;
    }
    BuilderLease::Held(elapsed)
}

/// What the daemon knows about this machine's builder slots right now.
///
/// Why: `tm doctor` needs two lists, not one — who is holding, and which leases
/// only the TTL could have ended. Folding them into a single count would lose
/// exactly the signal the Warn row exists to surface.
/// What: `holders` are the live leases the cap is decided from; `expired` are
/// builder delegations whose records still look live but whose lease has passed
/// [`BUILDER_LEASE_TTL_SECS`]. `cap` is the machine's effective cap at the
/// moment of the read.
/// Test: `census_separates_holders_from_expired_leases`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderSlotCensus {
    /// Builders currently holding a slot.
    pub holders: Vec<BuilderHolder>,
    /// Leases past the TTL that no other signal has ended.
    pub expired: Vec<BuilderHolder>,
    /// The machine's effective `builders.max_concurrent`.
    pub cap: u32,
}

impl DaemonState {
    /// Every builder currently holding one of this machine's slots.
    ///
    /// Why: deliberately NOT scoped to a session or a directory. The cap is a
    /// property of the machine, so the population is every session's
    /// delegations — the same widening ADR-0048 made for the shared-tree guard
    /// and for the same reason, one step further: there the key was the
    /// directory, here there is no key at all.
    /// What: scans the delegation map for builder-class agents
    /// ([`agent_is_builder`]) whose [`builder_lease`] is
    /// [`BuilderLease::Held`], resolving each owner's liveness from its
    /// session's recorded PID. `exclude_tool_use_id` drops the caller's own
    /// in-flight dispatch: the daemon's `matcher: "*"` hook and the guard's POST
    /// race on the SAME dispatch, and without the exclusion the very first
    /// builder on an idle machine could find itself and be denied.
    /// Test: `builder_holders_report_the_agent_session_and_elapsed_time`,
    /// `a_builder_claim_excludes_the_callers_own_dispatch`,
    /// `non_builder_delegations_hold_no_slot`.
    #[must_use]
    pub fn builder_slot_holders(&self, exclude_tool_use_id: Option<&str>) -> Vec<BuilderHolder> {
        self.builder_leases(exclude_tool_use_id, BuilderLease::is_held)
    }

    /// This machine's builder-slot census, for `tm doctor` (#6892).
    ///
    /// Why: one read answers both of doctor's questions, so the row can never
    /// describe a holder set and an expiry set sampled a moment apart.
    /// What: [`Self::builder_slot_holders`] plus every lease
    /// [`BuilderLease::ReleasedByTtl`] left it, under `cap`. Read-only — it
    /// reaps nothing, because a doctor row that mutated state would make the
    /// diagnosis change the thing diagnosed.
    /// Test: `census_separates_holders_from_expired_leases`.
    #[must_use]
    pub fn builder_slot_census(&self, cap: u32) -> BuilderSlotCensus {
        BuilderSlotCensus {
            holders: self.builder_slot_holders(None),
            expired: self.builder_leases(None, |lease| lease == BuilderLease::ReleasedByTtl),
            cap,
        }
    }

    /// Answer "who holds a builder slot" and claim one, in one step (#6892).
    ///
    /// Why: asking and acting are two steps, and two dispatches issued in ONE PM
    /// turn — the framework's own documented pattern for parallel work — can
    /// both ask before either is recorded, both see a free slot, and both be
    /// admitted. That is the whole failure this cap exists to prevent, so the
    /// answer and the claim are one operation. `delegations` is a `DashMap`: it
    /// makes each entry atomic, never a scan-then-insert pair.
    ///
    /// What: under
    /// [`builder_claim`](DaemonState::builder_claim_guard)'s mutex, computes
    /// the holder list and, when `eligible` says this dispatch is itself a
    /// builder AND the holders are strictly under `cap`, runs `record` before
    /// releasing. Returns the holders the caller decides on plus whether the
    /// slot was claimed. A second caller arriving concurrently blocks until the
    /// first has recorded, so it sees the fuller list — exactly `cap` dispatches
    /// are admitted however many arrive at once.
    ///
    /// `record` is a closure rather than an inlined write so this method keeps
    /// no opinion about what a delegation record looks like: its only caller
    /// passes the delegation tracker's own `PreToolUse` observer, so the claim
    /// IS the record that tracker would have written milliseconds later, with
    /// the same lifecycle and the same staleness sweep.
    ///
    /// **A refused claim runs `release` instead, and that is not symmetry for
    /// its own sake (#6892 critic round).** By the time this is asked, the
    /// guard's preceding shared-tree or worktree-grant call has ALREADY recorded
    /// a `Running` delegation for this dispatch — both of those claim by
    /// recording on an empty answer. Denying here means the tool never runs, so
    /// no `SubagentStop` will ever close that record and no process will die: it
    /// stays live for the six hours of
    /// [`RUNNING_STALE_AFTER_SECS`](super::sessions::RUNNING_STALE_AFTER_SECS),
    /// occupying both the checkout (so #4480 refuses the re-issue this cap's own
    /// deny message recommends) and a builder slot (so the machine is one
    /// permanently short). Releasing it inside this same critical section is
    /// what makes the deny leave no trace.
    ///
    /// Neither closure may take THIS lock again — it is not reentrant — and
    /// neither may await. Both MAY take
    /// [`dispatch_record_guard`](DaemonState::dispatch_record_guard), and both
    /// passed today do: that is the documented lock order, matching the
    /// shared-tree claim's. This mutex is never taken inside that one, so the
    /// two orders cannot cross.
    /// Test: `builder_cap_admits_up_to_the_cap_and_denies_the_rest`,
    /// `builder_cap_admits_exactly_one_of_two_simultaneous_claims`,
    /// `a_refused_builder_claim_records_nothing`,
    /// `a_non_builder_dispatch_claims_no_slot`,
    /// `a_denied_builder_releases_the_record_the_dispatch_just_claimed`.
    pub fn claim_builder_slot<C: FnOnce(&Self), R: FnOnce(&Self)>(
        &self,
        cap: u32,
        exclude_tool_use_id: Option<&str>,
        eligible: bool,
        record: C,
        release: R,
    ) -> (Vec<BuilderHolder>, bool) {
        let _claim = self.builder_claim_guard();
        let holders = self.builder_slot_holders(exclude_tool_use_id);
        let claimed = eligible && u32::try_from(holders.len()).unwrap_or(u32::MAX) < cap;
        if claimed {
            record(self);
        } else if eligible {
            // #6892: only an ELIGIBLE refusal is a deny. An ineligible payload
            // was never going to be denied by this guard, so nothing it may have
            // recorded is this rule's to undo.
            release(self);
        }
        (holders, claimed)
    }

    /// Close out the delegation record a denied builder dispatch just created
    /// (#6892 critic round).
    ///
    /// Why: see [`Self::claim_builder_slot`]. The record is written by the
    /// shared-tree claim or the worktree grant that runs immediately before the
    /// cap is asked, and a `PreToolUse` deny means nothing downstream will ever
    /// close it.
    /// What: marks the delegation carrying `tool_use_id`
    /// [`DelegationStatus::Cancelled`](crate::core::agent::DelegationStatus::Cancelled)
    /// and stamps `ended_at`, under the dispatch-record lock so it cannot race
    /// the tracker's own writer.
    ///
    /// It RETAINS the record rather than removing it, deliberately. The
    /// tracker's `matcher: "*"` hook fires on the same dispatch and may land
    /// after this, and `delegation_tracker::on_dispatch_locked` returns early on
    /// a `tool_use_id` it already knows — whatever its status. Keeping a
    /// cancelled record is therefore what stops that hook from re-creating a
    /// live one; deleting it would leave the hole open. `Cancelled`, not
    /// `Completed`: nothing ran.
    /// Returns `false` when there is no such record — the ordinary case when the
    /// guard's own preceding claim declined to record.
    /// Test: `a_denied_builder_releases_the_record_the_dispatch_just_claimed`,
    /// `a_denied_builder_releases_only_its_own_record`,
    /// `releasing_an_unknown_dispatch_is_a_no_op`.
    pub fn release_denied_builder_dispatch(
        &self,
        session: SessionId,
        tool_use_id: Option<&str>,
    ) -> bool {
        let Some(tool_use_id) = tool_use_id else {
            return false;
        };
        let _record = self.dispatch_record_guard();
        let Some(id) =
            self.find_delegation(session, |d| d.tool_use_id.as_deref() == Some(tool_use_id))
        else {
            return false;
        };
        self.terminate_delegation(id, crate::core::agent::DelegationStatus::Cancelled)
    }

    /// The one scan every builder-slot query runs.
    ///
    /// Why: two copies of this filter would drift, and a drift here is a cap
    /// that counts a finished build or misses a running one. `keep` is the only
    /// axis the callers disagree on.
    /// Test: as the two public wrappers.
    fn builder_leases(
        &self,
        exclude_tool_use_id: Option<&str>,
        keep: impl Fn(BuilderLease) -> bool,
    ) -> Vec<BuilderHolder> {
        let now = chrono::Utc::now();
        let mut rows: Vec<BuilderHolder> = self
            .delegations
            .iter()
            .filter_map(|entry| {
                let d = entry.value();
                if !agent_is_builder(&d.agent) {
                    return None;
                }
                if exclude_tool_use_id.is_some() && d.tool_use_id.as_deref() == exclude_tool_use_id
                {
                    return None;
                }
                let lease = builder_lease(d, self.session_owner_alive(d.session), now);
                if !keep(lease) {
                    return None;
                }
                let started = d.started_at.unwrap_or(d.created_at);
                Some(BuilderHolder {
                    agent: d.agent.clone(),
                    session: d.session,
                    elapsed_secs: (now - started).num_seconds().max(0),
                })
            })
            .collect();
        // Stable output so a deny message and a doctor row read the same way
        // twice in a row; a `DashMap` scan has no inherent order.
        rows.sort_by(|a, b| {
            b.elapsed_secs
                .cmp(&a.elapsed_secs)
                .then_with(|| a.agent.cmp(&b.agent))
        });
        rows
    }

    /// Is the process that dispatched this delegation still running?
    ///
    /// Why: a killed PM takes every subagent it dispatched with it, and none of
    /// them emits a `SubagentStop` — so without a PID check those slots stay
    /// held for the full TTL on a machine that is doing nothing. This is the
    /// same evidence the #6497 dead-session reaper acts on, read here rather
    /// than waited for, because the reaper runs on the housekeeping loop and a
    /// dispatch cannot wait for a loop tick.
    /// What: `Some(false)` only when the session records a PID and
    /// [`is_process_alive`](crate::core::process::is_process_alive) says it is
    /// gone. `None` — not `Some(true)` — when the session is unknown or records
    /// no PID: absence of a PID is undeterminable, not evidence of death
    /// (ADR-0045), and treating it as either would be a guess in a place where
    /// guessing wrong releases a running build's slot.
    /// Test: `a_lease_is_released_when_the_owner_pid_is_dead`,
    /// `an_unknown_owner_pid_does_not_release_the_lease`.
    fn session_owner_alive(&self, session: SessionId) -> Option<bool> {
        let pid = self.session(session)?.pid?;
        Some(crate::core::process::is_process_alive(pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
    use crate::core::session::{ControlModel, Session, SessionStatus};

    /// A registered session whose PID field the caller sets.
    fn session_with_pid(state: &DaemonState, pid: Option<u32>) -> SessionId {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut s = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        s.tmux_name = format!("tmpm-builder-test-{n}");
        s.status = SessionStatus::Active;
        s.pid = pid;
        let id = s.id;
        state.register_session(s);
        id
    }

    /// One running delegation of `agent`, started `age_secs` ago.
    fn running(session: SessionId, agent: &str, age_secs: i64) -> Delegation {
        let mut d = Delegation::new(session, None, agent, ModelTier::Sonnet, "build it");
        d.status = DelegationStatus::Running;
        d.started_at = Some(chrono::Utc::now() - chrono::Duration::seconds(age_secs));
        d
    }

    // ---- the pure lease predicate ---------------------------------------

    #[test]
    fn a_terminal_delegation_holds_nothing() {
        let mut d = running(SessionId::new(), "rust-engineer", 10);
        for status in [
            DelegationStatus::Completed,
            DelegationStatus::Failed,
            DelegationStatus::Cancelled,
            DelegationStatus::Stale,
        ] {
            d.status = status;
            assert_eq!(
                builder_lease(&d, Some(true), chrono::Utc::now()),
                BuilderLease::ReleasedByStatus,
                "{status:?} must release the slot"
            );
        }
    }

    /// Criterion 3, as the predicate sees it. A TTL-only implementation returns
    /// `Held` here and fails this case.
    #[test]
    fn a_lease_is_released_when_the_owner_pid_is_dead() {
        let d = running(SessionId::new(), "rust-engineer", 30);
        assert_eq!(
            builder_lease(&d, Some(false), chrono::Utc::now()),
            BuilderLease::ReleasedByDeadOwner,
            "a dead dispatcher releases its builders' slots long before the TTL"
        );
    }

    /// Criterion 4. The PID check answers nothing and the status never moves —
    /// the TTL is the only thing left, and it must still fire.
    #[test]
    fn a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive() {
        let d = running(
            SessionId::new(),
            "rust-engineer",
            BUILDER_LEASE_TTL_SECS + 1,
        );
        assert_eq!(
            builder_lease(&d, None, chrono::Utc::now()),
            BuilderLease::ReleasedByTtl
        );
        // And exactly at the boundary, not one second later.
        let d = running(SessionId::new(), "rust-engineer", BUILDER_LEASE_TTL_SECS);
        assert_eq!(
            builder_lease(&d, None, chrono::Utc::now()),
            BuilderLease::ReleasedByTtl
        );
    }

    #[test]
    fn a_lease_inside_the_ttl_is_still_held() {
        let d = running(
            SessionId::new(),
            "rust-engineer",
            BUILDER_LEASE_TTL_SECS - 60,
        );
        let lease = builder_lease(&d, Some(true), chrono::Utc::now());
        assert!(lease.is_held(), "{lease:?}");
        assert!(matches!(lease, BuilderLease::Held(e) if e >= BUILDER_LEASE_TTL_SECS - 61));
    }

    #[test]
    fn an_unknown_owner_pid_does_not_release_the_lease() {
        // Absence of a PID is undeterminable, not death — releasing on it would
        // free a running build's slot on no evidence at all.
        let d = running(SessionId::new(), "rust-engineer", 30);
        assert!(builder_lease(&d, None, chrono::Utc::now()).is_held());
    }

    // ---- the state-level scan and claim ---------------------------------

    #[test]
    fn builder_holders_report_the_agent_session_and_elapsed_time() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "rust-engineer", 120));

        let holders = state.builder_slot_holders(None);
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].agent, "rust-engineer");
        assert_eq!(holders[0].session, session);
        assert!(holders[0].elapsed_secs >= 120, "{:?}", holders[0]);
    }

    /// Criterion 7 at the counting layer: a non-builder dispatch is not merely
    /// admitted, it is INVISIBLE — it never appears as a holder, so it can never
    /// deny anyone else either.
    #[test]
    fn non_builder_delegations_hold_no_slot() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        for agent in [
            "research",
            "ticketing",
            "qa",
            "documentation",
            "version-control",
        ] {
            state.upsert_delegation(running(session, agent, 60));
        }
        assert!(state.builder_slot_holders(None).is_empty());
    }

    /// Criterion 8 at the counting layer: `local-ops` declares `role: ops` and
    /// must still occupy a slot.
    #[test]
    fn a_local_ops_delegation_holds_a_builder_slot() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "local-ops", 5));
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    /// Criterion 3 end to end through the real PID probe. `u32::MAX` is above
    /// every real PID, so the owner is provably gone without killing anything.
    #[test]
    fn a_dead_sessions_builders_stop_holding_slots() {
        let state = DaemonState::new();
        let dead = session_with_pid(&state, Some(u32::MAX));
        let alive = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(dead, "rust-engineer", 30));
        state.upsert_delegation(running(alive, "python-engineer", 30));

        let holders = state.builder_slot_holders(None);
        assert_eq!(holders.len(), 1, "{holders:?}");
        assert_eq!(holders[0].agent, "python-engineer");
    }

    /// Criterion 1. Two sessions, cap 2, four engineer dispatches: the first two
    /// are admitted whichever session they came from, and the third and fourth
    /// are refused with both holders named.
    #[test]
    fn builder_cap_admits_up_to_the_cap_and_denies_the_rest() {
        let state = DaemonState::new();
        let a = session_with_pid(&state, Some(std::process::id()));
        let b = session_with_pid(&state, Some(std::process::id()));

        let mut admitted = 0;
        let mut last_holders = Vec::new();
        for (session, agent) in [
            (a, "rust-engineer"),
            (b, "python-engineer"),
            (a, "react-engineer"),
            (b, "local-ops"),
        ] {
            let (holders, claimed) = state.claim_builder_slot(
                2,
                None,
                true,
                |s| s.upsert_delegation(running(session, agent, 1)),
                |_| {},
            );
            last_holders = holders;
            if claimed {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 2, "the cap is machine-wide, not per session");
        assert_eq!(
            last_holders.len(),
            2,
            "the deny must name both actual holders: {last_holders:?}"
        );
        let names: Vec<&str> = last_holders.iter().map(|h| h.agent.as_str()).collect();
        assert!(names.contains(&"rust-engineer"), "{names:?}");
        assert!(names.contains(&"python-engineer"), "{names:?}");
    }

    /// Criterion 2. One free slot, two claims arriving at once — the claim is
    /// atomic, so exactly one is admitted. A check-then-decide implementation
    /// admits both here.
    #[test]
    fn builder_cap_admits_exactly_one_of_two_simultaneous_claims() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = Arc::new(DaemonState::new());
        let session = session_with_pid(&state, Some(std::process::id()));
        let admitted = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let handles: Vec<_> = ["rust-engineer", "python-engineer"]
            .into_iter()
            .map(|agent| {
                let state = Arc::clone(&state);
                let admitted = Arc::clone(&admitted);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let (_, claimed) = state.claim_builder_slot(
                        1,
                        None,
                        true,
                        |s| s.upsert_delegation(running(session, agent, 0)),
                        |_| {},
                    );
                    if claimed {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread");
        }
        assert_eq!(
            admitted.load(Ordering::Relaxed),
            1,
            "one free slot must admit exactly one of two simultaneous dispatches"
        );
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    #[test]
    fn a_refused_builder_claim_records_nothing() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "rust-engineer", 10));

        let mut recorded = false;
        let mut released = false;
        let (holders, claimed) =
            state.claim_builder_slot(1, None, true, |_| recorded = true, |_| released = true);
        assert_eq!(holders.len(), 1, "the deny must name the holder");
        assert!(!claimed);
        assert!(
            !recorded,
            "nothing may be written when the claim is refused"
        );
        // #6892 critic round: and the record the preceding claim wrote is undone.
        assert!(released, "a refused eligible claim must release");
    }

    /// Criterion 7 at the claim layer. `eligible = false` is the daemon's own
    /// re-derivation for a non-builder dispatch: no slot is taken even on an
    /// idle machine.
    #[test]
    fn a_non_builder_dispatch_claims_no_slot() {
        let state = DaemonState::new();
        let mut recorded = false;
        let mut released = false;
        let (holders, claimed) =
            state.claim_builder_slot(4, None, false, |_| recorded = true, |_| released = true);
        assert!(holders.is_empty());
        assert!(!claimed);
        assert!(!recorded);
        // An INELIGIBLE dispatch was never going to be denied by this guard, so
        // nothing it may have recorded is this rule's to undo.
        assert!(!released, "an ineligible payload must not be released");
    }

    #[test]
    fn a_builder_claim_excludes_the_callers_own_dispatch() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        // What the daemon's own `matcher: "*"` hook writes when it wins the race
        // with the guard's POST for the same dispatch.
        let mut mine = running(session, "rust-engineer", 0);
        mine.tool_use_id = Some("toolu_MINE".to_string());
        state.upsert_delegation(mine);

        let (holders, claimed) =
            state.claim_builder_slot(1, Some("toolu_MINE"), true, |_| {}, |_| {});
        assert!(
            holders.is_empty() && claimed,
            "a dispatch must never be denied by its own record: {holders:?}"
        );
    }

    /// The release is keyed by `tool_use_id`, so a payload without one — or a
    /// dispatch whose preceding claim recorded nothing — is a no-op rather than
    /// a scan that guesses at which record to close.
    #[test]
    fn releasing_an_unknown_dispatch_is_a_no_op() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "rust-engineer", 10));

        assert!(!state.release_denied_builder_dispatch(session, None));
        assert!(!state.release_denied_builder_dispatch(session, Some("toolu_NOT_HERE")));
        assert_eq!(
            state.builder_slot_holders(None).len(),
            1,
            "a no-op release must not touch the live holder"
        );
    }

    #[test]
    fn census_separates_holders_from_expired_leases() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, None);
        state.upsert_delegation(running(session, "rust-engineer", 60));
        state.upsert_delegation(running(
            session,
            "python-engineer",
            BUILDER_LEASE_TTL_SECS + 60,
        ));

        let census = state.builder_slot_census(3);
        assert_eq!(census.cap, 3);
        assert_eq!(census.holders.len(), 1);
        assert_eq!(census.holders[0].agent, "rust-engineer");
        assert_eq!(census.expired.len(), 1);
        assert_eq!(census.expired[0].agent, "python-engineer");
    }
}
