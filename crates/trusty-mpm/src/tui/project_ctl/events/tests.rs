//! Key-dispatch tests for the `tm projects` TUI (#2118/#2383/#2120).
//!
//! Why: split out of `events/mod.rs` (pre-emptive 500-SLOC production-cap
//! avoidance — see that file's module doc) into a file named exactly
//! `tests.rs`, which CLAUDE.md's SLOC-cap rules classify as a TEST file (the
//! 1500-SLOC cap) rather than counting against `events/mod.rs`'s production
//! cap. Every test here calls the public [`super::handle_key`] entry point
//! exclusively — exactly how the real run loop drives it — so the three
//! modals' own handlers (`events/modal.rs`) are exercised end-to-end, not in
//! isolation.
//! What: focus cycling, selection movement, every lettered action's
//! valid/invalid-context branches, the confirm gate (kill/decommission), the
//! Deliverable/Milestone view (#2383), and the config form (#2120) —
//! open/edit/tab/submit/error/close/quit-swallowing.
//! Test: this file is the test.

use super::*;
use crate::tui::project_ctl::state::{NodeId, ProjectRow};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl_c() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
}

/// A full [`crate::project::Project`] record matching one seeded
/// [`ProjectRow`] — [`ProjectCtlState::projects_full`] is what
/// [`ProjectCtlState::open_config_form`] (#2120) reads to seed the form,
/// so the config-form tests below need it populated exactly as a real
/// poll would.
fn full_project(name: &str) -> crate::project::Project {
    crate::project::Project {
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
        project_key: None,
    }
}

fn seeded_state() -> ProjectCtlState {
    let mut state = ProjectCtlState {
        projects: vec![
            ProjectRow {
                name: "alpha".to_string(),
                repo_url: "https://github.com/acme/alpha".to_string(),
                live_count: 1,
                total_count: 1,
            },
            ProjectRow {
                name: "beta".to_string(),
                repo_url: "https://github.com/acme/beta".to_string(),
                live_count: 0,
                total_count: 0,
            },
        ],
        projects_full: [
            ("alpha".to_string(), full_project("alpha")),
            ("beta".to_string(), full_project("beta")),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    state.sessions_by_project.insert(
        "alpha".to_string(),
        vec![SessionRow {
            id: "sess-1".to_string(),
            short_id: "sess-1".chars().take(8).collect(),
            name: "alpha-session".to_string(),
            branch: Some("main".to_string()),
            task: Some("ship it".to_string()),
            state: "active".to_string(),
            pending_decision: None,
            proposed_default: None,
            deliverable_id: None,
            unresumable: false,
            ..Default::default()
        }],
    );
    // #10476: open on the alpha project node — the old Projects-pane default.
    state.select_node(project_node("alpha"));
    state
}

/// The tree node of project `name` (owner `acme`).
fn project_node(name: &str) -> NodeId {
    NodeId::Project {
        owner: "acme".to_string(),
        name: name.to_string(),
    }
}

/// Select alpha's lone session — what focusing the Sessions pane used to mean.
fn on_session(state: &mut ProjectCtlState) {
    assert!(state.focus_session("sess-1"));
}

#[test]
fn quit_on_bare_q_and_ctrl_c() {
    let mut state = seeded_state();
    assert!(handle_key(&mut state, key(KeyCode::Char('q'))).is_none());
    assert!(state.should_exit);

    let mut state = seeded_state();
    assert!(handle_key(&mut state, ctrl_c()).is_none());
    assert!(state.should_exit);
}

#[test]
fn tab_cycles_focus_forward_and_back() {
    let mut state = seeded_state();
    assert_eq!(state.focus, Pane::Tree);
    handle_key(&mut state, key(KeyCode::Tab));
    assert_eq!(state.focus, Pane::Activity);
    handle_key(&mut state, key(KeyCode::Tab));
    assert_eq!(state.focus, Pane::Tree);
    handle_key(&mut state, key(KeyCode::BackTab));
    assert_eq!(state.focus, Pane::Activity);
}

#[test]
fn arrow_keys_move_the_tree_selection() {
    // Rows: acme, alpha (selected, collapsed), beta.
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Down));
    assert_eq!(state.selected_project().unwrap().name, "beta");
    handle_key(&mut state, key(KeyCode::Up));
    handle_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.selected_node(), Some(&NodeId::Owner("acme".into())));
}

#[test]
fn arrow_keys_do_nothing_in_the_activity_pane() {
    let mut state = seeded_state();
    state.focus = Pane::Activity;
    handle_key(&mut state, key(KeyCode::Down));
    assert_eq!(state.selected_project().unwrap().name, "alpha");
}

#[test]
fn enter_on_owner_and_project_toggles() {
    let mut state = seeded_state();
    let alpha = project_node("alpha");
    assert!(handle_key(&mut state, key(KeyCode::Enter)).is_none());
    assert!(
        state.tree.expanded.contains(&alpha),
        "Enter opens a project"
    );
    assert!(state.tree.rows.iter().any(|r| matches!(&r.id,
        NodeId::Session { id, .. } if id == "sess-1")));
    handle_key(&mut state, key(KeyCode::Enter));
    assert!(
        !state.tree.expanded.contains(&alpha),
        "Enter closes it again"
    );

    let acme = NodeId::Owner("acme".to_string());
    state.select_node(acme.clone());
    handle_key(&mut state, key(KeyCode::Enter));
    assert!(!state.tree.expanded.contains(&acme));
    assert_eq!(
        state.tree.rows.len(),
        1,
        "a closed owner hides its projects"
    );
    handle_key(&mut state, key(KeyCode::Enter));
    assert!(state.tree.expanded.contains(&acme));
    assert!(state.tree.details.is_none(), "toggling never opens details");
}

/// REGRESSION (#10476): before the tree, Enter on a session did nothing
/// (`events/mod.rs` bound Enter on the Projects pane only).
#[test]
fn enter_on_session_opens_details_and_returns_no_pending_action() {
    let mut state = seeded_state();
    on_session(&mut state);
    let action = handle_key(&mut state, key(KeyCode::Enter));
    assert!(action.is_none(), "details are read-only: {action:?}");
    assert_eq!(state.tree.details.as_deref(), Some("sess-1"));
    let (project, shown) = state.details_session().expect("details show the session");
    assert_eq!((project, shown.id.as_str()), ("alpha", "sess-1"));
    assert!(state.pending_confirm.is_none());
}

#[test]
fn esc_closes_details() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Enter));
    assert!(handle_key(&mut state, key(KeyCode::Esc)).is_none());
    assert!(state.tree.details.is_none());
    assert_eq!(state.selected_session().unwrap().id, "sess-1");
}

#[test]
fn f_in_details_returns_focus() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Enter));
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('f'))),
        Some(PendingAction::Focus("alpha-session".to_string()))
    );
    assert!(
        state.tree.details.is_some(),
        "focus leaves the overlay open"
    );
}

#[test]
fn f_on_a_session_node_returns_focus_and_on_a_project_does_nothing() {
    let mut state = seeded_state();
    assert!(handle_key(&mut state, key(KeyCode::Char('f'))).is_none());
    assert_eq!(state.notice.as_deref(), Some("select a session first"));
    on_session(&mut state);
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('f'))),
        Some(PendingAction::Focus("alpha-session".to_string()))
    );
}

#[test]
fn details_swallow_quit_and_close_when_session_leaves() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Enter));
    handle_key(&mut state, key(KeyCode::Char('q')));
    handle_key(&mut state, ctrl_c());
    assert!(
        !state.should_exit,
        "quit is swallowed while details are open"
    );
    assert!(state.tree.details.is_some());

    state.sessions_by_project.get_mut("alpha").unwrap().clear();
    state.rebuild_tree();
    handle_key(&mut state, key(KeyCode::Char('x')));
    assert!(state.tree.details.is_none());
    assert_eq!(
        state.notice.as_deref(),
        Some("that session is no longer listed")
    );
}

#[test]
fn details_forward_the_session_verbs() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Enter));
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('a'))),
        Some(PendingAction::Attach("sess-1".to_string()))
    );
    handle_key(&mut state, key(KeyCode::Char('d')));
    assert_eq!(
        state.pending_confirm.as_ref().map(|c| c.kind),
        Some(ConfirmKind::Decommission)
    );
}

#[test]
fn g_toggles_worker_indentation() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('g')));
    assert!(state.tree.flat_workers);
    handle_key(&mut state, key(KeyCode::Char('g')));
    assert!(!state.tree.flat_workers, "default is indented");
}

#[test]
fn launch_targets_the_selected_nodes_project() {
    let mut state = seeded_state();
    on_session(&mut state);
    let action = handle_key(&mut state, key(KeyCode::Char('l')));
    assert_eq!(action, Some(PendingAction::Launch("alpha".to_string())));

    state.select_node(project_node("beta"));
    let action = handle_key(&mut state, key(KeyCode::Char('l')));
    assert_eq!(action, Some(PendingAction::Launch("beta".to_string())));

    state.select_node(NodeId::Owner("acme".to_string()));
    assert!(handle_key(&mut state, key(KeyCode::Char('l'))).is_none());
    assert_eq!(state.notice.as_deref(), Some("no project selected"));
}

#[test]
fn kill_resume_decommission_attach_on_bar_or_project_act_on_nothing() {
    for node in [NodeId::Owner("acme".to_string()), project_node("alpha")] {
        for verb in ['k', 'r', 'd', 'a'] {
            let mut state = seeded_state();
            state.select_node(node.clone());
            let action = handle_key(&mut state, key(KeyCode::Char(verb)));
            assert!(action.is_none(), "{verb} on {node:?} returned {action:?}");
            assert!(
                state.pending_confirm.is_none(),
                "{verb} on {node:?} opened a gate"
            );
            assert_eq!(state.notice.as_deref(), Some("select a session first"));
        }
    }
}

#[test]
fn existing_bindings_unchanged() {
    // Every pre-tree verb still produces what it did, on the node kind that
    // stands in for its old pane.
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('c')));
    assert_eq!(state.config_form.take().unwrap().project_name, "alpha");
    handle_key(&mut state, key(KeyCode::Char('v')));
    assert_eq!(state.deliverable_view.take().unwrap().project_name, "alpha");

    on_session(&mut state);
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('r'))),
        Some(PendingAction::Resume("sess-1".to_string()))
    );
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('a'))),
        Some(PendingAction::Attach("sess-1".to_string()))
    );
    handle_key(&mut state, key(KeyCode::Char('k')));
    assert_eq!(
        state.pending_confirm.take().map(|c| c.kind),
        Some(ConfirmKind::Kill)
    );
    handle_key(&mut state, key(KeyCode::Char('d')));
    assert_eq!(
        state.pending_confirm.take().map(|c| c.kind),
        Some(ConfirmKind::Decommission)
    );
    state.set_notice("x");
    handle_key(&mut state, key(KeyCode::Esc));
    assert!(state.notice.is_none());
    handle_key(&mut state, key(KeyCode::Char('q')));
    assert!(state.should_exit);
}

#[test]
fn kill_without_selection_sets_notice_and_returns_none() {
    let mut state = seeded_state();
    state.select_node(project_node("beta")); // beta has no sessions
    let action = handle_key(&mut state, key(KeyCode::Char('k')));
    assert!(action.is_none());
    assert_eq!(state.notice.as_deref(), Some("select a session first"));
    assert!(state.pending_confirm.is_none());
}

#[test]
fn resume_and_attach_target_selected_session_immediately() {
    // Neither verb is destructive, so DOC-35 §5.2's confirm gate does not
    // apply — both fire on the first keypress.
    let mut state = seeded_state();
    on_session(&mut state);
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('r'))),
        Some(PendingAction::Resume("sess-1".to_string()))
    );
    assert!(state.pending_confirm.is_none());
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('a'))),
        Some(PendingAction::Attach("sess-1".to_string()))
    );
    assert!(state.pending_confirm.is_none());
}

/// #2595: `r` on a session flagged `unresumable` must NOT fire
/// `PendingAction::Resume` — its workspace is gone for good, so the daemon
/// round trip can only 422 (#2594). Mirrors the CLI picker's
/// `PickerDecision::Unresumable` guard, adapted to the TUI's notice idiom: no
/// confirm gate, just a refusal pointing at `d` (decommission) instead.
#[test]
fn resume_on_unresumable_session_is_blocked() {
    let mut state = seeded_state();
    on_session(&mut state);
    state.sessions_by_project.get_mut("alpha").unwrap()[0].unresumable = true;

    let action = handle_key(&mut state, key(KeyCode::Char('r')));
    assert!(
        action.is_none(),
        "a dead session must never produce PendingAction::Resume"
    );
    assert!(state.pending_confirm.is_none());
    let notice = state.notice.as_deref().unwrap_or_default();
    assert!(
        notice.contains("dead") || notice.to_lowercase().contains("workspace"),
        "notice should explain the session is dead / workspace missing, got {notice:?}"
    );
    assert!(
        notice.contains('d'),
        "notice should point at the [d] decommission remedy, got {notice:?}"
    );
}

/// #2595 counterpart: a HEALTHY session (`unresumable: false`, the default)
/// must still fire `PendingAction::Resume` immediately — the guard must not
/// over-fire and block ordinary resumes.
#[test]
fn resume_on_healthy_session_fires_immediately() {
    let mut state = seeded_state();
    on_session(&mut state);
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Char('r'))),
        Some(PendingAction::Resume("sess-1".to_string()))
    );
    assert!(state.notice.is_none());
}

// ---- DOC-35 §6 config form (#2120) --------------------------------------

#[test]
fn c_opens_the_config_form_directly_not_via_pending_action() {
    let mut state = seeded_state();
    let action = handle_key(&mut state, key(KeyCode::Char('c')));
    assert!(
        action.is_none(),
        "opening the form is a pure state mutation, not a PendingAction"
    );
    let form = state.config_form.expect("form should be open");
    assert_eq!(form.project_name, "alpha");
}

#[test]
fn c_without_a_selected_project_sets_notice_and_opens_nothing() {
    let mut state = ProjectCtlState::default();
    handle_key(&mut state, key(KeyCode::Char('c')));
    assert!(state.config_form.is_none());
    assert_eq!(state.notice.as_deref(), Some("no project selected"));
}

#[test]
fn c_is_ignored_on_a_session_node() {
    let mut state = seeded_state();
    on_session(&mut state);
    assert!(handle_key(&mut state, key(KeyCode::Char('c'))).is_none());
    assert!(state.config_form.is_none());
}

#[test]
fn config_form_captures_typed_characters_into_the_focused_field() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('c')));
    // Focus starts on default_branch; typed chars append to its buffer.
    handle_key(&mut state, key(KeyCode::Char('!')));
    assert_eq!(
        state.config_form.as_ref().unwrap().default_branch.value,
        "main!"
    );
    handle_key(&mut state, key(KeyCode::Backspace));
    assert_eq!(
        state.config_form.as_ref().unwrap().default_branch.value,
        "main"
    );
}

#[test]
fn config_form_tab_cycles_field_focus_not_pane_focus() {
    use super::super::state::ConfigFormFocus;
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('c')));
    assert_eq!(
        state.config_form.as_ref().unwrap().focus,
        ConfigFormFocus::DefaultBranch
    );
    handle_key(&mut state, key(KeyCode::Tab));
    assert_eq!(
        state.config_form.as_ref().unwrap().focus,
        ConfigFormFocus::Description
    );
    // The outer pane focus must NOT have moved — Tab is captured by the modal.
    assert_eq!(state.focus, Pane::Tree);
}

#[test]
fn config_form_esc_closes_without_submitting() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('c')));
    handle_key(&mut state, key(KeyCode::Char('!'))); // unsaved edit
    let action = handle_key(&mut state, key(KeyCode::Esc));
    assert!(action.is_none());
    assert!(state.config_form.is_none(), "Esc discards the form");
}

#[test]
fn config_form_enter_with_no_edits_sets_inline_error_and_stays_open() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('c')));
    let action = handle_key(&mut state, key(KeyCode::Enter));
    assert!(action.is_none(), "an unchanged form has nothing to submit");
    let form = state.config_form.expect("form stays open");
    assert!(form.error.is_some());
}

#[test]
fn config_form_enter_with_an_edit_returns_submit_config_action() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('c')));
    // Directly set the buffer rather than typing, to model a real edit
    // without depending on the field's pre-filled starting text.
    state.config_form.as_mut().unwrap().default_branch.value = "develop".to_string();
    let action = handle_key(&mut state, key(KeyCode::Enter));
    match action {
        Some(PendingAction::SubmitConfig(name, args)) => {
            assert_eq!(name, "alpha");
            assert_eq!(args.default_branch.as_deref(), Some("develop"));
        }
        other => panic!("expected SubmitConfig, got {other:?}"),
    }
    // The form stays open until the async result lands (success closes it
    // in `actions::dispatch`, not here).
    assert!(state.config_form.is_some());
}

#[test]
fn config_form_swallows_quit_and_pane_keys() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('c')));

    handle_key(&mut state, key(KeyCode::Char('q')));
    assert!(
        !state.should_exit,
        "q must not quit while the form is open — it types 'q'"
    );
    assert!(state.config_form.is_some());

    // 'q' is captured as a text character into the focused field, not
    // treated as quit — matches "modal captures ALL input".
    assert!(
        state
            .config_form
            .as_ref()
            .unwrap()
            .default_branch
            .value
            .ends_with('q')
    );
}

#[test]
fn lettered_actions_are_ignored_outside_their_pane() {
    let mut state = seeded_state();
    on_session(&mut state);
    // #10476: the Activity pane binds no verb (no action, no notice).
    state.focus = Pane::Activity;
    for verb in ['l', 'k', 'r', 'd', 'a', 'c', 'v', 'f', 'g'] {
        assert!(handle_key(&mut state, key(KeyCode::Char(verb))).is_none());
    }
    assert!(state.notice.is_none());
    assert!(state.pending_confirm.is_none());
    assert!(!state.tree.flat_workers);
}

#[test]
fn esc_clears_notice() {
    let mut state = seeded_state();
    state.set_notice("hi");
    handle_key(&mut state, key(KeyCode::Esc));
    assert!(state.notice.is_none());
}

// ---- DOC-35 §5.2 confirmation gate ---------------------------------------

#[test]
fn kill_on_active_session_opens_confirm_gate_instead_of_firing() {
    // seeded_state's lone session is "active".
    let mut state = seeded_state();
    on_session(&mut state);
    let action = handle_key(&mut state, key(KeyCode::Char('k')));
    assert!(action.is_none(), "must not fire on the first keypress");
    let confirm = state.pending_confirm.expect("confirm gate should be open");
    assert_eq!(confirm.kind, ConfirmKind::Kill);
    assert_eq!(confirm.session_id, "sess-1");
}

#[test]
fn kill_on_non_active_session_fires_immediately() {
    let mut state = seeded_state();
    state.sessions_by_project.get_mut("alpha").unwrap()[0].state = "stopped".to_string();
    on_session(&mut state);
    let action = handle_key(&mut state, key(KeyCode::Char('k')));
    assert_eq!(action, Some(PendingAction::Kill("sess-1".to_string())));
    assert!(state.pending_confirm.is_none());
}

#[test]
fn decommission_always_opens_confirm_gate_regardless_of_state() {
    for state_word in ["active", "stopped", "provisioning", "errored"] {
        let mut state = seeded_state();
        state.sessions_by_project.get_mut("alpha").unwrap()[0].state = state_word.to_string();
        on_session(&mut state);
        let action = handle_key(&mut state, key(KeyCode::Char('d')));
        assert!(action.is_none(), "must not fire for state {state_word}");
        let confirm = state
            .pending_confirm
            .expect("confirm gate should be open for state {state_word}");
        assert_eq!(confirm.kind, ConfirmKind::Decommission);
        assert_eq!(confirm.session_id, "sess-1");
    }
}

#[test]
fn confirm_gate_y_resolves_to_the_real_action_and_clears() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Char('d'))); // open the gate
    assert!(state.pending_confirm.is_some());

    let action = handle_key(&mut state, key(KeyCode::Char('y')));
    assert_eq!(
        action,
        Some(PendingAction::Decommission("sess-1".to_string()))
    );
    assert!(state.pending_confirm.is_none());
}

#[test]
fn confirm_gate_uppercase_y_and_enter_also_confirm() {
    for confirm_key in [key(KeyCode::Char('Y')), key(KeyCode::Enter)] {
        let mut state = seeded_state();
        on_session(&mut state);
        handle_key(&mut state, key(KeyCode::Char('d')));
        let action = handle_key(&mut state, confirm_key);
        assert_eq!(
            action,
            Some(PendingAction::Decommission("sess-1".to_string()))
        );
    }
}

#[test]
fn confirm_gate_n_or_esc_cancels_and_sets_notice() {
    for cancel_key in [
        key(KeyCode::Char('n')),
        key(KeyCode::Char('N')),
        key(KeyCode::Esc),
    ] {
        let mut state = seeded_state();
        on_session(&mut state);
        handle_key(&mut state, key(KeyCode::Char('d')));
        let action = handle_key(&mut state, cancel_key);
        assert!(action.is_none());
        assert!(state.pending_confirm.is_none());
        assert_eq!(state.notice.as_deref(), Some("cancelled"));
    }
}

#[test]
fn confirm_gate_swallows_unrelated_keys_and_stays_open() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Char('d')));
    assert!(state.pending_confirm.is_some());

    // An unrelated key must not resolve or dismiss the gate.
    let action = handle_key(&mut state, key(KeyCode::Char('x')));
    assert!(action.is_none());
    assert!(
        state.pending_confirm.is_some(),
        "gate must stay open for an unrecognised key"
    );
}

#[test]
fn confirm_gate_blocks_quit_keys_matching_the_spec_not_in_a_modal_rule() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Char('d')));

    handle_key(&mut state, key(KeyCode::Char('q')));
    assert!(!state.should_exit, "q must not quit while a modal is open");
    assert!(state.pending_confirm.is_some());

    handle_key(&mut state, ctrl_c());
    assert!(
        !state.should_exit,
        "Ctrl-C must not quit while a modal is open"
    );
    assert!(state.pending_confirm.is_some());
}

#[test]
fn quit_still_works_once_the_gate_is_resolved() {
    let mut state = seeded_state();
    on_session(&mut state);
    handle_key(&mut state, key(KeyCode::Char('d')));
    handle_key(&mut state, key(KeyCode::Esc)); // cancel — gate closes
    assert!(state.pending_confirm.is_none());

    handle_key(&mut state, key(KeyCode::Char('q')));
    assert!(state.should_exit);
}

// ---- DOC-35 §10.8 Deliverable/Milestone view (#2383) ---------------------

#[test]
fn v_opens_the_deliverable_view_for_the_selected_project() {
    let mut state = seeded_state();
    assert!(handle_key(&mut state, key(KeyCode::Char('v'))).is_none());
    let view = state.deliverable_view.expect("view should be open");
    assert_eq!(view.project_name, "alpha");
}

#[test]
fn v_without_a_selected_project_sets_notice_and_opens_nothing() {
    let mut state = ProjectCtlState::default();
    handle_key(&mut state, key(KeyCode::Char('v')));
    assert!(state.deliverable_view.is_none());
    assert_eq!(state.notice.as_deref(), Some("no project selected"));
}

#[test]
fn v_is_ignored_on_a_session_node() {
    let mut state = seeded_state();
    on_session(&mut state);
    assert!(handle_key(&mut state, key(KeyCode::Char('v'))).is_none());
    assert!(state.deliverable_view.is_none());
}

#[test]
fn deliverable_view_scrolls_and_closes_on_esc_or_v() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('v')));
    handle_key(&mut state, key(KeyCode::Down));
    assert_eq!(state.deliverable_view.as_ref().unwrap().scroll, 1);
    handle_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.deliverable_view.as_ref().unwrap().scroll, 0);

    handle_key(&mut state, key(KeyCode::Esc));
    assert!(state.deliverable_view.is_none());

    handle_key(&mut state, key(KeyCode::Char('v')));
    handle_key(&mut state, key(KeyCode::Char('v')));
    assert!(
        state.deliverable_view.is_none(),
        "v must close the view the same way it opened it"
    );
}

#[test]
fn deliverable_view_swallows_quit_and_unrelated_keys() {
    let mut state = seeded_state();
    handle_key(&mut state, key(KeyCode::Char('v')));

    handle_key(&mut state, key(KeyCode::Char('q')));
    assert!(!state.should_exit, "q must not quit while the view is open");
    assert!(state.deliverable_view.is_some());

    handle_key(&mut state, ctrl_c());
    assert!(!state.should_exit);
    assert!(state.deliverable_view.is_some());

    handle_key(&mut state, key(KeyCode::Char('x')));
    assert!(
        state.deliverable_view.is_some(),
        "an unrecognised key must not close the view"
    );
}
