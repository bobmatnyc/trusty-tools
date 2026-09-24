//! Tests for the #7602 operator repair of a stuck delegation record.
//!
//! Why: every arm of this verb is a refusal, and a refusal that silently
//! becomes a write is how a live agent's working tree gets handed to a second
//! writer. The gate is therefore covered arm by arm, both as the pure decision
//! and through the state-mutating entry point.
//! What: [`decide`]'s five outcomes, [`owner_liveness`]'s three, and
//! [`repair_delegation`]'s write-or-refuse behaviour over a real
//! [`DaemonState`].
//! Test: this file IS the test module.

use super::*;
use crate::core::agent::{DelegationStatus, ModelTier};
use crate::core::session::{ControlModel, Session};
use crate::daemon::services::delegation_records::delegation_records_tests::assert_no_uuid;
use crate::daemon::state::DaemonState;

/// A daemon holding one session in `status`, or holding none at all.
fn state_with(status: Option<SessionStatus>) -> (Arc<DaemonState>, SessionId) {
    let state = Arc::new(DaemonState::new());
    let id = SessionId::new();
    if let Some(status) = status {
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = status;
        state.register_session(s);
    }
    (state, id)
}

/// One delegation naming `agent_id`, in `status`.
fn delegation(session: SessionId, agent_id: &str, status: DelegationStatus) -> Delegation {
    let mut d = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    d.agent_id = Some(agent_id.to_string());
    d.status = status;
    d
}

// #7602: the reported shape — a record left non-terminal by a session that
// ended without a stop hook, whose owner the daemon can see is gone.
#[test]
fn repair_ends_a_stuck_record_of_a_dead_owner_7602() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(
        session,
        "af20cc838b2b30a55",
        DelegationStatus::Running,
    ));

    let outcome = repair_delegation(&state, "af20cc838b2b30a55", false);

    assert_eq!(outcome, RepairOutcome::Ended { records: 1 });
    // The status must be TERMINAL: `Stale` is what #6497 already wrote and is
    // exactly what left the worktree gate refusing.
    let d = &state.all_delegations()[0];
    assert_eq!(d.status, DelegationStatus::Cancelled);
    assert!(d.status.is_terminal(), "the repair must release the gate");
}

// #7602: the closure condition, end to end — the worktree gate that refused the
// specimen ("a delegation naming it has not ended") answers `Ended` afterwards.
#[test]
fn repair_releases_the_worktree_reclaim_gate_7602() {
    use crate::daemon::services::agent_worktree_reap::delegation_state_for_agent;
    use crate::session_manager::worktree_ownership::AgentDelegationState;

    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(
        session,
        "af20cc838b2b30a55",
        DelegationStatus::Running,
    ));
    assert_eq!(
        delegation_state_for_agent(&state, "af20cc838b2b30a55"),
        AgentDelegationState::Live,
        "precondition: the gate refuses the tree"
    );

    repair_delegation(&state, "af20cc838b2b30a55", false);

    assert_eq!(
        delegation_state_for_agent(&state, "af20cc838b2b30a55"),
        AgentDelegationState::Ended
    );
}

// #7602: the whole point of the reclaim gate — a live owner is never overridden,
// whatever the operator asserts.
#[test]
fn repair_refuses_while_the_owner_is_live_7602() {
    let (state, session) = state_with(Some(SessionStatus::Active));
    state.upsert_delegation(delegation(session, "agent-live", DelegationStatus::Running));

    let outcome = repair_delegation(&state, "agent-live", true);

    match outcome {
        RepairOutcome::Refused { reason } => assert!(reason.contains("still Active"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

// #7602 / ADR-0045: the delegation map is rebuilt empty at every daemon start,
// so an owner the registry cannot find is undeterminable, not gone.
#[test]
fn repair_refuses_an_undeterminable_owner_without_force_7602() {
    let (state, session) = state_with(None);
    state.upsert_delegation(delegation(
        session,
        "agent-unknown",
        DelegationStatus::Running,
    ));

    let outcome = repair_delegation(&state, "agent-unknown", false);

    match outcome {
        RepairOutcome::Refused { reason } => assert!(reason.contains("--force"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

#[test]
fn repair_force_ends_a_record_whose_owner_is_unknown_7602() {
    let (state, session) = state_with(None);
    state.upsert_delegation(delegation(
        session,
        "agent-unknown",
        DelegationStatus::Running,
    ));

    assert_eq!(
        repair_delegation(&state, "agent-unknown", true),
        RepairOutcome::Ended { records: 1 }
    );
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Cancelled
    );
}

// #7602: a `Stale` record is NOT terminal, so it is exactly the population this
// verb exists to end — the #6497 sweep leaves records in this state and the
// reclaim gate still reads them as live.
#[test]
fn repair_ends_a_stale_record_too_7602() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(session, "agent-stale", DelegationStatus::Stale));

    assert_eq!(
        repair_delegation(&state, "agent-stale", false),
        RepairOutcome::Ended { records: 1 }
    );
}

#[test]
fn repair_reports_no_record_for_an_unknown_agent_7602() {
    let (state, _session) = state_with(Some(SessionStatus::Stopped));
    assert_eq!(
        repair_delegation(&state, "agent-nobody-knows", false),
        RepairOutcome::NoRecord
    );
}

#[test]
fn repair_reports_already_ended_and_writes_nothing_7602() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(
        session,
        "agent-done",
        DelegationStatus::Completed,
    ));

    assert_eq!(
        repair_delegation(&state, "agent-done", false),
        RepairOutcome::AlreadyEnded { records: 1 }
    );
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Completed
    );
}

#[test]
fn owner_liveness_reads_active_stopped_and_absent_7602() {
    let (active, id) = state_with(Some(SessionStatus::Active));
    assert_eq!(owner_liveness(&active, id), OwnerLiveness::Live);

    let (stopped, id) = state_with(Some(SessionStatus::Stopped));
    assert_eq!(owner_liveness(&stopped, id), OwnerLiveness::Gone);

    let (empty, id) = state_with(None);
    assert_eq!(owner_liveness(&empty, id), OwnerLiveness::Unknown);
}

#[test]
fn decide_refuses_a_live_owner_even_with_force_7602() {
    assert!(matches!(
        decide(1, 0, OwnerLiveness::Live, true),
        RepairOutcome::Refused { .. }
    ));
}

#[test]
fn decide_needs_force_for_an_unknown_owner_7602() {
    assert!(matches!(
        decide(1, 0, OwnerLiveness::Unknown, false),
        RepairOutcome::Refused { .. }
    ));
    assert_eq!(
        decide(1, 0, OwnerLiveness::Unknown, true),
        RepairOutcome::Ended { records: 1 }
    );
}

#[test]
fn decide_ends_a_gone_owners_records_7602() {
    assert_eq!(
        decide(2, 1, OwnerLiveness::Gone, false),
        RepairOutcome::Ended { records: 2 }
    );
}

#[test]
fn decide_reports_no_record() {
    assert_eq!(
        decide(0, 0, OwnerLiveness::Unknown, true),
        RepairOutcome::NoRecord
    );
}

/// A record a stop matched by agent type (#6556): no agent id, `Stale`.
fn type_matched(session: SessionId) -> Delegation {
    let mut d = Delegation::new(session, None, "version-control", ModelTier::Sonnet, "work");
    d.status = DelegationStatus::Stale;
    d.stale_by_agent_type = true;
    d
}

// #8257: `tm repair delegation <agent-id>` could never name this record.
#[test]
fn repair_by_delegation_id_ends_a_record_with_no_agent_id_8257() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    let mut d = delegation(session, "unused", DelegationStatus::Running);
    d.agent_id = None;
    state.upsert_delegation(d.clone());

    assert_eq!(
        repair_delegation_by_id(&state, d.id, false, &no_caller()),
        RepairOutcome::Ended { records: 1 }
    );
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Cancelled
    );
}

// #8257: the 2026-09-17 specimen — matched by type, owned by a live session.
// A type-matched stop can name a running sibling (#6556) and leaves nothing for
// the probe to check, so only the owning session may clear it.
#[test]
fn a_type_matched_record_of_a_live_session_needs_its_owner_8257() {
    let (state, session) = state_with(Some(SessionStatus::Active));
    let d = type_matched(session);
    state.upsert_delegation(d.clone());

    for caller in [no_caller(), RepairCaller::Session(SessionId::new())] {
        match repair_delegation_by_id(&state, d.id, true, &caller) {
            RepairOutcome::Refused { reason } => {
                assert!(reason.contains("matched by agent type"), "{reason}");
                assert!(reason.contains("Only the owning session can"), "{reason}");
            }
            other => panic!("expected a refusal for {caller:?}, got {other:?}"),
        }
        assert_eq!(state.all_delegations()[0].status, DelegationStatus::Stale);
    }

    assert_eq!(
        repair_delegation_by_id(&state, d.id, false, &RepairCaller::Session(session)),
        RepairOutcome::Ended { records: 1 }
    );
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Cancelled
    );
}

// #8257: one record whose owner is gone and one whose owner the registry cannot
// find. The undeterminable one decides the call, so it needs --force.
#[test]
fn an_unknown_owner_outranks_a_gone_one_8257() {
    let (state, gone) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(gone, "agent-pair", DelegationStatus::Running));
    state.upsert_delegation(delegation(
        SessionId::new(),
        "agent-pair",
        DelegationStatus::Running,
    ));
    let run = |force| {
        repair_matching(
            &state,
            |d| d.agent_id.as_deref() == Some("agent-pair"),
            force,
            &no_caller(),
            |_| LiveEvidence::Clear,
        )
    };

    match run(false) {
        RepairOutcome::Refused { reason } => assert!(reason.contains("--force"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        state
            .all_delegations()
            .iter()
            .all(|d| d.status == DelegationStatus::Running && d.repair.is_none()),
        "a refusal writes nothing"
    );
    assert_eq!(run(true), RepairOutcome::Ended { records: 2 });
}

// #8257 Fail-Open Check: a process standing in the agent's own tree refuses
// the repair, whatever the registry says and whatever `--force` asserts.
#[test]
fn repair_refuses_while_a_live_process_holds_the_tree_8257() {
    let tree = tempfile::tempdir().expect("tree");
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .current_dir(tree.path())
        .spawn()
        .expect("spawn sleep");
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    let mut d = delegation(session, "agent-held", DelegationStatus::Running);
    d.cwd = Some(std::path::PathBuf::from("/repo"));
    d.worktree_path = Some(tree.path().to_path_buf());
    state.upsert_delegation(d);

    let outcome = repair_delegation(&state, "agent-held", true);
    let _ = child.kill();
    let _ = child.wait();

    match outcome {
        RepairOutcome::Refused { reason } => {
            assert!(reason.contains("live process"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

// #8257 Fail-Open Check: a probe that cannot answer refuses, naming the step.
#[test]
fn repair_refuses_when_the_live_agent_probe_cannot_answer_8257() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(session, "agent-x", DelegationStatus::Running));

    let outcome = repair_matching(
        &state,
        |d| d.agent_id.as_deref() == Some("agent-x"),
        true,
        &no_caller(),
        |_| LiveEvidence::Undeterminable("lsof exited 1".to_string()),
    );

    match outcome {
        RepairOutcome::Refused { reason } => {
            assert!(reason.contains("could not answer"), "{reason}");
            assert!(reason.contains("lsof exited 1"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

// #8257: each arm of the harness-lock check; every unanswered step is `Err`.
#[test]
fn lock_holder_evidence_reads_each_arm_8257() {
    use super::super::delegation_repair_probe::lock_holder_evidence;
    let reason = "claude agent agent-a1 (pid 4242 start Mon Sep 14 15:30:46 2026)";
    // The harness writes the stamp in UTC.
    let started = chrono::NaiveDate::from_ymd_opt(2026, 9, 14)
        .and_then(|d| d.and_hms_opt(15, 30, 46))
        .expect("date")
        .and_utc()
        .timestamp();
    let at = move |_| Some(started);

    assert!(matches!(
        lock_holder_evidence(reason, |_| Some(true), at),
        Ok(Some(_))
    ));
    assert_eq!(lock_holder_evidence(reason, |_| Some(false), at), Ok(None));
    assert_eq!(
        lock_holder_evidence(reason, |_| Some(true), move |_| Some(started + 5000)),
        Ok(None),
        "a reused pid is not the lock's holder"
    );
    assert!(
        lock_holder_evidence(reason, |_| Some(true), move |_| Some(started + 3600)).is_err(),
        "a start one zone offset away is ambiguous, never dead"
    );
    assert!(lock_holder_evidence(reason, |_| None, at).is_err());
    assert!(lock_holder_evidence(reason, |_| Some(true), |_| None).is_err());
    assert!(
        lock_holder_evidence("claude agent agent-a1 (pid 4242)", |_| Some(true), at).is_err(),
        "no start time to match"
    );
    assert!(lock_holder_evidence("claude agent agent-a1", |_| Some(true), at).is_err());
}

// #8257: the harness writes the lock's start in UTC. On a machine whose zone is
// not UTC, a live holder must still read live — whether the stamp is in UTC or
// in the machine's zone — and a stamp in any other zone must read unknown.
#[test]
fn a_live_lock_reads_live_in_any_time_zone_8257() {
    use super::super::delegation_repair_probe::lock_holder_evidence_in;
    use chrono::{FixedOffset, TimeZone};
    let started: i64 = 1_790_246_739; // Thu Sep 24 2026, 10:45:39 UTC
    let stamp_in = |tz: &FixedOffset| {
        let at = tz.timestamp_opt(started, 0).single().expect("epoch");
        format!(
            "claude agent agent-a1 (pid 4242 start {})",
            at.format("%a %b %e %H:%M:%S %Y")
        )
    };
    let utc = FixedOffset::east_opt(0).expect("utc");
    for secs in [
        -4 * 3600,
        -5 * 3600,
        5 * 3600 + 1800,
        9 * 3600,
        14 * 3600,
        -12 * 3600,
    ] {
        let machine = FixedOffset::east_opt(secs).expect("offset");
        for written in [&utc, &machine] {
            let reason = stamp_in(written);
            assert!(
                matches!(
                    lock_holder_evidence_in(&reason, |_| Some(true), |_| Some(started), &machine),
                    Ok(Some(_))
                ),
                "machine {machine}, stamp {reason}: a live holder must read live"
            );
        }
        let elsewhere = FixedOffset::east_opt(3 * 3600).expect("offset");
        let reason = stamp_in(&elsewhere);
        assert!(
            lock_holder_evidence_in(&reason, |_| Some(true), |_| Some(started), &machine).is_err(),
            "machine {machine}, stamp {reason}: another zone is unknown, never dead"
        );
    }
}

/// A live session owning one young `Running` record naming `agent_id`.
fn live_owned(agent_id: &str) -> (Arc<DaemonState>, SessionId) {
    let (state, session) = state_with(Some(SessionStatus::Active));
    state.upsert_delegation(delegation(session, agent_id, DelegationStatus::Running));
    (state, session)
}

/// Run `body` under a thread-local capture subscriber; return its rendered lines.
fn captured<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    use tracing_subscriber::layer::SubscriberExt;

    // #4931: raise the process-global MAX_LEVEL a thread-local default cannot.
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let out = tracing::subscriber::with_default(subscriber, body);
    (out, buffer.tail(64))
}

/// The captured lines that record a repair clearing a record.
fn clear_lines(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.contains("repair cleared a record"))
        .collect()
}

// #8257 owner ruling: the owning session may clear its own live record once
// the probe finds no live agent; the record and the log say who and why.
#[test]
fn the_owning_session_clears_its_own_live_record_8257() {
    let (state, session) = live_owned("a-own");

    let (outcome, lines) =
        captured(|| repair_delegation_as(&state, "a-own", false, &RepairCaller::Session(session)));

    assert_eq!(outcome, RepairOutcome::Ended { records: 1 });
    let d = &state.all_delegations()[0];
    assert_eq!(d.status, DelegationStatus::Cancelled);
    let repair = d
        .repair
        .as_ref()
        .expect("the repair is stamped on the record");
    assert_eq!(repair.by_session, Some(session));
    assert_eq!(repair.reason, "owner-attested finished (#8257)");

    let logged = clear_lines(&lines);
    assert_eq!(logged.len(), 1, "one clear line per record: {lines:#?}");
    let line = logged[0];
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains(&d.id.0.to_string()), "{line}");
    assert!(
        line.contains(&format!("by_session={}", session.0)),
        "{line}"
    );
    assert!(line.contains("owner-attested finished"), "{line}");
    assert!(line.contains("forced=false"), "{line}");
}

// #8257 owner ruling: live evidence binds the owner too — a probe that sees
// the agent alive refuses, writes nothing, and logs no clear.
#[test]
fn the_owner_is_refused_while_the_agent_shows_live_8257() {
    let (state, session) = live_owned("a-alive");

    let (outcome, lines) = captured(|| {
        repair_matching(
            &state,
            |d| d.agent_id.as_deref() == Some("a-alive"),
            false,
            &RepairCaller::Session(session),
            |_| LiveEvidence::Held("pid 4242 has its cwd in the tree".to_string()),
        )
    });

    match outcome {
        RepairOutcome::Refused { reason } => {
            assert!(reason.contains("a live process still holds"), "{reason}");
            assert!(reason.contains("pid 4242"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    let d = &state.all_delegations()[0];
    assert_eq!(d.status, DelegationStatus::Running);
    assert!(d.repair.is_none(), "a refusal stamps nothing");
    assert!(clear_lines(&lines).is_empty(), "{lines:#?}");
}

// #8257 owner ruling: a `--force` clear is logged like an owner's, naming the
// unestablished caller and the forced basis.
#[test]
fn a_forced_clear_is_logged_8257() {
    let (state, session) = state_with(None);
    state.upsert_delegation(delegation(session, "a-forced", DelegationStatus::Running));

    let (outcome, lines) = captured(|| repair_delegation(&state, "a-forced", true));

    assert_eq!(outcome, RepairOutcome::Ended { records: 1 });
    let logged = clear_lines(&lines);
    assert_eq!(logged.len(), 1, "{lines:#?}");
    assert!(logged[0].contains("forced=true"), "{}", logged[0]);
    assert!(
        logged[0].contains("by_session=unestablished"),
        "{}",
        logged[0]
    );
    assert!(logged[0].contains("operator-forced"), "{}", logged[0]);
}

#[test]
fn a_non_owning_session_is_refused_on_a_live_record_8257() {
    let (state, owner) = live_owned("a-other");
    let stranger = SessionId::new();

    let outcome = repair_delegation_as(&state, "a-other", true, &RepairCaller::Session(stranger));

    match outcome {
        RepairOutcome::Refused { reason } => {
            assert!(reason.contains("Only the owning session can"), "{reason}");
            assert_no_uuid(&reason, owner);
            // The caller's own id is its own to see.
            assert!(reason.contains(&stranger.0.to_string()), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

// #8257 owner ruling: no owner-check refusal hands the caller the owner's
// UUID — every caller shape, on both the agent-id and the delegation-id path.
// The owner is named by its tmux name, which the caller header rejects.
#[test]
fn an_owner_refusal_never_names_the_owner_uuid_8257() {
    let (state, owner) = live_owned("a-hidden");
    let typed = type_matched(owner);
    state.upsert_delegation(typed.clone());
    let label = crate::daemon::services::delegation_records::owner_label(&state, owner);
    assert!(label.starts_with("session `tm-"), "{label}");
    let callers = [
        no_caller(),
        RepairCaller::from_request(None),
        RepairCaller::from_request(Some("not-a-uuid")),
        RepairCaller::from_request(Some(&label)),
        RepairCaller::Session(SessionId::new()),
    ];

    for caller in &callers {
        for outcome in [
            repair_delegation_as(&state, "a-hidden", true, caller),
            repair_delegation_by_id(&state, typed.id, true, caller),
        ] {
            let RepairOutcome::Refused { reason } = outcome else {
                panic!("expected a refusal for {caller:?}, got {outcome:?}");
            };
            assert!(reason.contains(&format!("owned by {label}")), "{reason}");
            assert_no_uuid(&reason, owner);
            // The wire form the CLI prints is the same text.
            let wire = serde_json::to_string(&RepairOutcome::Refused { reason }).expect("json");
            assert_no_uuid(&wire, owner);
        }
    }
}

// #8257 owner ruling: the refusal text drops the owner's UUID, so the daemon
// log is where the operator finds it — one WARN line per refusal.
#[test]
fn an_owner_refusal_logs_the_owner_uuid_8257() {
    let (state, owner) = live_owned("a-logged");
    let stranger = SessionId::new();

    let (outcome, lines) = captured(|| {
        repair_delegation_as(&state, "a-logged", false, &RepairCaller::Session(stranger))
    });

    assert!(
        matches!(outcome, RepairOutcome::Refused { .. }),
        "{outcome:?}"
    );
    let refused: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("repair refused"))
        .collect();
    assert_eq!(refused.len(), 1, "{lines:#?}");
    let line = refused[0];
    assert!(line.contains("WARN"), "{line}");
    assert!(
        line.contains(&format!("owner_session={}", owner.0.hyphenated())),
        "{line}"
    );
    assert!(line.contains(&format!("caller={}", stranger.0)), "{line}");
    assert!(clear_lines(&lines).is_empty(), "{lines:#?}");
}

// #8257: a caller whose session cannot be established is refused, naming why —
// never read as the owner and never as "someone else".
#[test]
fn an_unestablished_caller_is_refused_on_a_live_record_8257() {
    let (state, _owner) = live_owned("a-anon");
    for (raw, want) in [
        (None, "CLAUDE_CODE_SESSION_ID"),
        (Some("not-a-uuid"), "is not a session id"),
    ] {
        let outcome =
            repair_delegation_as(&state, "a-anon", true, &RepairCaller::from_request(raw));
        match outcome {
            RepairOutcome::Refused { reason } => {
                assert!(reason.contains("could not be established"), "{reason}");
                assert!(reason.contains(want), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

// #8257 owner ruling: live evidence still binds the owner. A harness lock
// naming the agent, whose pid runs with the lock's start time, refuses.
#[test]
fn the_owner_is_refused_while_a_harness_lock_names_its_agent_8257() {
    use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

    let fixture = GitWorktreeFixture::new();
    let wt = fixture.add_worktree("agent-a1b2c3");
    let pid = std::process::id();
    let started = {
        let spid = sysinfo::Pid::from_u32(pid);
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[spid]), true);
        let secs = sys.process(spid).expect("this process").start_time();
        // #8257: the harness writes this stamp in UTC.
        chrono::DateTime::from_timestamp(i64::try_from(secs).expect("epoch"), 0)
            .expect("timestamp")
            .format("%a %b %e %H:%M:%S %Y")
            .to_string()
    };
    let lock = std::process::Command::new("git")
        .arg("-C")
        .arg(&fixture.repo)
        .args(["worktree", "lock", "--reason"])
        .arg(format!(
            "claude agent agent-a1b2c3 (pid {pid} start {started})"
        ))
        .arg(&wt)
        .status()
        .expect("git worktree lock");
    assert!(lock.success());

    let (state, session) = state_with(Some(SessionStatus::Active));
    let mut d = delegation(session, "a1b2c3", DelegationStatus::Running);
    d.cwd = Some(fixture.repo.clone());
    state.upsert_delegation(d);

    let outcome = repair_delegation_as(&state, "a1b2c3", true, &RepairCaller::Session(session));

    match outcome {
        RepairOutcome::Refused { reason } => {
            assert!(reason.contains("harness lock pid"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

/// A gone owner's `Running` record naming `agent_id`, dispatched from `cwd`.
fn gone_owned_in(agent_id: &str, cwd: &std::path::Path) -> Arc<DaemonState> {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    let mut d = delegation(session, agent_id, DelegationStatus::Running);
    d.cwd = Some(cwd.to_path_buf());
    state.upsert_delegation(d);
    state
}

// #8257 critic R2: outside any repository no harness lock can exist, so git's
// "not a git repository" exit 128 must not refuse the repair.
#[test]
fn a_record_whose_cwd_is_outside_any_repository_is_repairable_8257() {
    use crate::session_manager::worktree_registry::list_registered_worktrees;

    let dir = tempfile::tempdir().expect("non-git cwd");
    assert!(
        list_registered_worktrees(dir.path()).is_none(),
        "precondition: git cannot list worktrees for {}",
        dir.path().display()
    );
    for force in [false, true] {
        let state = gone_owned_in("a-nongit", dir.path());
        assert_eq!(
            repair_delegation(&state, "a-nongit", force),
            RepairOutcome::Ended { records: 1 },
            "force={force}"
        );
    }
}

// #8257 critic R2: a real repository git cannot read is not "no repository" —
// git says "not a git repository" here too, and the repair still refuses.
#[cfg(unix)]
#[test]
fn a_record_whose_repository_git_cannot_read_still_refuses_8257() {
    use crate::session_manager::worktree_registry::list_registered_worktrees;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("repo cwd");
    let init = std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(dir.path())
        .status()
        .expect("git init");
    assert!(init.success());
    let git_dir = dir.path().join(".git");
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let listed = list_registered_worktrees(dir.path());
    let state = gone_owned_in("a-unreadable", dir.path());
    let outcome = repair_delegation(&state, "a-unreadable", true);
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o755)).expect("restore");

    assert!(
        listed.is_none(),
        "precondition: git fails on an unreadable .git"
    );
    match outcome {
        RepairOutcome::Refused { reason } => {
            assert!(reason.contains("git could not list"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

// #8257 critic R2: a cwd that no longer exists reads `Clear`, as it did before.
#[test]
fn a_record_whose_cwd_is_gone_is_repairable_8257() {
    let dir = tempfile::tempdir().expect("parent");
    let state = gone_owned_in("a-gone-cwd", &dir.path().join("removed"));
    assert_eq!(
        repair_delegation(&state, "a-gone-cwd", true),
        RepairOutcome::Ended { records: 1 }
    );
}

#[test]
fn decide_reports_already_ended() {
    assert_eq!(
        decide(0, 3, OwnerLiveness::Gone, false),
        RepairOutcome::AlreadyEnded { records: 3 }
    );
}
