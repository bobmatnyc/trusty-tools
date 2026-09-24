//! Ending a delegation record that no signal can ever end (#7602).
//!
//! Why: a dispatched agent's record leaves `Running` only on a `SubagentStop`
//! the daemon actually receives. When the parent session dies without one — the
//! harness is killed, the `tm hook` POST loses its 2 s budget, the daemon
//! restarts mid-flight — the record stays non-terminal forever, and
//! [`crate::session_manager::worktree_reclaim::agent_ownership_blocks`] reads
//! non-terminal as `Live` and refuses the agent's worktree for good. #6497 and
//! #6556 closed the cases where the daemon SEES the owner die; this one is the
//! case where it never sees anything, and before this module no `tm` verb could
//! end such a record at all. The live specimen is
//! `.claude/worktrees/agent-af20cc838b2b30a55`, whose tree both
//! `tm session prune-worktrees --merged-prs` and `tm session adopt-worktree`
//! refused with "a delegation naming it has not ended".
//!
//! # Why the status written is terminal, and why `Stale` would not do
//!
//! [`Stale`](crate::core::agent::DelegationStatus::Stale) is deliberately
//! neither live nor terminal, and
//! the two consumers read that differently: the idle nudge treats it as not
//! live, but
//! [`crate::daemon::services::agent_worktree_reap::delegation_state_for_agent`]
//! — the reclaim gate's only informant — asks `is_terminal()` and so still
//! answers [`crate::session_manager::worktree_ownership::AgentDelegationState::Live`].
//! Staling therefore does not release the worktree, which is exactly why the
//! specimen survived #6497's session-end sweep. The repair writes
//! [`Cancelled`](crate::core::agent::DelegationStatus::Cancelled): an explicit
//! terminal state meaning "this
//! delegation was ended by an operator because nothing else could end it",
//! never `Completed`, which would claim the agent finished its work.
//!
//! # Fail direction
//!
//! Closed, in both of the two ways that matter. A record whose owning session is
//! still live is REFUSED whatever the caller passes — ending it would readmit a
//! second writer onto a HEAD a live agent holds, the ADR-0048 harm. A record
//! whose owner the registry has never heard of is UNDETERMINABLE, not absent
//! (ADR-0045): the delegation map is built empty at every boot, so silence after
//! a restart looks identical to "the session is gone". That arm refuses too, and
//! names `--force` as the operator's explicit assertion — which is the whole
//! point of a sanctioned verb rather than a timer that guesses.
//!
//! One residual remains, and it is bounded rather than closed. [`owner_liveness`]
//! keys on the exact [`SessionId`] recorded at dispatch, so a session whose
//! record was REMOVED (rather than marked `Stopped`) while its pane runs on
//! under a reissued id reads as `Unknown`, and `--force` would then cancel a
//! delegation whose agent may still be writing: `--force` trusts the daemon's
//! own session bookkeeping, not the OS and not git. Nothing is deleted on that
//! evidence, though — every destructive path downstream re-checks a live OS
//! process before removing anything (`agent_worktree_reap`'s gate 6,
//! `worktree_reclaim_sweep`, #7504) — so the worst outcome is a status flipped
//! early, not a live agent's tree removed.
//!
//! # #8257: reaching every record, and asking the OS before writing
//!
//! A record a stop matched by agent type never learns an `agent_id`, so it is
//! addressed by its delegation id ([`repair_delegation_by_id`]). A record older
//! than `RUNNING_STALE_AFTER_SECS` is evidence about the AGENT, so a live owner
//! session no longer refuses it ([`record_liveness`]). A type-matched stop is
//! not: it can name a still-running sibling (#6556), so that record still
//! needs its owner's word while the owner lives.
//! Every would-be write then passes
//! [`super::delegation_repair_probe::probe_live_evidence`], which refuses on a
//! live process in the agent's tree and on any probe that cannot answer.
//!
//! # #8257 owner ruling (2026-09-24)
//!
//! The one exception to "a live owner refuses": the OWNING session may clear
//! its own record. The caller is read from [`CALLER_SESSION_HEADER`], which
//! `tm` fills from the harness's `CLAUDE_CODE_SESSION_ID` — never from a CLI
//! argument or the request body. A caller that cannot be established refuses
//! ([`owner_attests`]); live evidence from the probe still refuses the owner;
//! every clear is logged and stamped on the record as a [`DelegationRepair`].
//! Test: `delegation_repair_tests`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::delegation_records::owner_label;
use super::delegation_repair_probe::{LiveEvidence, probe_live_evidence};
use crate::core::agent::{Delegation, DelegationId, DelegationRepair};
use crate::core::session::{SessionId, SessionStatus};
use crate::daemon::state::DaemonState;
use crate::daemon::state::sessions::RUNNING_STALE_AFTER_SECS;

/// What the daemon can say about the session that owns a stuck record.
///
/// Why: "the owner is gone" and "the registry cannot say" are the same empty
/// answer read two ways, and only the first is evidence — the same split
/// [`crate::session_manager::worktree_ownership::AgentDelegationState`] makes
/// one level up, for the same ADR-0045 reason.
/// What: `Live` — a session record exists, is Active, and its tracked process
/// (when one is tracked) is alive. `Gone` — a record exists and says otherwise.
/// `Unknown` — no record at all.
/// Test: `owner_liveness_reads_active_stopped_and_absent_7602`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerLiveness {
    /// The owning session is still running.
    Live,
    /// The daemon holds positive evidence the owner is gone.
    Gone,
    /// The daemon holds no record of the owner, which proves nothing.
    Unknown,
}

/// What one `tm repair delegation` call did, or refused to do.
///
/// Why: the caller's exit status turns on this, and a refusal has to carry the
/// gate's own words — a repair that reports "nothing to do" for a record it
/// declined to touch is the silence this verb exists to remove.
/// What: the four outcomes, tagged for the wire.
/// Test: `repair_ends_a_stuck_record_of_a_dead_owner_7602`,
/// `repair_refuses_while_the_owner_is_live_7602`,
/// `repair_refuses_an_undeterminable_owner_without_force_7602`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RepairOutcome {
    /// No delegation names this agent at all.
    NoRecord,
    /// Every delegation naming this agent is already terminal.
    AlreadyEnded {
        /// How many terminal records name the agent.
        records: usize,
    },
    /// A gate refused; nothing was written.
    Refused {
        /// The gate's own wording.
        reason: String,
    },
    /// Non-terminal records were ended.
    Ended {
        /// How many records this call terminalized.
        records: usize,
    },
}

/// The request body of the repair route.
///
/// Why: `force` is an operator assertion, so it travels explicitly rather than
/// being inferred from anything the daemon can see.
/// What: one flag, defaulting to the refusing behaviour.
/// Test: `repair_refuses_an_undeterminable_owner_without_force_7602`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct RepairDelegationRequest {
    /// End the record even when the owning session is undeterminable.
    #[serde(default)]
    pub force: bool,
}

/// The header carrying the calling harness session (#8257 owner ruling).
///
/// Why a header, not a body field: the request body is a public struct, and
/// the caller identity is transport metadata `tm` fills from the harness's
/// `CLAUDE_CODE_SESSION_ID` — never from a CLI argument.
///
/// Residual: the header is caller-asserted, so any local process that knows
/// the owner's UUID can send it. Owner ruling 2026-09-24: 1.7.3 keeps that
/// UUID out of every text and wire field a denied caller reads
/// ([`owner_label`]); authenticating the caller by its peer pid is deferred to
/// 1.7.4 (#8257).
pub const CALLER_SESSION_HEADER: &str = "x-tm-caller-session";

/// Who asked for the repair (#8257 owner ruling).
///
/// Why: the owning session may clear its own live record on its own word, so
/// the gate must know whether the caller IS that session — and "could not
/// tell" must refuse rather than read as "someone else".
/// What: an established session id, or the reason none could be established.
/// Test: `an_unestablished_caller_is_refused_on_a_live_record_8257`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairCaller {
    /// The calling harness session.
    Session(SessionId),
    /// No caller session could be established; the text says why.
    Unestablished(String),
}

impl RepairCaller {
    /// Read the caller from the request's [`CALLER_SESSION_HEADER`] value.
    ///
    /// Only a UUID establishes a caller, so an [`owner_label`] — a tmux name —
    /// cannot be replayed as one (#8257 owner ruling).
    pub fn from_request(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None => Self::Unestablished(
                "the request named no caller session — `tm` sends the harness's \
                 CLAUDE_CODE_SESSION_ID, which was unset in the calling process"
                    .to_string(),
            ),
            Some(s) => uuid::Uuid::parse_str(s)
                .map(|u| Self::Session(SessionId(u)))
                .unwrap_or_else(|_| {
                    Self::Unestablished(format!("the caller session `{s}` is not a session id"))
                }),
        }
    }

    fn session(&self) -> Option<SessionId> {
        match self {
            Self::Session(s) => Some(*s),
            Self::Unestablished(_) => None,
        }
    }
}

/// The audit basis written on a record its owning session cleared (#8257).
pub const OWNER_ATTESTED: &str = "owner-attested finished (#8257)";

/// The whole gate, as a pure function of the counts and the owner's liveness.
///
/// Why: every refusal arm is testable without a daemon, a session store, or a
/// hook payload — and the arms are the point of the verb, not the write.
/// What: nothing named → `NoRecord`; nothing non-terminal → `AlreadyEnded`; a
/// live owner → `Refused` unconditionally, `force` included; an unknown owner →
/// `Refused` unless `force`; otherwise `Ended`.
/// Test: `decide_refuses_a_live_owner_even_with_force_7602`,
/// `decide_needs_force_for_an_unknown_owner_7602`,
/// `decide_ends_a_gone_owners_records_7602`, `decide_reports_no_record`,
/// `decide_reports_already_ended`.
pub(crate) fn decide(
    open: usize,
    terminal: usize,
    owner: OwnerLiveness,
    force: bool,
) -> RepairOutcome {
    if open == 0 && terminal == 0 {
        return RepairOutcome::NoRecord;
    }
    if open == 0 {
        return RepairOutcome::AlreadyEnded { records: terminal };
    }
    match owner {
        // Never, at any force. A live owner's agent may still be writing, and
        // releasing its tree is the ADR-0048 harm this refusal exists for.
        // #8257: names the three ways the record becomes repairable.
        OwnerLiveness::Live => RepairOutcome::Refused {
            reason: "the session that dispatched this agent is still Active, no stop has arrived \
                     for the agent, and the record is younger than the 6 h stale threshold — so \
                     the agent may still be running, and ending its delegation would readmit a \
                     second writer onto a working tree it may still hold. It becomes repairable \
                     once that session stops, when that session clears it itself, or at 6 h \
                     (#7602, #8257)"
                .to_string(),
        },
        OwnerLiveness::Unknown if !force => RepairOutcome::Refused {
            reason: "the daemon holds no record of the session that dispatched this agent, so it \
                     cannot confirm the owner is gone — the delegation map is rebuilt empty at \
                     every daemon start, and silence after a restart is undeterminable, not \
                     absent (ADR-0045). Re-run with --force to assert the owner is gone (#7602)"
                .to_string(),
        },
        OwnerLiveness::Unknown | OwnerLiveness::Gone => RepairOutcome::Ended { records: open },
    }
}

/// What the daemon can say about the session owning `session`.
///
/// Why: the one reading of the session registry this verb makes, so the three
/// arms cannot drift from the `reap_against` rules they mirror.
/// What: absent → `Unknown`; `Stopped`, or a tracked pid that is not alive →
/// `Gone`; anything else → `Live`.
/// Test: `owner_liveness_reads_active_stopped_and_absent_7602`.
pub(crate) fn owner_liveness(state: &Arc<DaemonState>, session: SessionId) -> OwnerLiveness {
    let Some(record) = state.session(session) else {
        return OwnerLiveness::Unknown;
    };
    if record.status == SessionStatus::Stopped {
        return OwnerLiveness::Gone;
    }
    match record.pid {
        Some(pid) if !crate::core::process::is_process_alive(pid) => OwnerLiveness::Gone,
        _ => OwnerLiveness::Live,
    }
}

/// End every non-terminal delegation naming `agent_id`, or say why not (#7602).
///
/// Why: the operator-facing half — `tm repair delegation <agent-id>` calls the
/// route that calls this. It is the only sanctioned way a stuck record ends, and
/// it writes a terminal status precisely so the reclaim gate, which reads
/// `is_terminal()`, stops refusing the agent's worktree.
/// What: tallies the records naming the agent, resolves the strictest owner
/// liveness across their sessions (any live owner wins), runs [`decide`], and on
/// `Ended` writes [`Cancelled`](crate::core::agent::DelegationStatus::Cancelled)
/// to each non-terminal record.
/// Nothing is written on any other outcome.
/// Test: `repair_ends_a_stuck_record_of_a_dead_owner_7602`,
/// `repair_refuses_while_the_owner_is_live_7602`,
/// `repair_refuses_an_undeterminable_owner_without_force_7602`,
/// `repair_force_ends_a_record_whose_owner_is_unknown_7602`,
/// `repair_reports_no_record_for_an_unknown_agent_7602`.
pub fn repair_delegation(state: &Arc<DaemonState>, agent_id: &str, force: bool) -> RepairOutcome {
    repair_delegation_as(state, agent_id, force, &no_caller())
}

/// [`repair_delegation`] on behalf of `caller` (#8257 owner ruling).
///
/// What: the route's entry point — `caller` is what lets the owning session
/// clear its own live record.
/// Test: `the_owning_session_clears_its_own_live_record_8257`.
pub fn repair_delegation_as(
    state: &Arc<DaemonState>,
    agent_id: &str,
    force: bool,
    caller: &RepairCaller,
) -> RepairOutcome {
    repair_matching(
        state,
        |d| d.agent_id.as_deref() == Some(agent_id),
        force,
        caller,
        probe_live_evidence,
    )
}

/// End the one delegation `id` names, or say why not (#8257).
///
/// Why: a record a stop matched by agent type has no `agent_id`, so
/// [`repair_delegation`] could never name it — the unrepairable record #8257
/// reports. Its delegation id is always known; the dispatch deny prints it.
/// What: [`repair_matching`] over the single record with that id, on behalf of
/// `caller`.
/// Test: `repair_by_delegation_id_ends_a_record_with_no_agent_id_8257`,
/// `a_type_matched_record_of_a_live_session_needs_its_owner_8257`.
pub fn repair_delegation_by_id(
    state: &Arc<DaemonState>,
    id: DelegationId,
    force: bool,
    caller: &RepairCaller,
) -> RepairOutcome {
    repair_matching(state, |d| d.id == id, force, caller, probe_live_evidence)
}

/// The caller of a direct, in-process repair: none established.
fn no_caller() -> RepairCaller {
    RepairCaller::Unestablished("no caller session was supplied to this repair".to_string())
}

/// A record's owner liveness, as far as it still speaks for the AGENT (#8257).
///
/// Why: a record past `RUNNING_STALE_AFTER_SECS` is past the budget the sweep
/// already gives up at. A stop matched only by agent type is NOT evidence: it
/// can name a still-running sibling (#6556), and the record carries no agent id
/// or tree for the probe to check — so it stays on its owner's liveness, and a
/// live owner's record ends only on that owner's word.
/// What: `Gone` for a record at or past the threshold; otherwise `owner`
/// unchanged. The OS probe still runs before any write.
/// Test: `a_type_matched_record_of_a_live_session_needs_its_owner_8257`,
/// `repair_refuses_while_the_owner_is_live_7602`.
pub(crate) fn record_liveness(
    owner: OwnerLiveness,
    d: &Delegation,
    now: chrono::DateTime<chrono::Utc>,
) -> OwnerLiveness {
    let aged_out = super::delegation_records::record_age_secs(d, now) >= RUNNING_STALE_AFTER_SECS;
    if aged_out { OwnerLiveness::Gone } else { owner }
}

/// Resolve a record whose agent may still run: only its owner may end it
/// (#8257 owner ruling).
///
/// What: `Ok(())` when `caller` is the record's own session — the owner
/// attests the agent finished. Otherwise `Err` with the refusal: a different
/// session is told only the owner can clear it before the stop or the 6 h
/// mark; an unestablished caller is told why none could be established. The
/// owner is named by `owner`, an [`owner_label`] — never by its UUID, which
/// the caller could replay in [`CALLER_SESSION_HEADER`] (#8257 owner ruling).
/// Test: `the_owning_session_clears_its_own_live_record_8257`,
/// `a_non_owning_session_is_refused_on_a_live_record_8257`,
/// `an_unestablished_caller_is_refused_on_a_live_record_8257`,
/// `an_owner_refusal_never_names_the_owner_uuid_8257`.
fn owner_attests(d: &Delegation, caller: &RepairCaller, owner: &str) -> Result<(), String> {
    // #8257: a type-matched stop may belong to a sibling (#6556), so say so.
    let stop = if d.stale_by_agent_type {
        "the only stop that arrived was matched by agent type and may belong to a sibling"
    } else {
        "no stop has arrived"
    };
    let head = format!(
        "{} record {} is owned by {owner}, which is still Active; {stop} and the record is \
         under the 6 h stale threshold, so the agent may still be running",
        d.agent, d.id.0
    );
    match caller {
        RepairCaller::Session(s) if *s == d.session => Ok(()),
        RepairCaller::Session(s) => Err(format!(
            "{head}. Only the owning session can clear it before its agent's own stop arrives \
             or the record reaches 6 h; this repair came from session {} (#8257)",
            s.0
        )),
        RepairCaller::Unestablished(why) => Err(format!(
            "{head}. Only the owning session can clear it before its agent's own stop \
             arrives or the record reaches 6 h, and the calling session could not be \
             established: {why} (#8257)"
        )),
    }
}

/// The repair over every record `matches` selects, with the OS probe injected.
///
/// Why: the probe is the one part a test cannot drive through the real OS on
/// demand — a probe that FAILS in particular — so it is a parameter here and
/// the public entry points pass [`probe_live_evidence`].
/// What: tallies the matched records and resolves each one's
/// [`record_liveness`]. A still-`Live` record ends only when [`owner_attests`]
/// — otherwise the call refuses with its words. The rest resolve the strictest
/// liveness across them (`Unknown` over `Gone`) and run [`decide`]. On `Ended`
/// it probes each open
/// record and refuses — naming the record and the probe's words — on any
/// [`LiveEvidence`] that is not `Clear`, the owner included. Only then does it
/// write [`Cancelled`](crate::core::agent::DelegationStatus::Cancelled) and a
/// [`DelegationRepair`] naming the caller and the basis, and logs one WARN line
/// per cleared record. An owner-check refusal logs one WARN line carrying the
/// owner's UUID, which its refusal text omits (#8257 owner ruling). Nothing is
/// written or logged on any other outcome.
/// Test: `repair_refuses_while_a_live_process_holds_the_tree_8257`,
/// `repair_refuses_when_the_live_agent_probe_cannot_answer_8257`,
/// `the_owner_is_refused_while_the_agent_shows_live_8257`,
/// `the_owner_is_refused_while_a_harness_lock_names_its_agent_8257`,
/// `a_forced_clear_is_logged_8257`, `an_unknown_owner_outranks_a_gone_one_8257`,
/// `an_owner_refusal_logs_the_owner_uuid_8257`.
pub(crate) fn repair_matching(
    state: &Arc<DaemonState>,
    matches: impl Fn(&Delegation) -> bool,
    force: bool,
    caller: &RepairCaller,
    probe: impl Fn(&Delegation) -> LiveEvidence,
) -> RepairOutcome {
    let now = chrono::Utc::now();
    let mut open: Vec<(Delegation, &str)> = Vec::new();
    let mut terminal = 0usize;
    let mut owner = None;
    for d in state.all_delegations() {
        if !matches(&d) {
            continue;
        }
        if d.status.is_terminal() {
            terminal += 1;
            continue;
        }
        let (liveness, basis) = match record_liveness(owner_liveness(state, d.session), &d, now) {
            // #8257 owner ruling: the owning session's word ends its own record.
            OwnerLiveness::Live => {
                match owner_attests(&d, caller, &owner_label(state, d.session)) {
                    Ok(()) => (OwnerLiveness::Gone, OWNER_ATTESTED),
                    Err(reason) => {
                        // #8257 owner ruling: the refusal omits the owner's UUID;
                        // the operator reads it here.
                        tracing::warn!(
                            delegation_id = %d.id.0,
                            agent = %d.agent,
                            owner_session = %d.session.0,
                            caller = caller.session().map_or_else(|| "unestablished".to_string(), |s| s.0.to_string()),
                            "delegation: repair refused — only the owning session may clear a live record (#8257)"
                        );
                        return RepairOutcome::Refused { reason };
                    }
                }
            }
            OwnerLiveness::Gone => (
                OwnerLiveness::Gone,
                "operator-ended: owner gone, agent stopped, or past 6 h (#7602, #8257)",
            ),
            OwnerLiveness::Unknown => (
                OwnerLiveness::Unknown,
                "operator-forced: owner undeterminable (#7602)",
            ),
        };
        // The strictest answer across the records decides the whole call.
        // #8257: Unknown outranks Gone, so one undeterminable record needs --force.
        owner = Some(match (owner, liveness) {
            (Some(OwnerLiveness::Unknown), _) | (_, OwnerLiveness::Unknown) => {
                OwnerLiveness::Unknown
            }
            _ => OwnerLiveness::Gone,
        });
        open.push((d, basis));
    }

    let outcome = decide(
        open.len(),
        terminal,
        owner.unwrap_or(OwnerLiveness::Unknown),
        force,
    );
    if !matches!(outcome, RepairOutcome::Ended { .. }) {
        return outcome;
    }
    // #8257: the registry says the records may end; the OS must agree first.
    for (d, _) in &open {
        let reason = match probe(d) {
            LiveEvidence::Clear => continue,
            LiveEvidence::Held(why) => format!("a live process still holds its tree: {why}"),
            LiveEvidence::Undeterminable(why) => format!(
                "the live-agent probe could not answer, and an unanswered probe is not proof \
                 the agent is gone (ADR-0045): {why}"
            ),
        };
        return RepairOutcome::Refused {
            reason: format!(
                "{} record {} (agent id {}): {reason} (#8257)",
                d.agent,
                d.id.0,
                d.agent_id.as_deref().unwrap_or("none")
            ),
        };
    }
    // #7602: Cancelled, never Completed. #8257: the record says who ended it
    // and on what basis; a record a real stop ended meanwhile is left alone.
    let by_session = caller.session();
    for (d, basis) in &open {
        let mut cleared = false;
        state.mutate_delegation(d.id, |rec| {
            if rec.status.is_terminal() {
                return;
            }
            rec.status = crate::core::agent::DelegationStatus::Cancelled;
            rec.repair = Some(DelegationRepair {
                by_session,
                reason: (*basis).to_string(),
                at: now,
            });
            cleared = true;
        });
        // #8257 owner ruling: every clear — owner-attested or forced — is
        // logged, one line per record, naming the caller and the basis.
        if cleared {
            tracing::warn!(
                delegation_id = %d.id.0,
                agent = %d.agent,
                agent_id = d.agent_id.as_deref().unwrap_or("none"),
                owner_session = %d.session.0,
                by_session = by_session.map_or_else(|| "unestablished".to_string(), |s| s.0.to_string()),
                basis = *basis,
                forced = force,
                "delegation: repair cleared a record — status Cancelled (#7602, #8257)"
            );
        }
    }
    outcome
}

#[cfg(test)]
#[path = "delegation_repair_tests.rs"]
mod delegation_repair_tests;
