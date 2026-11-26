//! #11822: the new-session picker opens on the cwd repo, its arrows move the
//! highlight from every step, and every row says why it is offered.
//!
//! Why: the reported dialog preselected an unrelated registry row, ignored the
//! arrow keys in its free-text step, and listed a registry row with no reason.
//! What: one test per acceptance criterion, plus the fail-open fallback and the
//! Enter leg of the new cwd row. Keys go through [`super::map_key`], the
//! crossterm-to-[`Input`] decoder the live loop uses, never a hand-built
//! `Input`.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::project::Project;

use super::map_key;
use super::new_session::{self, NewProject, NewSessionFlow, ProjectIdentity, Step};
use super::state::{Action, Mode, TuiState};

/// A real git checkout outside every temp prefix: this crate's own directory.
///
/// The #7406 filter drops a cwd under a temp dir or outside any checkout, so a
/// `tempdir()` fixture would only ever exercise the fallback.
fn cwd_root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

/// The reporter's cwd: `design-system`, checked out at [`cwd_root`].
fn design_system() -> ProjectIdentity {
    ProjectIdentity {
        name: "design-system".to_string(),
        repo_url: "https://github.com/acme/design-system".to_string(),
        root: cwd_root(),
    }
}

fn project(name: &str, repo_url: &str) -> Project {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "repo_url": repo_url,
        "default_branch": "main",
    }))
    .expect("project fixture")
}

/// Every registry row is checked out at [`cwd_root`] only when it IS the cwd.
fn checkout_of(project: &Project) -> Option<PathBuf> {
    if project.name == "design-system" {
        return Some(PathBuf::from(cwd_root()));
    }
    Some(PathBuf::from("/nonexistent/11822").join(&project.name))
}

/// The reporter's registry: two projects, neither the cwd repo.
fn registry() -> Vec<Project> {
    vec![
        project("service-a", "https://github.com/acme/service-a"),
        project("skills", "https://github.com/acme/skills"),
    ]
}

fn session(source_id: &str) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: format!("{source_id}-id"),
        name: "tm-fixture".to_string(),
        state: "active".to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: Some(format!("https://github.com/{source_id}")),
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: Some(source_id.to_string()),
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot: 1,
        deleted: false,
        auto_resume_parked: None,
        kind: None,
        parent_session_id: None,
    }
}

/// The highlighted row's text.
fn highlighted(flow: &NewSessionFlow) -> String {
    flow.rows()
        .into_iter()
        .find(|r| r.starts_with('▸'))
        .expect("one row is highlighted")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Send one raw key event through the live decoder into the TUI state.
fn press(state: &mut TuiState, code: KeyCode) -> Action {
    let input = map_key(key(code)).expect("the decoder maps this key");
    state.apply(input, &[])
}

fn open_flow(state: &TuiState) -> &NewSessionFlow {
    match state.mode() {
        Mode::New(flow) => flow,
        _ => panic!("the new-session flow is not open"),
    }
}

/// AC (a): an unregistered cwd repo is offered and preselected, even when the
/// cursor session sits on a different project.
#[test]
fn new_session_cwd_preselects_an_unregistered_cwd_repo() {
    let targets = new_session::targets_from_with(&registry(), &[], checkout_of);
    let cursor = session("acme/service-a");
    let flow = NewSessionFlow::new(targets).preselected(Some(design_system()), Some(&cursor));
    assert_eq!(
        highlighted(&flow),
        "▸ acme/design-system · cwd",
        "{:?}",
        flow.rows()
    );
}

/// AC (a): a registered cwd repo is preselected in place, not offered twice.
#[test]
fn new_session_cwd_preselects_the_registered_cwd_row() {
    let mut projects = registry();
    projects.push(project(
        "design-system",
        "https://github.com/acme/design-system",
    ));
    let targets = new_session::targets_from_with(&projects, &[], checkout_of);
    let before = targets.len();
    let cursor = session("acme/service-a");
    let flow = NewSessionFlow::new(targets).preselected(Some(design_system()), Some(&cursor));
    assert_eq!(flow.rows().len(), before, "{:?}", flow.rows());
    assert_eq!(highlighted(&flow), "▸ acme/design-system · cwd");
}

/// Fail-open check: with no cwd repo, or a cwd the #7406 filter refuses, the
/// picker adds no row and opens on the cursor session's project as before.
#[test]
fn new_session_cwd_falls_back_to_the_cursor_without_a_cwd_repo() {
    let cursor = session("acme/skills");
    let in_temp = ProjectIdentity {
        root: "/tmp/11822/design-system".to_string(),
        ..design_system()
    };
    for cwd in [None, Some(in_temp)] {
        let targets = new_session::targets_from_with(&registry(), &[], checkout_of);
        let before = targets.len();
        let flow = NewSessionFlow::new(targets).preselected(cwd.clone(), Some(&cursor));
        assert_eq!(flow.rows().len(), before, "cwd {cwd:?}: {:?}", flow.rows());
        assert_eq!(
            highlighted(&flow),
            "▸ acme/skills · registered",
            "cwd {cwd:?}"
        );
    }
}

/// AC (b): Up/Down, sent as raw key events through the decoder, move the
/// highlight over the list and clamp at both ends.
#[test]
fn new_session_cwd_arrow_keys_through_the_decoder_move_the_highlight() {
    let mut state = TuiState::new(None, None);
    let targets = new_session::targets_from_with(&registry(), &[], checkout_of);
    state.open_new_session(NewSessionFlow::new(targets).preselected(Some(design_system()), None));
    let start = highlighted(open_flow(&state));
    // Rows: the cwd repo first, then the registry by label, then `other`.
    let steps = [
        (KeyCode::Up, "design-system"),
        (KeyCode::Down, "service-a"),
        (KeyCode::Down, "skills"),
        (KeyCode::Down, "other"),
        (KeyCode::Down, "other"),
        (KeyCode::Up, "skills"),
    ];
    assert!(start.contains("design-system"), "{start}");
    for (code, expected) in steps {
        press(&mut state, code);
        let row = highlighted(open_flow(&state));
        assert!(
            row.contains(expected),
            "{code:?}: expected {expected}, got {row}"
        );
    }
}

/// AC (b): an arrow in the free-text entry — the variant with no list — goes
/// back to the list and moves the highlight, instead of doing nothing.
#[test]
fn new_session_cwd_arrow_keys_leave_the_path_entry_for_the_list() {
    let mut state = TuiState::new(None, None);
    let targets = new_session::targets_from_with(&registry(), &[], checkout_of);
    state.open_new_session(NewSessionFlow::new(targets).preselected(None, None));
    press(&mut state, KeyCode::End);
    press(&mut state, KeyCode::Enter);
    assert_eq!(
        open_flow(&state).typed(),
        Some(""),
        "Enter on `other` opens the entry"
    );

    press(&mut state, KeyCode::Up);
    let flow = open_flow(&state);
    assert_eq!(flow.typed(), None, "the arrow left the entry");
    assert!(highlighted(flow).contains("skills"), "{:?}", flow.rows());
}

/// AC (c): every row is the cwd repo or a registry row (after the #7406
/// filter). A repo with a live session but no registration never appears.
#[test]
fn new_session_cwd_rows_are_only_the_cwd_and_the_registry() {
    let mut projects = registry();
    // #7406: a scratchpad registration stays filtered out.
    projects.push(project("scratch", "/tmp/claude/scratchpad/scratch"));
    let sessions = [session("acme/stray")];
    let targets = new_session::targets_from_with(&projects, &sessions, checkout_of);
    let flow = NewSessionFlow::new(targets).preselected(Some(design_system()), None);
    let mut labels: Vec<String> = flow
        .rows()
        .iter()
        .map(|r| r[r.char_indices().nth(2).map_or(0, |(i, _)| i)..].to_string())
        .collect();
    labels.sort();
    assert_eq!(
        labels,
        vec![
            "acme/design-system · cwd",
            "acme/service-a · registered",
            "acme/skills · registered",
            "other — type a project path…",
        ]
    );
}

/// AC (d): every project row says why it is offered.
#[test]
fn new_session_cwd_rows_say_why_they_are_offered() {
    let targets = new_session::targets_from_with(&registry(), &[], checkout_of);
    let flow = NewSessionFlow::new(targets).preselected(Some(design_system()), None);
    let rows = flow.rows();
    let (escape, projects) = rows.split_last().expect("rows");
    assert!(escape.contains("other"), "{rows:?}");
    assert!(projects[0].ends_with(" · cwd"), "{rows:?}");
    for row in &projects[1..] {
        assert!(row.ends_with(" · registered"), "{rows:?}");
    }
}

/// Enter on an unregistered cwd row registers it, then creates in the checkout.
#[test]
fn new_session_cwd_enter_registers_then_creates_in_the_checkout() {
    let targets = new_session::targets_from_with(&registry(), &[], checkout_of);
    let mut flow = NewSessionFlow::new(targets).preselected(Some(design_system()), None);
    let Step::Create(request) = flow.apply(super::state::Input::Enter) else {
        panic!("Enter on the cwd row creates");
    };
    assert_eq!(
        request.register,
        Some(NewProject {
            name: "design-system".to_string(),
            repo_url: "https://github.com/acme/design-system".to_string(),
        })
    );
    assert_eq!(request.repo, cwd_root());
}

/// Twelve registered projects under one owner: longer than the eight-row window.
fn long_registry() -> Vec<Project> {
    (0..12)
        .map(|i| project(&format!("p{i:02}"), &format!("https://github.com/acme/p{i:02}")))
        .collect()
}

/// Assert the highlighted row is the one labelled `label`.
fn assert_at(state: &TuiState, label: &str) {
    let row = highlighted(open_flow(state));
    assert!(row.starts_with(&format!("▸ {label}")), "want {label}, got {row}");
}

/// AC (b): every Up/Down, sent through the decoder, moves the highlight exactly
/// one row, across the eight-row window, and clamps at the first and last row.
#[test]
fn new_session_cwd_arrows_step_one_row_and_clamp_over_a_long_list() {
    let mut state = TuiState::new(None, None);
    let targets = new_session::targets_from_with(&long_registry(), &[], checkout_of);
    state.open_new_session(NewSessionFlow::new(targets).preselected(None, None));
    let order: Vec<String> = (0..12)
        .map(|i| format!("acme/p{i:02}"))
        .chain(std::iter::once("other".to_string()))
        .collect();
    assert_at(&state, &order[0]);
    assert_eq!(press(&mut state, KeyCode::Up), Action::Ignore, "Up clamps");
    assert_at(&state, &order[0]);
    for label in &order[1..] {
        assert_eq!(press(&mut state, KeyCode::Down), Action::Redraw, "{label}");
        assert_at(&state, label);
    }
    assert_eq!(press(&mut state, KeyCode::Down), Action::Ignore, "Down clamps");
    assert_at(&state, "other");
    for label in order[..order.len() - 1].iter().rev() {
        assert_eq!(press(&mut state, KeyCode::Up), Action::Redraw, "{label}");
        assert_at(&state, label);
    }
    assert_eq!(press(&mut state, KeyCode::Up), Action::Ignore, "Up clamps again");
}

/// AC (b): with no rows at all, every list key is a no-op and nothing panics.
#[test]
fn new_session_cwd_list_keys_on_an_empty_list_change_nothing() {
    let mut state = TuiState::new(None, None);
    state.open_new_session(NewSessionFlow::new(Vec::new()).preselected(None, None));
    let keys = [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Enter,
    ];
    for code in keys {
        assert_eq!(press(&mut state, code), Action::Ignore, "{code:?}");
        assert!(open_flow(&state).rows().is_empty(), "{code:?}");
    }
}

/// AC (b) reads "j/k, where the dialog list accepts them". Since #7421 it does
/// not: a printable key narrows the list, so `j`/`k` filter and never move.
#[test]
fn new_session_cwd_j_and_k_filter_the_list_rather_than_move() {
    let mut state = TuiState::new(None, None);
    let targets = new_session::targets_from_with(&registry(), &[], checkout_of);
    state.open_new_session(NewSessionFlow::new(targets).preselected(Some(design_system()), None));
    press(&mut state, KeyCode::Char('k'));
    let flow = open_flow(&state);
    assert_eq!(flow.filter(), "k");
    assert!(highlighted(flow).contains("skills"), "{:?}", flow.rows());
    press(&mut state, KeyCode::Backspace);
    press(&mut state, KeyCode::Char('j'));
    assert_eq!(open_flow(&state).filter(), "j");
}

/// AC (c), the reporter's case: the cursor session runs in a repo the registry
/// does not hold. With or without a cwd repo, every row is the cwd repo, a
/// registry row labelled `registered`, or the typed-path escape. The session's
/// repo never becomes a row.
#[test]
fn new_session_cwd_a_cursor_session_outside_the_registry_adds_no_row() {
    let stray = session("acme/history-only");
    let registered: Vec<String> = registry()
        .iter()
        .map(|p| format!("acme/{} · registered", p.name))
        .collect();
    for cwd in [None, Some(design_system())] {
        let sessions = [stray.clone()];
        let targets = new_session::targets_from_with(&registry(), &sessions, checkout_of);
        let flow = NewSessionFlow::new(targets).preselected(cwd.clone(), Some(&stray));
        for row in flow.rows() {
            let label = row[row.char_indices().nth(2).map_or(0, |(i, _)| i)..].to_string();
            let allowed = registered.contains(&label)
                || label.starts_with("other")
                || (cwd.is_some() && label == "acme/design-system · cwd");
            assert!(allowed, "cwd {cwd:?}: stray row {label:?}");
            assert!(!label.contains("history-only"), "cwd {cwd:?}: {label:?}");
        }
    }
}
