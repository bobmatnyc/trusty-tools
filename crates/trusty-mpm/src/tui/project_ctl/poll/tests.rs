//! Unit tests for [`super`] (`poll::mod`) — split into a `tests.rs` file
//! (recognized test-file path, 1500-SLOC cap) so the production poll logic
//! in `mod.rs` stays under the 500-SLOC production cap once the #2470
//! failure-streak tests were added.
//!
//! Why: `mod.rs` shares its private items via `use super::*;`, so this file
//! is a plain extraction (behavior unchanged) — see git history for the
//! pre-split version. Split out during #2470's fix rather than at the
//! (already exercised, #2476) `rows` split point, since these tests are
//! specific to `refresh_activity`/`refresh_deliverables`/`project_ctl_poll_daemon`,
//! not the pure row projections `rows.rs` covers.
//! What: daemon-down / stale-keep branches against a guaranteed-dead client
//! (mirroring `tui::coordinator::poll::tests::poll_marks_unreachable_clears_sessions`),
//! plus the #2470 failure-classification and failure-streak state-machine
//! tests.
//! Test: this *is* the test module.

use super::*;
use crate::client::ManagedSessionSummary;

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
        injection_status: None,
        unresumable: false,
    }
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

// ---- #2470: classification + the pure failure-streak state machine ----

fn activity_response(state: &str, summary: &str) -> ManagedActivityResponse {
    ManagedActivityResponse {
        raw_pane: String::new(),
        runtime_active: true,
        pane_stale: false,
        state: state.to_string(),
        summary: summary.to_string(),
        confidence: 0.9,
        cache_hit: false,
        input_tokens: 0,
        output_tokens: 0,
        latency_ms: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        classification: None,
        pending_decision: None,
        proposed_default: None,
    }
}

/// Mirrors the plain `anyhow` message
/// [`crate::client::http_client::error::response_or_body_error`] bails
/// with on a non-success status — no `reqwest::Error` in the chain — so
/// it must classify as non-transport (404, malformed body, etc. all take
/// this shape).
#[test]
fn classify_status_error_is_non_transport() {
    let err = anyhow::anyhow!("404 Not Found: session not found");
    assert_eq!(
        classify_activity_error(&err),
        ActivityFetchFailure::NonTransport
    );
}

/// A genuine connection-refused `reqwest::Error` (same dead-loopback
/// fixture the daemon-down tests above use) must classify as transport.
#[tokio::test]
async fn classify_connect_error_is_transport() {
    let client = DaemonClient::new(dead_loopback_url());
    let err = client
        .managed_session_activity("s1")
        .await
        .expect_err("a dead loopback port must fail the request");
    assert_eq!(
        classify_activity_error(&err),
        ActivityFetchFailure::Transport
    );
}

/// (a) N consecutive non-transport ("404-class") failures for the same
/// session must eventually flip [`ProjectCtlState::activity_unavailable_for_selected`]
/// to `true`, but not before the threshold is reached.
#[test]
fn apply_activity_outcome_non_transport_failures_reach_unavailable_threshold() {
    let mut state = seeded_activity_state("s1");

    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    assert!(
        !state.activity_unavailable_for_selected(),
        "1 failure must not yet flip to unavailable"
    );

    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    assert!(
        !state.activity_unavailable_for_selected(),
        "2 failures must not yet flip to unavailable"
    );

    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    assert!(
        state.activity_unavailable_for_selected(),
        "3 consecutive non-transport failures must flip to unavailable"
    );
}

/// (b) a success after failures must show the fresh data AND actually
/// reset the streak (not merely mask it while `Some` data exists).
#[test]
fn apply_activity_outcome_success_after_failures_resets_streak_and_shows_data() {
    let mut state = seeded_activity_state("s1");
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );

    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Ok(activity_response("working", "back online")),
    );

    assert!(
        !state.activity_unavailable_for_selected(),
        "a success must reset the streak"
    );
    let activity = state
        .activity
        .as_ref()
        .expect("a successful fetch must populate activity");
    assert_eq!(activity.summary, "back online");
    assert!(!activity.stale);

    // Two MORE failures alone (not three) must still fall short of the
    // threshold -- proving the streak was actually zeroed, not masked.
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    assert!(
        !state.activity_unavailable_for_selected(),
        "the streak must have restarted from zero after the success"
    );
}

/// (c) transport-class failures (the whole daemon looks down) must never
/// advance the non-transport streak, however many occur in a row or
/// interleaved with real non-transport failures.
#[test]
fn apply_activity_outcome_transport_failure_never_advances_streak() {
    let mut state = seeded_activity_state("s1");
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );

    for _ in 0..5 {
        apply_activity_outcome(
            &mut state,
            "s1".to_string(),
            Err(ActivityFetchFailure::Transport),
        );
    }
    assert!(
        !state.activity_unavailable_for_selected(),
        "transport failures must never advance the non-transport streak"
    );

    // The ORIGINAL streak of 2 non-transport failures is still intact --
    // one more completes it to 3.
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    assert!(
        state.activity_unavailable_for_selected(),
        "the non-transport streak must have survived the interleaved transport failures"
    );
}

/// (d) switching the Sessions-pane selection to a different session must
/// not let that session inherit the previous session's failure count.
#[test]
fn apply_activity_outcome_session_switch_starts_a_fresh_streak() {
    let mut state = seeded_activity_state("s1");
    state
        .sessions_by_project
        .get_mut("widget")
        .unwrap()
        .push(session_to_row(summary("s2", "active")));
    state.sessions_nav.sync_len(state.current_sessions().len());
    assert_eq!(state.selected_session().unwrap().id, "s1");

    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    apply_activity_outcome(
        &mut state,
        "s1".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    assert!(
        !state.activity_unavailable_for_selected(),
        "2 failures for s1 must not yet be unavailable"
    );

    // The operator moves the Sessions-pane selection to "s2".
    state.sessions_nav.select(1);
    assert_eq!(state.selected_session().unwrap().id, "s2");
    assert!(
        !state.activity_unavailable_for_selected(),
        "s2 has no failure history of its own yet"
    );

    // One failure for s2 must NOT inherit s1's count-of-2.
    apply_activity_outcome(
        &mut state,
        "s2".to_string(),
        Err(ActivityFetchFailure::NonTransport),
    );
    assert!(
        !state.activity_unavailable_for_selected(),
        "s2's own streak must start from zero, not resume s1's count"
    );
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
