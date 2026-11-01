//! One refresh tick split into a fetch that runs off the UI loop and an apply
//! that runs on it (#10476).
//!
//! Why: the run loop awaited the whole poll inline, so a slow fleet call (~1 s
//! measured) froze drawing and key input for the length of the tick. And the
//! tick fetched `/activity` (a tmux capture on the daemon) every second.
//! What: [`request_for`] snapshots what a tick needs from the state.
//! [`fetch_tick`] takes an owned client and that request, so it can run as a
//! spawned task, and returns a [`PollOutcome`]. [`apply_tick`] writes the
//! result into the state. [`activity_target`] asks for `/activity` only when
//! the selected session has no request younger than [`ACTIVITY_REFRESH`]; other
//! ticks serve the cached Activity pane. [`HealthCadence`] asks for the
//! health probe on the first poll and every [`HEALTH_EVERY`]th one after.
//! Test: `normal_tick_makes_no_activity_call_10476`,
//! `health_checked_every_nth_tick`,
//! `fleet_error_without_health_check_marks_unreachable_10476`
//! (`super::cadence_tests`);
//! `activity_target_serves_cache_until_refresh_is_due` (`super::tests`);
//! `keypress_redraws_while_poll_in_flight_10476` (`project_ctl::tests`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::{
    ActivityFetchFailure, apply_activity_outcome, classify_activity_error,
    fetch_projects_and_sessions, rediscover, sync_open_view_deliverables,
};
use crate::client::{DaemonClient, ManagedActivityResponse};
use crate::deliverable::{Deliverable, Milestone};
use crate::project::Project;
use crate::tui::project_ctl::state::{ProjectCtlState, ProjectRow, SessionRow};
use crate::tui::project_ctl::trace::CallTimings;

/// How long a fetched Activity pane is served before the next `/activity`.
///
/// Why: the Architect kept the Activity pane on a slower cached refresh, so a
/// normal 1 s tick makes no tmux call (#10476 Q-A).
pub(crate) const ACTIVITY_REFRESH: Duration = Duration::from_secs(5);

/// How many polls share one health probe: the first poll and every
/// `HEALTH_EVERY`th one after it call `/health`.
///
/// Why: at the default 1 s interval this probes health every 5 s, the same
/// cadence as [`ACTIVITY_REFRESH`]; a skipped probe loses nothing, because a
/// failed fleet call already marks the daemon unreachable (#10476).
pub(crate) const HEALTH_EVERY: u64 = 5;

/// Counts polls to decide which ones probe `/health`.
#[derive(Debug, Default)]
pub(crate) struct HealthCadence {
    polls: u64,
}

impl HealthCadence {
    /// Whether the next poll probes health, and count that poll.
    ///
    /// Why: the probe cost one round trip on every tick (#10476).
    /// What: due on the first poll, every [`HEALTH_EVERY`]th poll, and every
    /// poll while the daemon reads as unreachable, so re-discovery of a moved
    /// daemon still runs on each tick until it answers.
    /// Test: `health_checked_every_nth_tick`.
    pub(crate) fn next_due(&mut self, daemon_reachable: bool) -> bool {
        let due = self.polls.is_multiple_of(HEALTH_EVERY) || !daemon_reachable;
        self.polls += 1;
        due
    }
}

/// The registry projects, the fleet's session rows, and the full records.
pub(crate) type FleetData = (
    Vec<ProjectRow>,
    BTreeMap<String, Vec<SessionRow>>,
    BTreeMap<String, Project>,
);

/// The health probe and the project/fleet fetch of one tick.
#[derive(Debug, Default)]
pub(crate) struct CoreFetch {
    /// `true` when health answered and both list calls succeeded.
    pub reachable: bool,
    /// The fetched lists; `None` when the daemon is down or a call failed.
    pub fleet: Option<FleetData>,
}

/// The selected project's Deliverables (and Milestones, while the view is open).
#[derive(Debug, Default)]
pub(crate) struct DeliverablesFetch {
    /// The project the request named, so a moved selection discards the result.
    pub project: Option<String>,
    /// `None` when not fetched or the call failed (the state keeps its list).
    pub deliverables: Option<Vec<Deliverable>>,
    /// `None` when not fetched or the call failed.
    pub milestones: Option<Vec<Milestone>>,
}

/// One `/activity` fetch: the session it was for and its result.
pub(crate) type ActivityFetch = (
    String,
    Result<ManagedActivityResponse, ActivityFetchFailure>,
);

/// Everything one tick fetched, ready for [`apply_tick`].
#[derive(Debug, Default)]
pub(crate) struct PollResult {
    pub core: CoreFetch,
    /// `Some` only on a tick that asked for activity.
    pub activity: Option<ActivityFetch>,
    pub deliverables: DeliverablesFetch,
}

/// When a tick started and what each call cost, for `TM_TUI_TRACE`.
#[derive(Debug)]
pub(crate) struct TickTiming {
    pub started: Instant,
    pub calls: CallTimings,
    pub total: Duration,
}

/// A finished tick: the client (its URL may have been re-discovered), the
/// result, and the timing.
#[derive(Debug)]
pub(crate) struct PollOutcome {
    pub client: DaemonClient,
    pub result: PollResult,
    pub timing: TickTiming,
}

/// What a tick fetches, read from the state when the tick starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PollRequest {
    /// The selected project, for its Deliverables.
    pub project: Option<String>,
    /// `true` while the Deliverable view is open for `project`.
    pub milestones: bool,
    /// The selected session when its Activity pane is due a refresh.
    pub activity_for: Option<String>,
    /// `true` when this poll probes `/health` first; see [`HealthCadence`].
    pub check_health: bool,
}

/// Snapshot the request for the next tick; records an activity request.
pub(crate) fn request_for(state: &mut ProjectCtlState, now: Instant) -> PollRequest {
    PollRequest {
        activity_for: activity_target(state, now),
        ..request_for_deliverables(state)
    }
}

/// The Deliverables half of [`request_for`]; asks for no activity.
pub(crate) fn request_for_deliverables(state: &ProjectCtlState) -> PollRequest {
    let project = state.selected_project_name().map(str::to_string);
    let milestones = match (&project, &state.deliverable_view) {
        (Some(p), Some(view)) => view.project_name == *p,
        _ => false,
    };
    PollRequest {
        project,
        milestones,
        activity_for: None,
        check_health: false,
    }
}

/// The selected session when its `/activity` is due, else `None`.
///
/// Why: each `/activity` costs a tmux capture on the daemon (#10476).
/// What: due when the daemon is reachable and the selected session has no
/// request recorded in the last [`ACTIVITY_REFRESH`] (a new selection is due at
/// once). A due answer records the request time in `activity_attempt`.
/// Test: `activity_target_serves_cache_until_refresh_is_due`.
pub(crate) fn activity_target(state: &mut ProjectCtlState, now: Instant) -> Option<String> {
    if !state.daemon_reachable {
        return None;
    }
    let id = state.selected_session()?.id.clone();
    if let Some((last, at)) = &state.activity_attempt
        && *last == id
        && now.saturating_duration_since(*at) < ACTIVITY_REFRESH
    {
        return None;
    }
    state.activity_attempt = Some((id.clone(), now));
    Some(id)
}

/// Run one tick's daemon calls; never touches the state.
pub(crate) async fn fetch_tick(mut client: DaemonClient, req: PollRequest) -> PollOutcome {
    let started = Instant::now();
    let mut calls = CallTimings::new();
    let core = fetch_core(&mut client, req.check_health, &mut calls).await;
    let mut result = PollResult {
        core,
        ..PollResult::default()
    };
    if result.core.reachable {
        if let Some(id) = req.activity_for {
            let t = Instant::now();
            let outcome = fetch_activity(&client, &id).await;
            calls.push(("activity", t.elapsed()));
            result.activity = Some((id, outcome));
        }
        if let Some(project) = req.project.as_deref() {
            result.deliverables =
                fetch_deliverables(&client, project, req.milestones, &mut calls).await;
        }
    }
    PollOutcome {
        client,
        result,
        timing: TickTiming {
            started,
            calls,
            total: started.elapsed(),
        },
    }
}

/// Write one tick's result into the state, in the order the inline poll used.
pub(crate) fn apply_tick(state: &mut ProjectCtlState, result: PollResult) {
    apply_core(state, result.core);
    apply_activity(state, result.activity);
    apply_deliverables(state, result.deliverables);
}

/// Health (with one re-discovery retry) when `check_health`, then the project
/// and fleet lists. A failed list call reads as unreachable either way.
pub(crate) async fn fetch_core(
    client: &mut DaemonClient,
    check_health: bool,
    calls: &mut CallTimings,
) -> CoreFetch {
    // #10476: most polls skip the probe and go straight to the fleet calls.
    if check_health {
        let t = Instant::now();
        let mut reachable = client.is_healthy().await;
        if rediscover(client, reachable) {
            reachable = client.is_healthy().await;
        }
        calls.push(("health", t.elapsed()));
        if !reachable {
            return CoreFetch::default();
        }
    }
    let fleet = fetch_projects_and_sessions(client, calls).await.ok();
    CoreFetch {
        reachable: fleet.is_some(),
        fleet,
    }
}

/// Replace the project/session lists, or clear them when the daemon is down.
pub(crate) fn apply_core(state: &mut ProjectCtlState, core: CoreFetch) {
    state.daemon_reachable = core.reachable;
    match core.fleet {
        Some((projects, sessions_by_project, projects_full)) => {
            state.projects = projects;
            state.sessions_by_project = sessions_by_project;
            state.projects_full = projects_full;
        }
        None => {
            state.projects.clear();
            state.sessions_by_project.clear();
            state.projects_full.clear();
            // A down daemon makes the cache old: fetch again once it answers.
            state.activity_attempt = None;
        }
    }
    // #10476: the tree keeps its selection and expanded set across the swap.
    state.rebuild_tree();
}

/// One `/activity` call, classified for [`apply_activity_outcome`].
pub(crate) async fn fetch_activity(
    client: &DaemonClient,
    id: &str,
) -> Result<ManagedActivityResponse, ActivityFetchFailure> {
    client
        .managed_session_activity(id)
        .await
        .map_err(|e| classify_activity_error(&e))
}

/// Apply a tick's activity, or keep the cache when the tick fetched none.
///
/// No selection clears the pane and a down daemon marks it stale, as the
/// inline poll did; a result for a session no longer selected is dropped.
pub(crate) fn apply_activity(state: &mut ProjectCtlState, fetched: Option<ActivityFetch>) {
    let Some(selected) = state.selected_session().map(|s| s.id.clone()) else {
        state.activity = None;
        state.activity_failures.reset();
        return;
    };
    if !state.daemon_reachable {
        apply_activity_outcome(state, selected, Err(ActivityFetchFailure::Transport));
        return;
    }
    if let Some((id, outcome)) = fetched
        && id == selected
    {
        apply_activity_outcome(state, id, outcome);
    }
}

/// Fetch `project`'s Deliverables, and its Milestones when `milestones`.
pub(crate) async fn fetch_deliverables(
    client: &DaemonClient,
    project: &str,
    milestones: bool,
    calls: &mut CallTimings,
) -> DeliverablesFetch {
    let t = Instant::now();
    let deliverables = client.list_deliverables(project, None).await.ok();
    calls.push(("deliverables", t.elapsed()));
    let milestones = if milestones {
        let t = Instant::now();
        let list = client.list_milestones(project).await.ok();
        calls.push(("milestones", t.elapsed()));
        list
    } else {
        None
    };
    DeliverablesFetch {
        project: Some(project.to_string()),
        deliverables,
        milestones,
    }
}

/// Apply fetched Deliverables under the rules `refresh_deliverables` documents.
///
/// A result for a project that is no longer selected is dropped; the next tick
/// fetches the new one.
pub(crate) fn apply_deliverables(state: &mut ProjectCtlState, fetched: DeliverablesFetch) {
    let Some(selected) = state.selected_project_name().map(str::to_string) else {
        state.deliverables = None;
        return;
    };
    if !state.daemon_reachable {
        state.deliverables = None;
        return;
    }
    if fetched.project.as_deref() != Some(selected.as_str()) {
        return;
    }
    if let Some(deliverables) = fetched.deliverables {
        state.deliverables = Some(deliverables);
    }
    sync_open_view_deliverables(state, &selected);
    if let Some(milestones) = fetched.milestones
        && let Some(view) = &mut state.deliverable_view
        && view.project_name == selected
    {
        view.milestones = milestones;
    }
}
