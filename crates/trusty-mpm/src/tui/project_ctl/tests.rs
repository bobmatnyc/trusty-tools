//! Cross-module integration tests for the `tm projects` TUI skeleton (#2118).
//!
//! Why: each submodule (`state`, `poll`, `events`, `panes::*`) unit-tests its
//! own pure logic in isolation; this file instead wires them together the way
//! the real run loop does — poll projections feeding state, then key events
//! reading that state — catching seams the per-module tests cannot (mirrors
//! `tui::coordinator::tests` / `tui::health::tests`).
//! What: covers the full poll → state → key-dispatch → pane-render pipeline
//! with synthetic daemon data (no network, no terminal).
//! Test: this IS the test module.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::events::{PendingAction, handle_key};
use super::panes::{actions_bar, projects, sessions};
use super::poll::{project_ctl_poll_daemon, project_to_row, session_to_row};
use super::state::{ConfirmKind, DeliverableLinkState, Pane, ProjectCtlState};
use crate::client::{DaemonClient, FleetProjectGroupWire, ManagedSessionSummary};
use crate::project::Project;

fn project(name: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: format!("https://github.com/acme/{name}"),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

fn summary(id: &str, state: &str) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: id.to_string(),
        name: format!("s-{id}"),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: Some("main".to_string()),
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: Some("ship it".to_string()),
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot: 0,
        deleted: false,
        auto_resume_parked: None,
    }
}

/// Build a state that mirrors what `project_ctl_poll_daemon` would produce
/// from two projects, one of which has a live session — without a network
/// call.
fn seeded_state() -> ProjectCtlState {
    let trusty_tools = project("trusty-tools");
    let genealogy = project("genealogy");
    let group = FleetProjectGroupWire {
        project_name: "trusty-tools".to_string(),
        repo_url: trusty_tools.repo_url.clone(),
        sessions: vec![summary("a1b2c3d4e5f6", "active")],
    };

    let mut state = ProjectCtlState {
        projects: vec![
            project_to_row(&trusty_tools, Some(&group)),
            project_to_row(&genealogy, None),
        ],
        projects_full: [
            ("trusty-tools".to_string(), trusty_tools.clone()),
            ("genealogy".to_string(), genealogy.clone()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    state.sessions_by_project.insert(
        "trusty-tools".to_string(),
        group.sessions.into_iter().map(session_to_row).collect(),
    );
    state.projects_nav.sync_len(state.projects.len());
    state.sessions_nav.sync_len(state.current_sessions().len());
    state
}

#[test]
fn seeded_state_renders_live_project_and_its_session() {
    let state = seeded_state();
    assert_eq!(state.projects.len(), 2);
    assert_eq!(state.selected_project().unwrap().name, "trusty-tools");
    assert_eq!(state.current_sessions().len(), 1);

    let projects_line = projects::project_line(&state.projects[0]);
    let text: String = projects_line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        text.starts_with("●1"),
        "expected a live glyph+count: {text}"
    );

    let session_line = sessions::session_line(
        1,
        &state.current_sessions()[0],
        DeliverableLinkState::Unknown,
    );
    let text: String = session_line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(text.contains("a1b2c3d4"), "missing short id: {text}");
    assert!(text.contains("ship it"), "missing task: {text}");
}

#[test]
fn drilling_in_then_killing_an_active_session_requires_confirmation() {
    // The seeded session is "active", so DOC-35 §5.2's confirm gate applies:
    // `k` alone must not fire — it takes drill-in, `k`, THEN `y` to reach the
    // real PendingAction.
    let mut state = seeded_state();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert!(handle_key(&mut state, enter).is_none());
    assert_eq!(state.focus, Pane::Sessions);

    let kill = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
    assert!(
        handle_key(&mut state, kill).is_none(),
        "kill on an Active session must open the confirm gate, not fire"
    );
    assert!(state.pending_confirm.is_some());

    let confirm = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    let action = handle_key(&mut state, confirm);
    assert_eq!(
        action,
        Some(PendingAction::Kill("a1b2c3d4e5f6".to_string()))
    );
    assert!(state.pending_confirm.is_none());
}

#[test]
fn switching_to_the_empty_project_clears_the_sessions_pane() {
    let mut state = seeded_state();
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down);
    assert_eq!(state.selected_project().unwrap().name, "genealogy");
    assert!(state.current_sessions().is_empty());
    assert!(state.selected_session().is_none());
}

#[test]
fn action_bar_reflects_daemon_reachability_by_default() {
    let state = seeded_state();
    // Freshly seeded state (no poll ran) starts unreachable, matching the
    // real poller's default until the first health probe succeeds.
    assert!(actions_bar::bar_text(&state).contains(actions_bar::DAEMON_UNREACHABLE));
}

// #4306/#4415: the dead-address fixture used to be a local bind-an-ephemeral-
// port-then-drop-it helper, whose "nothing can be listening here" premise
// expired the moment the OS re-handed that port to another binder. All three
// former copies now share ONE fixture whose guarantee does not depend on
// winning a port race — see `crate::test_support::dead_loopback_url`.
use crate::test_support::dead_loopback_url;

/// A daemon poll racing with an open confirmation gate must never reassign or
/// clear it (DOC-35 §5.2) — the gate pins its target session id at the moment
/// it opens, and a refresh (even one that clears the whole fleet, as a
/// daemon-down poll does) must leave that pin untouched. This is the
/// regression test the #2119 task explicitly calls for on any change that
/// touches the refresh path.
#[tokio::test]
async fn poll_never_touches_pending_confirm() {
    let mut state = seeded_state();
    state.request_confirm(
        ConfirmKind::Decommission,
        "a1b2c3d4e5f6",
        "trusty-tools-session",
    );
    let before = state.pending_confirm.clone().expect("confirm requested");

    let mut client = DaemonClient::new(dead_loopback_url());
    project_ctl_poll_daemon(&mut state, &mut client).await;

    assert_eq!(
        state.pending_confirm,
        Some(before),
        "a poll must never mutate an open confirmation gate's pinned target"
    );
}

/// A daemon poll racing with an open Deliverable/Milestone view must never
/// close it (DOC-35 §10.8, #2383) — mirrors `poll_never_touches_pending_confirm`
/// for the second modal this screen now has. A daemon-down poll MAY leave the
/// view's cached `deliverables`/`milestones` stale (there is no live
/// connection to refresh them from), but must not clear `deliverable_view`
/// itself out from under the operator.
#[tokio::test]
async fn poll_never_closes_an_open_deliverable_view() {
    let mut state = seeded_state();
    state.open_deliverable_view("trusty-tools");
    assert!(state.deliverable_view.is_some());

    let mut client = DaemonClient::new(dead_loopback_url());
    project_ctl_poll_daemon(&mut state, &mut client).await;

    assert_eq!(
        state
            .deliverable_view
            .as_ref()
            .map(|v| v.project_name.as_str()),
        Some("trusty-tools"),
        "a poll must never close an open Deliverable/Milestone view"
    );
}

/// A daemon poll racing with an open config form must never touch it (DOC-35
/// §6, #2120) — the THIRD modal this invariant now covers, mirroring
/// `poll_never_touches_pending_confirm` (which pins by `session_id`) and
/// `poll_never_closes_an_open_deliverable_view` (which pins by
/// `project_name`). Pinned here by `project_name`, same as the Deliverable
/// view. Unlike the Deliverable view, the config form has no live daemon-fed
/// content to refresh mid-edit (`poll.rs` never touches `state.config_form`
/// at all, by construction — see `project_ctl_poll_daemon`'s doc), so this
/// test ALSO asserts the stronger claim the task calls for: an in-progress,
/// UNSAVED field edit survives a poll byte-for-byte, not just "the form
/// stays open."
#[tokio::test]
async fn poll_doesnt_touch_open_config_form() {
    let mut state = seeded_state();
    state.open_config_form("trusty-tools");
    state.config_form.as_mut().unwrap().description.value = "unsaved edit in progress".to_string();
    let before = state.config_form.clone().expect("form requested");

    let mut client = DaemonClient::new(dead_loopback_url());
    project_ctl_poll_daemon(&mut state, &mut client).await;

    assert_eq!(
        state.config_form,
        Some(before),
        "a poll must never mutate an open config form — not even its \
         unsaved, not-yet-submitted field edits"
    );
}

/// #5913: the `[d]` decommission hotkey must leave the base repo's worktree
/// bookkeeping clean, exactly as the CLI's routed verb and the bulk prune sweep
/// already do.
///
/// This screen reached the decommission endpoint directly, so it never ran the
/// bookkeeping repair — the same divergence #5913 closed for the other two
/// callers, still open on the one hotkey an operator is most likely to press.
/// Against the pre-fix `client.decommission_managed_session(&id)` this fails:
/// the workspace is gone and the stale `git worktree list` entry is still there.
///
/// A stub daemon rather than the real one, because the assertion is about what
/// the CLIENT does with the response: the fixture removes the workspace itself
/// (standing in for the daemon's `remove_dir_all`) and the stub reports that
/// removal with the path, which is the response shape that selects the prune.
#[tokio::test]
async fn project_ctl_decommission_prunes_stale_worktree_bookkeeping() {
    use std::future::IntoFuture;

    /// Run `git` in `dir`, failing the test on a nonzero exit.
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to start: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let base = tmp.path().join("tm-5913-base");
    std::fs::create_dir_all(&base).expect("create base repo dir");
    git(&base, &["init", "--quiet"]);
    git(&base, &["config", "user.email", "tm@example.invalid"]);
    git(&base, &["config", "user.name", "tm test"]);
    std::fs::write(base.join("README.md"), "seed\n").unwrap();
    git(&base, &["add", "README.md"]);
    git(&base, &["commit", "--quiet", "-m", "seed"]);

    let id = "11111111-2222-3333-4444-555555555555";
    let leaf = "tm-5913-project-ctl";
    let ws = base.join(".worktrees").join(leaf);
    git(
        &base,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &format!("session/{leaf}"),
            &ws.to_string_lossy(),
        ],
    );
    assert!(
        git(&base, &["worktree", "list", "--porcelain"]).contains(leaf),
        "fixture invariant: the worktree must be registered before decommission"
    );
    // Stand in for the daemon's `remove_dir_all` of an owned workspace: the
    // directory is gone and git's bookkeeping does not know it.
    std::fs::remove_dir_all(&ws).expect("remove the workspace");

    let outcome = serde_json::json!({
        "id": id,
        "name": leaf,
        "state": "decommissioned",
        "workspace_removed": true,
        "workspace_path_was": ws.to_string_lossy(),
    });
    let app = axum::Router::new().route(
        "/api/v1/sessions/managed/{id}/decommission",
        axum::routing::post(move || std::future::ready(axum::Json(outcome.clone()))),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());

    let client = DaemonClient::new(format!("http://{addr}"));
    let mut state = seeded_state();
    super::actions::dispatch(
        &mut state,
        &client,
        PendingAction::Decommission(id.to_string()),
    )
    .await;

    assert_eq!(
        state.notice.as_deref(),
        Some(format!("decommissioned {leaf} — now decommissioned").as_str()),
        "the hotkey must still report the state change"
    );
    assert!(
        !git(&base, &["worktree", "list", "--porcelain"]).contains(leaf),
        "the TUI's [d] decommission left a stale worktree entry for {leaf} — it \
         is not routed through the shared implementation"
    );
}
