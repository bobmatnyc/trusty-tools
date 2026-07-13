//! Live daemon polling for the `tm projects` TUI (#2118, live-refresh + activity
//! wiring #2119).
//!
//! Why: the run loop repeats one async step on its `--interval-ms` cadence —
//! probe daemon health, self-heal the URL on failure, pull the registry-B
//! project list plus the fleet-by-project session groups, fetch the selected
//! session's live activity, and project all three into the pure shapes
//! [`super::state`] renders. Mirrors `tui::coordinator::poll::coord_poll_daemon`
//! so every TUI screen self-heals the daemon URL identically. Fetching the
//! fleet ONCE per tick (rather than per-project) keeps the project/session
//! refresh to two HTTP calls regardless of project count; the activity fetch
//! is a third call, made only when a session is selected (DOC-35 §5.4).
//! What: [`project_ctl_poll_daemon`] (health → rediscover-on-fail → registry
//! list + fleet → map → selected-session activity via [`refresh_activity`] →
//! selected-project Deliverables (+ Milestones when the view is open) via
//! [`refresh_deliverables`], DOC-35 §10.6/§10.8, #2383; on daemon-down clears
//! the project/session state — which in turn clears `activity` too, since a
//! daemon-down poll has no session left selected to attribute it to; see
//! [`refresh_activity`]'s own doc for the narrower "fetch failed but the
//! daemon is otherwise up" case that DOES keep the last-known activity,
//! marked stale). The pure DTO → row projections ([`rows::project_to_row`],
//! [`rows::live_session_rows`], [`rows::session_to_row`],
//! [`rows::activity_from_response`]) live in the [`rows`] submodule (split
//! out by #2476 to keep this file under the 500-SLOC production cap) and are
//! re-exported here so existing callers keep using `poll::project_to_row`
//! etc. A PERSISTENT non-transport `/activity` fetch failure for the
//! selected session (an older daemon lacking the endpoint, a GC'd session, a
//! malformed body) is classified by [`classify_activity_error`] and tracked
//! by [`super::state::ActivityFailureStreak`] so the pane can eventually
//! report it explicitly instead of rendering "loading…" forever (#2470); see
//! [`apply_activity_outcome`] for the pure state-transition rules.
//! Test: `tests` (split into the sibling `tests.rs` file, a recognized
//! test-file path under the 1500-SLOC test cap, to keep THIS file under the
//! 500-SLOC production cap once the #2470 tests were added) covers the
//! daemon-down / stale-keep branches against a guaranteed-dead client
//! (mirroring `tui::coordinator::poll::tests::poll_marks_unreachable_clears_sessions`)
//! plus the failure-classification and failure-streak state machine;
//! [`rows`]'s own `tests` covers the pure projections; the daemon-UP path is
//! exercised manually / by launching the TUI.

mod rows;

pub(crate) use rows::{activity_from_response, live_session_rows, project_to_row};
// Only reachable from `super::tests` (this module's daemon-down test
// fixtures) and `super::super::tests` (the project_ctl-level integration
// tests) — both `#[cfg(test)]`-gated, so a plain `cargo build` never sees a
// caller and would otherwise warn on the re-export.
#[cfg(test)]
pub(crate) use rows::session_to_row;

use crate::client::{DaemonClient, ManagedActivityResponse};
use crate::project::Project;

#[cfg(test)]
use super::state::ActivityInfo;
use super::state::{ProjectCtlState, ProjectRow, SessionRow};

/// Refresh [`ProjectCtlState`] from one daemon poll.
///
/// Why: keeps the poll logic out of the key-driven run loop so the loop can
/// re-poll on its timer (and, after a mutating action, on demand) without
/// duplicating the health/rediscover/fetch sequence. Note this function never
/// touches [`ProjectCtlState::pending_confirm`] — the confirmation gate pins a
/// target session id at the moment it opens (DOC-35 §5.2) and a poll racing
/// with an open confirm modal must not reassign or clear it.
/// What: probes health; if the daemon looks down, re-resolves the URL from
/// the lock file via [`rediscover`] and retries one health probe. When
/// reachable it pulls `GET /api/v1/projects` and
/// `GET /api/v1/sessions/managed/fleet`, merges them into `state.projects` /
/// `state.sessions_by_project`; on a transport error or an unreachable daemon
/// it clears both and sets `daemon_reachable = false`. Always re-syncs both
/// navigation models so a shrunk list never leaves a selection past the end;
/// the Sessions-pane selection is PRESERVED across a refresh (only an
/// explicit project switch resets it — see
/// [`ProjectCtlState::on_project_selection_changed`]). Finally refreshes
/// [`ProjectCtlState::activity`] for whichever session is selected AFTER the
/// project/session refresh above (see [`refresh_activity`]).
/// Test: `poll_marks_unreachable_clears_state` drives the full-poll
/// daemon-down branch; `poll_never_touches_pending_confirm` (in
/// `super::tests`) covers the confirm-gate invariant, `poll_never_closes_an_open_deliverable_view`
/// covers the Deliverable-view invariant, and `poll_doesnt_touch_open_config_form`
/// covers the SAME invariant for the #2120 config form (never reassigned,
/// closed, or have its in-progress unsaved field edits clobbered by a
/// racing poll — the config form has no live daemon-fed content to refresh
/// mid-edit, unlike the Deliverable view, so a poll's only correct action
/// toward an open form is to leave it alone entirely); the daemon-up path
/// requires a live daemon and is exercised manually.
pub(crate) async fn project_ctl_poll_daemon(
    state: &mut ProjectCtlState,
    client: &mut DaemonClient,
) {
    state.daemon_reachable = client.is_healthy().await;
    if rediscover(client, state.daemon_reachable) {
        state.daemon_reachable = client.is_healthy().await;
    }
    if state.daemon_reachable {
        match fetch_projects_and_sessions(client).await {
            Ok((projects, sessions_by_project, projects_full)) => {
                state.projects = projects;
                state.sessions_by_project = sessions_by_project;
                state.projects_full = projects_full;
            }
            Err(_) => {
                state.daemon_reachable = false;
                state.projects.clear();
                state.sessions_by_project.clear();
                state.projects_full.clear();
            }
        }
    } else {
        state.projects.clear();
        state.sessions_by_project.clear();
        state.projects_full.clear();
    }
    state.projects_nav.sync_len(state.projects.len());
    state.sessions_nav.sync_len(state.current_sessions().len());
    refresh_activity(state, client).await;
    refresh_deliverables(state, client).await;
}

/// Refresh [`ProjectCtlState::deliverables`] for the currently selected
/// project, and — while [`ProjectCtlState::deliverable_view`] is open —
/// [`super::state::DeliverableView::milestones`] too (DOC-35 §10.6/§10.8,
/// #2383).
///
/// Why: split out of [`project_ctl_poll_daemon`] for the same reason
/// [`refresh_activity`] is — one focused function per "what does this fetch,
/// under what conditions" concern. This is the poll loop's ONE additional
/// steady-state call (Deliverables for the selected project, mirroring how
/// `refresh_activity` scopes its own fetch to the selected session) plus a
/// SECOND call that only fires while the view is open (Milestones) — bounded
/// by "is a modal open", never by session/project count, so it never becomes
/// an O(n) per-session loop. Runs AFTER the project/session refresh so
/// `state.selected_project_name()` reflects the just-synced navigation.
/// What: no project selected, or the daemon unreachable → resets
/// `state.deliverables` to `None` (Unknown; matching `state.projects`/
/// `sessions_by_project`'s own daemon-down handling in this function — every
/// session row also vanishes from the Sessions pane in that case, so the
/// glyph question is moot). A selected project with the daemon reachable →
/// fetches `list_deliverables`; on success replaces `state.deliverables`
/// with `Some(list)`. **On a transient fetch error, `state.deliverables` is
/// left UNCHANGED** (neither cleared nor reset to `None`) — mirrors
/// [`refresh_activity`]'s stale-keep pattern: a `None` (still-unknown) stays
/// `None`, and a `Some(last-known-good-list)` stays exactly that, so
/// [`ProjectCtlState::deliverable_link_state`] keeps resolving previously-
/// resolved sessions correctly instead of flipping them to a false
/// "dangling" reading on one bad poll (review finding on #2383's initial
/// PR). When [`ProjectCtlState::deliverable_view`] is `Some` for the SAME
/// project, also fetches `list_milestones` and updates its `milestones`
/// field in place (leaving `deliverables` alone —
/// [`ProjectCtlState::open_deliverable_view`] seeded it, and this function's
/// own `state.deliverables` update above keeps it current on every
/// subsequent tick via [`sync_open_view_deliverables`]).
/// Test: `refresh_deliverables_clears_when_no_project_selected`,
/// `refresh_deliverables_clears_on_daemon_down`,
/// `refresh_deliverables_keeps_stale_list_on_transient_fetch_failure`.
async fn refresh_deliverables(state: &mut ProjectCtlState, client: &DaemonClient) {
    let Some(project_name) = state.selected_project_name().map(str::to_string) else {
        state.deliverables = None;
        return;
    };

    if !state.daemon_reachable {
        state.deliverables = None;
        return;
    }

    // A transient `Err` deliberately falls through WITHOUT touching
    // `state.deliverables` — see the doc above for why (stale-keep, not
    // clear, to avoid a false "dangling" glyph on one bad poll).
    if let Ok(deliverables) = client.list_deliverables(&project_name, None).await {
        state.deliverables = Some(deliverables);
    }
    sync_open_view_deliverables(state, &project_name);

    if let Some(view) = &state.deliverable_view
        && view.project_name == project_name
        && let Ok(milestones) = client.list_milestones(&project_name).await
        && let Some(view) = &mut state.deliverable_view
    {
        view.milestones = milestones;
    }
}

/// Keep an open [`super::state::DeliverableView`]'s `deliverables` in sync
/// with [`ProjectCtlState::deliverables`] on every tick, when the view is
/// still scoped to `project_name`.
///
/// Why: [`ProjectCtlState::open_deliverable_view`] seeds the view from
/// whatever `state.deliverables` held at the moment it opened; without this,
/// a status change (e.g. an operator running `tm projects deliverables
/// set-status` in another terminal) would never appear in an already-open
/// view until it was closed and reopened. Only syncs on `Some` — a `None`
/// (Unknown, e.g. this tick's fetch failed) leaves the view's last-known
/// list on screen rather than blanking it, the same stale-keep principle
/// [`refresh_deliverables`] applies to `state.deliverables` itself.
fn sync_open_view_deliverables(state: &mut ProjectCtlState, project_name: &str) {
    let Some(deliverables) = state.deliverables.clone() else {
        return;
    };
    if let Some(view) = &mut state.deliverable_view
        && view.project_name == project_name
    {
        view.deliverables = deliverables;
    }
}

/// Refresh [`ProjectCtlState::activity`] for the currently selected session
/// (DOC-35 §5.4, #2119).
///
/// Why: split out of [`project_ctl_poll_daemon`] so the "no session selected"
/// / "daemon down, keep last known" / "fetch failed, keep last known" /
/// "fetch succeeded" branches are each one arm instead of nested inside the
/// larger poll function. Runs AFTER the project/session refresh above so
/// `state.selected_session()` reflects the just-synced navigation, not a
/// pre-refresh selection that may have shrunk out of range.
/// What: no selection → clears `state.activity` and the failure streak. A
/// daemon-unreachable poll is treated as a [`ActivityFetchFailure::Transport`]
/// outcome (the whole daemon looks down, same as a connect/timeout error);
/// otherwise the fetch's `Result` is classified via
/// [`classify_activity_error`] and handed to [`apply_activity_outcome`],
/// which owns the actual stale-keep / failure-streak / unavailable rules
/// (#2470).
/// Test: `refresh_activity_marks_existing_activity_stale_on_fetch_failure`,
/// `refresh_activity_clears_when_no_session_is_selected`;
/// [`apply_activity_outcome`]'s own doc covers the failure-streak rules.
async fn refresh_activity(state: &mut ProjectCtlState, client: &DaemonClient) {
    let Some(session_id) = state.selected_session().map(|s| s.id.clone()) else {
        state.activity = None;
        state.activity_failures.reset();
        return;
    };

    if !state.daemon_reachable {
        apply_activity_outcome(state, session_id, Err(ActivityFetchFailure::Transport));
        return;
    }

    let outcome = client
        .managed_session_activity(&session_id)
        .await
        .map_err(|e| classify_activity_error(&e));
    apply_activity_outcome(state, session_id, outcome);
}

/// Coarse classification of a `/activity` fetch failure (#2470).
///
/// Why: distinguishes "the whole daemon looks down or is restarting"
/// (transport — connection refused/timeout) from "the daemon answered, but
/// THIS session's activity fetch is itself broken" (non-transport — an HTTP
/// error status or an undecodable body: an older daemon lacking the
/// endpoint, a GC'd session, a malformed payload). Only the latter should
/// ever advance [`super::state::ActivityFailureStreak`] — a transport
/// failure means the daemon itself is unreachable, which the existing
/// stale-keep path already covers on its own, and flapping connectivity must
/// never be mistaken for "this session's activity is permanently gone".
/// What: [`classify_activity_error`] walks the `anyhow::Error` cause chain
/// for a [`reqwest::Error`] whose `is_connect()` or `is_timeout()` is `true`
/// → [`Self::Transport`]; anything else (a status/body error from
/// [`crate::client::http_client::error::response_or_body_error`], a decode
/// error, or no `reqwest::Error` in the chain at all) → [`Self::NonTransport`].
/// Test: `classify_status_error_is_non_transport`,
/// `classify_connect_error_is_transport` (in `tests`, below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityFetchFailure {
    /// Connection refused/timeout — the whole daemon looks down.
    Transport,
    /// The daemon answered; this session's activity fetch failed anyway.
    NonTransport,
}

/// See [`ActivityFetchFailure`]'s own doc.
fn classify_activity_error(err: &anyhow::Error) -> ActivityFetchFailure {
    let is_transport = err
        .chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(|e| e.is_connect() || e.is_timeout());
    if is_transport {
        ActivityFetchFailure::Transport
    } else {
        ActivityFetchFailure::NonTransport
    }
}

/// Apply one `/activity` fetch's outcome to `state.activity` and
/// `state.activity_failures` (#2470).
///
/// Why: pulled out of [`refresh_activity`] as a pure, synchronously testable
/// function — no daemon, no async, no `reqwest` types in its signature — so
/// the exact state-transition rules (stale-keep vs. reset vs. crossing the
/// unavailable threshold) are unit-testable with typed inputs, mirroring how
/// [`rows::activity_from_response`] keeps the pure DTO projection out of the
/// async fetch.
/// What: `Ok(resp)` → replaces `state.activity` with a fresh, non-stale
/// [`ActivityInfo`] and resets the failure streak
/// ([`super::state::ActivityFailureStreak::reset`]).
/// `Err(ActivityFetchFailure::NonTransport)` → records one failure in the
/// streak for `session_id` ([`super::state::ActivityFailureStreak::record_failure`]);
/// once that streak crosses [`super::state::ACTIVITY_UNAVAILABLE_THRESHOLD`],
/// [`ProjectCtlState::activity_unavailable_for_selected`] flips to `true`
/// and the Activity pane renders "unavailable" instead of "loading…"/stale
/// data. `Err(ActivityFetchFailure::Transport)` → never touches the streak.
/// Both `Err` arms then apply the SAME stale-keep-or-clear rule to
/// `state.activity` itself: mark `stale = true` if it already targets
/// `session_id`, else clear it (nothing recent enough to show as stale) —
/// unchanged from the pre-#2470 behavior.
/// Test: `apply_activity_outcome_success_resets_streak_and_shows_data`,
/// `apply_activity_outcome_non_transport_failures_reach_unavailable_threshold`,
/// `apply_activity_outcome_transport_failure_never_advances_streak`,
/// `apply_activity_outcome_success_after_failures_resets_streak`,
/// `apply_activity_outcome_session_switch_starts_a_fresh_streak`.
fn apply_activity_outcome(
    state: &mut ProjectCtlState,
    session_id: String,
    outcome: Result<ManagedActivityResponse, ActivityFetchFailure>,
) {
    match outcome {
        Ok(resp) => {
            state.activity = Some(activity_from_response(session_id, resp));
            state.activity_failures.reset();
            return;
        }
        Err(ActivityFetchFailure::NonTransport) => {
            state.activity_failures.record_failure(&session_id);
        }
        Err(ActivityFetchFailure::Transport) => { /* never touches the streak */ }
    }

    match &mut state.activity {
        Some(existing) if existing.session_id == session_id => existing.stale = true,
        _ => state.activity = None,
    }
}

/// Re-resolve the daemon URL from the lock file when the daemon is unreachable.
///
/// Why: [`DaemonClient`] is built once at startup; if the daemon later
/// restarted onto a fresh ephemeral port the client would stay pinned to a
/// stale address forever. Mirrors `tui::coordinator::poll::rediscover`.
/// What: when `reachable` is `false`, re-resolves via
/// [`crate::core::resolve_daemon_url`] and, if it differs from the client's
/// current URL, re-points the client and returns `true` so the caller retries
/// one health probe.
fn rediscover(client: &mut DaemonClient, reachable: bool) -> bool {
    if reachable {
        return false;
    }
    let resolved = crate::core::resolve_daemon_url(None);
    if resolved != client.base_url() {
        client.set_base_url(resolved);
        true
    } else {
        false
    }
}

/// Fetch the registry project list and the fleet session groups, merged.
///
/// Why: one place owns the two-call fetch + merge so [`project_ctl_poll_daemon`]
/// stays a thin health/rediscover wrapper.
/// What: GETs `registry_list_projects` and `fleet_managed_sessions`, then
/// builds the `Vec<ProjectRow>` (registry order, counts from the matching
/// fleet group), the `sessions_by_project` map (every fleet group, even a
/// project the registry list omitted — defensive against a transient
/// registry/fleet mismatch) via [`live_session_rows`] (which drops
/// decommissioned sessions, #2476), and a `name -> Project` map of the FULL
/// records `registry_list_projects` already returned (DOC-35 §6, #2120) —
/// the config form needs the full record to seed its baseline values;
/// retaining it here (rather than re-fetching per-project when the form
/// opens) costs nothing, since `projects` was already in hand before being
/// projected down to `ProjectRow`.
async fn fetch_projects_and_sessions(
    client: &DaemonClient,
) -> anyhow::Result<(
    Vec<ProjectRow>,
    std::collections::BTreeMap<String, Vec<SessionRow>>,
    std::collections::BTreeMap<String, Project>,
)> {
    let projects = client.registry_list_projects(None).await?;
    let groups = client.fleet_managed_sessions().await?;

    let mut sessions_by_project = std::collections::BTreeMap::new();
    for group in &groups {
        let rows = live_session_rows(group.sessions.clone());
        sessions_by_project.insert(group.project_name.clone(), rows);
    }

    let rows = projects
        .iter()
        .map(|p| {
            let group = groups.iter().find(|g| g.project_name == p.name);
            project_to_row(p, group)
        })
        .collect();

    let projects_full = projects.into_iter().map(|p| (p.name.clone(), p)).collect();

    Ok((rows, sessions_by_project, projects_full))
}

#[cfg(test)]
mod tests;
