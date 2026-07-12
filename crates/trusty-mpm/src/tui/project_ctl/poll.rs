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
//! marked stale) plus the pure projections [`project_to_row`],
//! [`session_to_row`], and [`activity_from_response`].
//! Test: `tests` covers the pure projections and the daemon-down branches
//! against a guaranteed-dead client (mirroring
//! `tui::coordinator::poll::tests::poll_marks_unreachable_clears_sessions`);
//! the daemon-UP path is exercised manually / by launching the TUI.

use crate::client::{
    DaemonClient, FleetProjectGroupWire, ManagedActivityResponse, ManagedSessionSummary,
};
use crate::project::Project;
use crate::tui::coordinator::rows::session_short_id;

use super::state::{ActivityInfo, ProjectCtlState, ProjectRow, SessionRow};

/// How many trailing lines of a session's raw tmux pane the Activity pane
/// previews (DOC-35 §5.1 mockup: "last 3 lines of raw_pane").
const RAW_PANE_TAIL_LINES: usize = 3;

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
/// What: no selection → clears `state.activity`. A selection with the daemon
/// unreachable, or whose fetch errors, → if `state.activity` already targets
/// this session id it is marked `stale` (kept, not discarded, per the
/// graceful-daemon-down requirement); otherwise cleared (nothing recent
/// enough to show as stale). A successful fetch replaces `state.activity`
/// outright with a fresh, non-stale [`ActivityInfo`].
/// Test: `refresh_activity_marks_existing_activity_stale_on_fetch_failure`,
/// `refresh_activity_clears_when_no_session_is_selected`.
async fn refresh_activity(state: &mut ProjectCtlState, client: &DaemonClient) {
    let Some(session_id) = state.selected_session().map(|s| s.id.clone()) else {
        state.activity = None;
        return;
    };

    if state.daemon_reachable {
        match client.managed_session_activity(&session_id).await {
            Ok(resp) => {
                state.activity = Some(activity_from_response(session_id, resp));
                return;
            }
            Err(_) => { /* fall through to the stale-or-clear handling below */ }
        }
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
/// registry/fleet mismatch), and a `name -> Project` map of the FULL records
/// `registry_list_projects` already returned (DOC-35 §6, #2120) — the config
/// form needs the full record to seed its baseline values; retaining it here
/// (rather than re-fetching per-project when the form opens) costs nothing,
/// since `projects` was already in hand before being projected down to
/// `ProjectRow`.
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
        let rows = group.sessions.iter().cloned().map(session_to_row).collect();
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

/// Project one registry [`Project`] (plus its matching fleet group, if any)
/// into a [`ProjectRow`].
///
/// Why: the Projects pane needs the aggregate-state glyph + live session
/// count (DOC-35 §5), not the raw registry/fleet DTOs.
/// What: `live_count` is the number of sessions in `group` whose `state` is
/// `"active"` or `"provisioning"`; `total_count` is `group.sessions.len()`.
/// A project with no matching fleet group (never spawned a session) gets
/// `0`/`0`.
/// Test: `project_to_row_counts_live_and_total`,
/// `project_to_row_missing_group_is_zeroed`.
pub(crate) fn project_to_row(p: &Project, group: Option<&FleetProjectGroupWire>) -> ProjectRow {
    let (live_count, total_count) = match group {
        Some(g) => (
            g.sessions
                .iter()
                .filter(|s| matches!(s.state.as_str(), "active" | "provisioning"))
                .count(),
            g.sessions.len(),
        ),
        None => (0, 0),
    };
    ProjectRow {
        name: p.name.clone(),
        repo_url: p.repo_url.clone(),
        live_count,
        total_count,
    }
}

/// Project one fleet [`ManagedSessionSummary`] into a [`SessionRow`].
///
/// Why: the Sessions pane needs a numbered, compact row per session; the
/// fleet poll is one call for every session regardless of count, so this
/// projection stays over the STATIC record fields even after #2119 — only the
/// one selected session's activity gets the extra live `/activity` fetch (see
/// [`refresh_activity`]), never the whole list.
/// What: derives the 8-hex short id via [`session_short_id`] and copies the
/// rest through unchanged, including `deliverable_id` (DOC-35 §10.6, #2383 —
/// drives the Sessions-pane deliverable glyph).
/// Test: `session_to_row_derives_short_id_and_copies_fields`,
/// `session_to_row_carries_deliverable_id`.
pub(crate) fn session_to_row(s: ManagedSessionSummary) -> SessionRow {
    let short_id = session_short_id(&s.id);
    SessionRow {
        id: s.id,
        short_id,
        name: s.name,
        branch: s.branch,
        task: s.task,
        state: s.state,
        pending_decision: s.pending_decision,
        proposed_default: s.proposed_default,
        deliverable_id: s.deliverable_id,
    }
}

/// Project a [`ManagedActivityResponse`] into an [`ActivityInfo`] for `session_id`.
///
/// Why: the Activity pane needs a lean, render-ready shape (DOC-35 §5.4) with
/// a bounded pane-tail preview rather than the full wire response.
/// What: copies `state`/`summary`/`pending_decision`/`proposed_default`
/// through unchanged, tags the snapshot with `session_id`, sets `stale` to
/// `false` (a just-succeeded fetch is by definition fresh), and derives
/// `raw_pane_tail` as the last [`RAW_PANE_TAIL_LINES`] non-empty lines of
/// `raw_pane` via [`tail_lines`].
/// Test: `activity_from_response_derives_fields_and_tail`,
/// `activity_from_response_handles_short_pane`.
pub(crate) fn activity_from_response(
    session_id: String,
    resp: ManagedActivityResponse,
) -> ActivityInfo {
    ActivityInfo {
        session_id,
        state: resp.state,
        summary: resp.summary,
        pending_decision: resp.pending_decision,
        proposed_default: resp.proposed_default,
        raw_pane_tail: tail_lines(&resp.raw_pane, RAW_PANE_TAIL_LINES),
        stale: false,
    }
}

/// The last `n` non-empty, trimmed lines of `text`, in original order.
///
/// Why: a raw tmux pane capture is typically newline-padded and much longer
/// than the Activity pane's fixed 4-row height can show; a small pure helper
/// keeps the "which lines" rule unit-testable independent of rendering.
/// What: splits on `\n`, trims each line, drops empty ones, then keeps the
/// last `n` (or fewer, if `text` has fewer than `n` non-empty lines).
/// Test: `tail_lines_keeps_last_n_non_empty_lines`,
/// `tail_lines_handles_fewer_lines_than_requested`.
fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let non_empty: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = non_empty.len().saturating_sub(n);
    non_empty[start..].iter().map(|l| l.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> Project {
        Project {
            name: name.to_string(),
            repo_url: format!("https://github.com/acme/{name}"),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        }
    }

    fn summary(id: &str, state: &str) -> ManagedSessionSummary {
        ManagedSessionSummary {
            id: id.to_string(),
            name: format!("s-{id}"),
            state: state.to_string(),
            workspace_path: None,
            repo_url: None,
            branch: Some("main".to_string()),
            created_at: None,
            last_activity_at: None,
            pending_decision: None,
            proposed_default: None,
            source_id: None,
            task: Some("do the thing".to_string()),
            cwd: None,
            claude_session_id: None,
            deliverable_id: None,
            pane_id: None,
        }
    }

    #[test]
    fn project_to_row_counts_live_and_total() {
        let p = project("widget");
        let group = FleetProjectGroupWire {
            project_name: "widget".to_string(),
            repo_url: p.repo_url.clone(),
            sessions: vec![
                summary("a1111111", "active"),
                summary("b2222222", "provisioning"),
                summary("c3333333", "stopped"),
            ],
        };
        let row = project_to_row(&p, Some(&group));
        assert_eq!(row.live_count, 2);
        assert_eq!(row.total_count, 3);
        assert_eq!(row.name, "widget");
    }

    #[test]
    fn project_to_row_missing_group_is_zeroed() {
        let p = project("widget");
        let row = project_to_row(&p, None);
        assert_eq!(row.live_count, 0);
        assert_eq!(row.total_count, 0);
    }

    #[test]
    fn session_to_row_derives_short_id_and_copies_fields() {
        let row = session_to_row(summary("4f9ca1b2ffff", "active"));
        assert_eq!(row.short_id, "4f9ca1b2");
        assert_eq!(row.id, "4f9ca1b2ffff");
        assert_eq!(row.state, "active");
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(row.task.as_deref(), Some("do the thing"));
    }

    #[test]
    fn session_to_row_carries_deliverable_id() {
        let mut s = summary("4f9ca1b2ffff", "active");
        s.deliverable_id = Some("11111111-1111-1111-1111-111111111111".to_string());
        let row = session_to_row(s);
        assert_eq!(
            row.deliverable_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );

        let none_row = session_to_row(summary("aaaaaaaabbbb", "active"));
        assert!(none_row.deliverable_id.is_none());
    }

    fn activity_response(raw_pane: &str) -> ManagedActivityResponse {
        ManagedActivityResponse {
            raw_pane: raw_pane.to_string(),
            runtime_active: true,
            pane_stale: false,
            state: "working".to_string(),
            summary: "running tests".to_string(),
            confidence: 0.9,
            cache_hit: false,
            input_tokens: 0,
            output_tokens: 0,
            latency_ms: 12,
            total_input_tokens: 0,
            total_output_tokens: 0,
            classification: None,
            pending_decision: Some("write to ci.yml?".to_string()),
            proposed_default: Some("yes".to_string()),
        }
    }

    #[test]
    fn activity_from_response_derives_fields_and_tail() {
        let resp = activity_response("line1\nline2\nline3\nline4\n");
        let info = activity_from_response("sess-1".to_string(), resp);
        assert_eq!(info.session_id, "sess-1");
        assert_eq!(info.state, "working");
        assert_eq!(info.summary, "running tests");
        assert_eq!(info.pending_decision.as_deref(), Some("write to ci.yml?"));
        assert_eq!(info.proposed_default.as_deref(), Some("yes"));
        assert_eq!(info.raw_pane_tail, vec!["line2", "line3", "line4"]);
        assert!(!info.stale);
    }

    #[test]
    fn activity_from_response_handles_short_pane() {
        let resp = activity_response("only one line");
        let info = activity_from_response("sess-1".to_string(), resp);
        assert_eq!(info.raw_pane_tail, vec!["only one line"]);
    }

    #[test]
    fn tail_lines_keeps_last_n_non_empty_lines() {
        let text = "a\nb\n\nc\nd\n";
        assert_eq!(tail_lines(text, 2), vec!["c", "d"]);
    }

    #[test]
    fn tail_lines_handles_fewer_lines_than_requested() {
        assert_eq!(tail_lines("only", 3), vec!["only"]);
        assert_eq!(tail_lines("", 3), Vec::<String>::new());
    }

    // ---- daemon-down branches (guaranteed-dead client, no live daemon needed) ---

    /// A `127.0.0.1` URL bound to an ephemeral port, then immediately dropped
    /// so a later connect is refused — mirrors
    /// `tui::coordinator::tests::dead_loopback_url`.
    fn dead_loopback_url() -> String {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
        let port = listener.local_addr().expect("read bound local addr").port();
        drop(listener);
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn poll_marks_unreachable_clears_state() {
        let discovered = crate::core::resolve_daemon_url(None);
        let probe = DaemonClient::new(discovered.clone());
        if probe.is_healthy().await {
            eprintln!("skipping: a reachable daemon was discovered at {discovered}");
            return;
        }

        let mut state = ProjectCtlState {
            projects: vec![ProjectRow {
                name: "widget".to_string(),
                repo_url: "https://github.com/acme/widget".to_string(),
                live_count: 1,
                total_count: 1,
            }],
            daemon_reachable: true, // pretend a prior poll had succeeded
            ..Default::default()
        };
        state
            .sessions_by_project
            .insert("widget".to_string(), vec![]);
        let mut client = DaemonClient::new(dead_loopback_url());

        project_ctl_poll_daemon(&mut state, &mut client).await;

        assert!(!state.daemon_reachable);
        assert!(state.projects.is_empty());
        assert!(state.sessions_by_project.is_empty());
    }

    fn seeded_activity_state(session_id: &str) -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![ProjectRow {
                name: "widget".to_string(),
                repo_url: "https://github.com/acme/widget".to_string(),
                live_count: 1,
                total_count: 1,
            }],
            daemon_reachable: true,
            ..Default::default()
        };
        state.sessions_by_project.insert(
            "widget".to_string(),
            vec![session_to_row(summary(session_id, "active"))],
        );
        state.projects_nav.sync_len(state.projects.len());
        state.sessions_nav.sync_len(state.current_sessions().len());
        state
    }

    /// `refresh_activity` is called directly (bypassing `project_ctl_poll_daemon`'s
    /// own health probe) so this test stays deterministic regardless of whether
    /// a real daemon happens to be discoverable on this machine — it only needs
    /// the `/activity` HTTP call itself to fail, which a dead loopback port
    /// guarantees.
    #[tokio::test]
    async fn refresh_activity_marks_existing_activity_stale_on_fetch_failure() {
        let mut state = seeded_activity_state("s1");
        state.activity = Some(ActivityInfo {
            session_id: "s1".to_string(),
            state: "working".to_string(),
            summary: "last known summary".to_string(),
            pending_decision: None,
            proposed_default: None,
            raw_pane_tail: vec!["$ cargo test".to_string()],
            stale: false,
        });
        let client = DaemonClient::new(dead_loopback_url());

        refresh_activity(&mut state, &client).await;

        let activity = state
            .activity
            .expect("last-known activity must be kept, not discarded");
        assert!(activity.stale, "a failed fetch must mark it stale");
        assert_eq!(
            activity.summary, "last known summary",
            "the last-known data must be kept, not cleared"
        );
    }

    #[tokio::test]
    async fn refresh_activity_clears_when_no_session_is_selected() {
        let mut state = ProjectCtlState {
            daemon_reachable: true,
            activity: Some(ActivityInfo {
                session_id: "orphaned".to_string(),
                state: "working".to_string(),
                summary: "stale from a since-deselected session".to_string(),
                pending_decision: None,
                proposed_default: None,
                raw_pane_tail: vec![],
                stale: false,
            }),
            ..Default::default()
        };
        let client = DaemonClient::new(dead_loopback_url());

        refresh_activity(&mut state, &client).await;

        assert!(state.activity.is_none());
    }

    #[tokio::test]
    async fn refresh_deliverables_clears_when_no_project_selected() {
        let mut state = ProjectCtlState {
            daemon_reachable: true,
            deliverables: Some(vec![]),
            ..Default::default()
        };
        let client = DaemonClient::new(dead_loopback_url());

        refresh_deliverables(&mut state, &client).await;

        assert!(state.deliverables.is_none());
    }

    #[tokio::test]
    async fn refresh_deliverables_clears_on_daemon_down() {
        let mut state = seeded_activity_state("s1");
        state.daemon_reachable = false;
        let client = DaemonClient::new(dead_loopback_url());

        refresh_deliverables(&mut state, &client).await;

        assert!(
            state.deliverables.is_none(),
            "a project IS selected here, but daemon_reachable=false must still reset to \
             Unknown, matching state.projects/sessions_by_project's own daemon-down handling"
        );
    }

    /// THE review-required regression test: `daemon_reachable == true` (the
    /// daemon is otherwise healthy — the earlier health probe succeeded) but
    /// `list_deliverables` itself fails on this one tick. Previously this
    /// cleared `state.deliverables` outright, which made
    /// `deliverable_link_state` report every previously-resolved session as
    /// `Dangling` — a false "the Deliverable was deleted" signal for what was
    /// really just one dropped HTTP call. The fix: keep the last-known-good
    /// `Some(list)` untouched, mirroring `refresh_activity`'s stale-keep
    /// pattern for `ActivityInfo`.
    #[tokio::test]
    async fn refresh_deliverables_keeps_stale_list_on_transient_fetch_failure() {
        let mut state = seeded_activity_state("s1");
        let known_id = crate::deliverable::DeliverableId::new();
        state.deliverables = Some(vec![crate::deliverable::Deliverable {
            id: known_id,
            project_name: "widget".to_string(),
            name: "OAuth2 flow".to_string(),
            description: String::new(),
            kind: crate::deliverable::DeliverableKind::Feature,
            ticket_ref: None,
            spec_ref: None,
            status: crate::deliverable::DeliverableStatus::InProgress,
            estimated_effort: crate::deliverable::EstimationTier::M,
            created_at: chrono::Utc::now(),
            target_date: None,
        }]);
        // `seeded_activity_state` leaves `daemon_reachable = true` — the
        // failure here is scoped to `list_deliverables` alone, via a dead
        // loopback client, exactly the "endpoint fails, daemon otherwise up"
        // case the review flagged.
        let client = DaemonClient::new(dead_loopback_url());

        refresh_deliverables(&mut state, &client).await;

        let kept = state
            .deliverables
            .as_ref()
            .expect("a transient fetch failure must KEEP the last-known-good list, not clear it");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, known_id);
        assert_eq!(
            state.deliverable_link_state(&known_id.to_string()),
            crate::tui::project_ctl::state::DeliverableLinkState::Resolved,
            "a previously-resolved link must stay Resolved through one bad poll, \
             never flip to Dangling"
        );
    }
}
