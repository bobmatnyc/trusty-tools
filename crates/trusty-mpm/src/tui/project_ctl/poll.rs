//! Live daemon polling for the `tm projects` TUI (#2118).
//!
//! Why: the run loop repeats one async step on its `--interval-ms` cadence —
//! probe daemon health, self-heal the URL on failure, pull the registry-B
//! project list plus the fleet-by-project session groups, and project both
//! into the pure row shapes [`super::state`] renders. Mirrors
//! `tui::coordinator::poll::coord_poll_daemon` so every TUI screen self-heals
//! the daemon URL identically. Fetching the fleet ONCE per tick (rather than
//! per-project) keeps this to two HTTP calls regardless of project count.
//! What: [`project_ctl_poll_daemon`] (health → rediscover-on-fail → registry
//! list + fleet → map; on daemon-down clears both) plus the pure projections
//! [`project_to_row`] and [`session_to_row`].
//! Test: `tests` covers the pure projections; the async glue mirrors the
//! coordinator's poller and is exercised by launching the TUI.

use crate::client::{DaemonClient, FleetProjectGroupWire, ManagedSessionSummary};
use crate::project::Project;
use crate::tui::coordinator::rows::session_short_id;

use super::state::{ProjectCtlState, ProjectRow, SessionRow};

/// Refresh [`ProjectCtlState`] from one daemon poll.
///
/// Why: keeps the poll logic out of the key-driven run loop so the loop can
/// re-poll on its timer (and, after a mutating action, on demand) without
/// duplicating the health/rediscover/fetch sequence.
/// What: probes health; if the daemon looks down, re-resolves the URL from
/// the lock file via [`rediscover`] and retries one health probe. When
/// reachable it pulls `GET /api/v1/projects` and
/// `GET /api/v1/sessions/managed/fleet`, merges them into `state.projects` /
/// `state.sessions_by_project`; on a transport error or an unreachable daemon
/// it clears both and sets `daemon_reachable = false`. Always re-syncs both
/// navigation models so a shrunk list never leaves a selection past the end;
/// the Sessions-pane selection is PRESERVED across a refresh (only an
/// explicit project switch resets it — see
/// [`ProjectCtlState::on_project_selection_changed`]).
/// Test: `poll_marks_unreachable_clears_state` drives the daemon-down branch;
/// the daemon-up path requires a live daemon and is exercised manually.
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
            Ok((projects, sessions_by_project)) => {
                state.projects = projects;
                state.sessions_by_project = sessions_by_project;
            }
            Err(_) => {
                state.daemon_reachable = false;
                state.projects.clear();
                state.sessions_by_project.clear();
            }
        }
    } else {
        state.projects.clear();
        state.sessions_by_project.clear();
    }
    state.projects_nav.sync_len(state.projects.len());
    state.sessions_nav.sync_len(state.current_sessions().len());
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
/// fleet group) and the `sessions_by_project` map (every fleet group, even a
/// project the registry list omitted — defensive against a transient
/// registry/fleet mismatch).
async fn fetch_projects_and_sessions(
    client: &DaemonClient,
) -> anyhow::Result<(
    Vec<ProjectRow>,
    std::collections::BTreeMap<String, Vec<SessionRow>>,
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

    Ok((rows, sessions_by_project))
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
/// Why: the Sessions pane needs a numbered, compact row per session, plus the
/// static `pending_decision`/`proposed_default` fields the Activity pane
/// skeleton renders (#2118 does NOT call the live `/activity` endpoint — that
/// is #2119).
/// What: derives the 8-hex short id via [`session_short_id`] and copies the
/// rest through unchanged.
/// Test: `session_to_row_derives_short_id_and_copies_fields`.
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
    }
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
}
