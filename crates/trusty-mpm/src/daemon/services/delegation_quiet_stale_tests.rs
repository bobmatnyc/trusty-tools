//! Tests for the #6556 quiet-bound staling pass.
//!
//! Test: this file IS the test module
//! (`#[path = "delegation_quiet_stale_tests.rs"] mod tests;`).

use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::services::delegation_tracker::observe;

/// The dispatcher's shared checkout.
const REPO: &str = "/repo";
/// The tree the harness gave the agent.
const TREE: &str = "/repo/.claude/worktrees/agent-a1";

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-11T04:00:00Z")
        .expect("fixed instant")
        .with_timezone(&Utc)
}

fn default_bound() -> Duration {
    Duration::seconds(DEFAULT_QUIET_STALE_SECS)
}

/// A `Running` record, last heard from at `heard`, standing in its own tree.
fn isolated(session: SessionId, agent_id: &str, heard: DateTime<Utc>) -> Delegation {
    let mut d = Delegation::observed(session, "rust-engineer", "task", Some("toolu_1".into()));
    d.status = DelegationStatus::Running;
    d.agent_id = Some(agent_id.to_string());
    d.cwd = Some(PathBuf::from(REPO));
    d.worktree_path = Some(PathBuf::from(TREE));
    d.last_agent_cwd = Some(PathBuf::from(TREE).join("crates"));
    d.created_at = heard;
    d.started_at = Some(heard);
    d.last_event_at = Some(heard);
    d
}

/// One record `bound + 1 s` quiet, run through the pass with `evidence`.
fn status_after(evidence: LiveEvidence) -> (usize, DelegationStatus) {
    let state = DaemonState::new();
    let session = SessionId::new();
    state.upsert_delegation(isolated(session, "a1", t0()));
    let now = t0() + default_bound() + Duration::seconds(1);
    let staled = stale_quiet_delegations_at(&state, now, default_bound(), |_| evidence.clone());
    (staled, state.delegations_for(session)[0].status)
}

/// 🔴 REGRESSION (#6556), criterion 1: quiet past the bound with no live OS
/// evidence reads `Stale`; the same record with live evidence, or with a probe
/// that cannot answer, reads `Running` (fail closed).
#[test]
fn a_quiet_isolated_record_reads_stale_only_without_live_evidence() {
    assert_eq!(
        status_after(LiveEvidence::Clear),
        (1, DelegationStatus::Stale),
        "quiet, isolated, and nothing on the machine holds the tree"
    );
    assert_eq!(
        status_after(LiveEvidence::Held("pid 42 stands in the tree".into())),
        (0, DelegationStatus::Running),
        "live OS evidence keeps the record running"
    );
    assert_eq!(
        status_after(LiveEvidence::Undeterminable("lsof is missing".into())),
        (0, DelegationStatus::Running),
        "a probe that cannot answer never stales"
    );
}

/// 🔴 REGRESSION (#6556), criterion 2: a quiet-staled record that then gets
/// its stop ends exactly where a `Running` record that gets its stop ends.
#[test]
fn a_quiet_staled_record_ends_like_a_running_one_on_its_stop() {
    let state = DaemonState::new();
    let session = SessionId::new();
    let now = t0() + default_bound() + Duration::seconds(1);
    state.upsert_delegation(isolated(session, "quiet", t0()));
    state.upsert_delegation(isolated(session, "busy", now));

    let staled = stale_quiet_delegations_at(&state, now, default_bound(), |_| LiveEvidence::Clear);
    assert_eq!(staled, 1);
    let by_id = |id: &str| {
        state
            .delegations_for(session)
            .into_iter()
            .find(|d| d.agent_id.as_deref() == Some(id))
            .expect("record")
    };
    assert_eq!(by_id("quiet").status, DelegationStatus::Stale);
    assert_eq!(by_id("busy").status, DelegationStatus::Running);

    for id in ["quiet", "busy"] {
        let stop = json!({ "agent_id": id, "agent_type": "rust-engineer" });
        observe(&state, session, HookEvent::SubagentStop, &stop);
    }

    let (quiet, busy) = (by_id("quiet"), by_id("busy"));
    assert_eq!(quiet.status, DelegationStatus::Completed);
    assert_eq!(quiet.status, busy.status);
    assert!(quiet.ended_at.is_some() && busy.ended_at.is_some());
    assert_eq!(quiet.stale_by_agent_type, busy.stale_by_agent_type);
    assert_eq!(
        state.pending_stops().len(),
        0,
        "both stops resolved a record"
    );
}

/// 🔴 REGRESSION (#6556), criterion 3: a non-default bound is honoured on
/// both sides of it.
#[test]
fn a_custom_quiet_bound_is_honoured() {
    let bound = Duration::seconds(5 * 60);
    let session = SessionId::new();
    let now = t0() + Duration::seconds(60 * 60);
    let clear = |_: &Delegation| LiveEvidence::Clear;

    let inside = isolated(session, "a1", now - Duration::seconds(4 * 60));
    assert_eq!(
        quiet_verdict(&inside, now, bound, clear),
        QuietVerdict::NotQuiet
    );
    let past = isolated(session, "a1", now - Duration::seconds(6 * 60));
    assert_eq!(quiet_verdict(&past, now, bound, clear), QuietVerdict::Stale);
    assert_eq!(
        quiet_verdict(&past, now, default_bound(), clear),
        QuietVerdict::NotQuiet,
        "six quiet minutes are inside the default bound"
    );

    let state = DaemonState::new();
    state.upsert_delegation(past);
    assert_eq!(stale_quiet_delegations_at(&state, now, bound, clear), 1);
    assert_eq!(
        state.delegations_for(session)[0].status,
        DelegationStatus::Stale
    );
}

/// The override parses whole positive seconds and rejects everything else.
#[test]
fn the_quiet_bound_parses_overrides_and_rejects_nonsense() {
    assert_eq!(DEFAULT_QUIET_STALE_SECS, 15 * 60);
    assert_eq!(parse_quiet_stale_secs(None), Ok(None));
    assert_eq!(parse_quiet_stale_secs(Some(" 300 ")), Ok(Some(300)));
    for bad in ["", "0", "-5", "ten", "1.5", "99999999999999"] {
        assert_eq!(
            parse_quiet_stale_secs(Some(bad)),
            Err(bad.to_string()),
            "{bad:?}"
        );
    }
}

/// 🔴 REGRESSION (#6556), criterion 4: an agent with no tree of its own stays
/// Undeterminable under the same quiet conditions, and is never probed.
#[test]
fn an_unisolated_quiet_record_stays_undeterminable() {
    let state = DaemonState::new();
    let session = SessionId::new();
    let mut shared = isolated(session, "shared", t0());
    shared.worktree_path = None;
    shared.last_agent_cwd = Some(PathBuf::from(REPO));
    // Entered a tree once, then walked back out into the shared checkout.
    let mut left = isolated(session, "left", t0());
    left.last_agent_cwd = Some(PathBuf::from(REPO));
    let now = t0() + default_bound() + Duration::seconds(1);
    let never = |_: &Delegation| -> LiveEvidence { panic!("an unisolated agent is never probed") };

    for d in [shared, left] {
        assert_eq!(
            quiet_verdict(&d, now, default_bound(), never),
            QuietVerdict::Undeterminable
        );
        state.upsert_delegation(d);
    }
    assert_eq!(
        stale_quiet_delegations_at(&state, now, default_bound(), never),
        0
    );
    assert!(
        state
            .delegations_for(session)
            .iter()
            .all(|d| d.status == DelegationStatus::Running)
    );
}

/// A hook event that lands while the probe runs wins over the probe's answer.
#[test]
fn a_hook_event_during_the_probe_wins() {
    let state = DaemonState::new();
    let session = SessionId::new();
    let d = isolated(session, "a1", t0());
    let id = d.id;
    state.upsert_delegation(d);
    let now = t0() + default_bound() + Duration::seconds(1);

    let staled = stale_quiet_delegations_at(&state, now, default_bound(), |_| {
        state.mutate_delegation(id, |r| r.last_event_at = Some(now));
        LiveEvidence::Clear
    });

    assert_eq!(staled, 0);
    assert_eq!(
        state.delegations_for(session)[0].status,
        DelegationStatus::Running
    );
}

/// The quiet bound measures from the agent's own last hook event.
#[test]
fn a_subagent_tool_call_stamps_its_last_hook_event() {
    let state = DaemonState::new();
    let session = SessionId::new();
    let mut d = isolated(session, "a1", t0());
    d.last_event_at = None;
    state.upsert_delegation(d);
    let before = Utc::now();

    let call = json!({ "tool": "Bash", "agent_id": "a1", "cwd": TREE });
    observe(&state, session, HookEvent::PreToolUse, &call);

    let stamped = state.delegations_for(session)[0].last_event_at;
    assert!(stamped.is_some_and(|at| at >= before), "{stamped:?}");
}
