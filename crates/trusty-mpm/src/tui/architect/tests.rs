//! Tests for the read-only Architect view (#10966, goal ar-G01).

use std::path::Path;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

use super::data::{DashSnapshot, STORE_NOT_SET, fetch_dashboard, snapshot_from};
use super::{ArchitectAction, ArchitectState, Fetch, REFRESH, Tab, handle_key};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn doc(id: &str, data: Value) -> Value {
    json!({"id": id, "version": 1, "data": data})
}

fn item(id: &str, kind: &str, status: &str, title: &str) -> Value {
    doc(
        id,
        json!({"kind": kind, "status": status, "title": title, "goal": "G01", "session": "tm-03"}),
    )
}

fn sample() -> DashSnapshot {
    let items = [
        item("D3a", "question", "open", "Read-only Architect view?"),
        item("D3b", "question", "done", "closed decision"),
        item("A10", "action", "answered", "Seed the sandbox"),
        item("T21", "task", "open", "Detail pane"),
        item("T22", "task", "cancelled", "dropped"),
    ];
    let goals = [
        doc(
            "G01",
            json!({"title": "tm tui console look", "lifecycle": "active", "status_line": "slice 3"}),
        ),
        doc("G02", json!({"title": "old", "lifecycle": "done"})),
    ];
    let sessions = [doc(
        "S01",
        json!({"name": "tm-03", "state": "working", "summary": "theme"}),
    )];
    let questions = [
        doc(
            "Q01",
            json!({"asked": "Dark only?", "status": "pending", "session": "tm-03"}),
        ),
        doc("Q02", json!({"asked": "gone", "status": "cleared"})),
    ];
    let tally = json!({"open": 2, "questions": 1, "actions": 0, "tasks": 1,
        "answered_unrelayed": 1, "parked": 0, "running": 0, "answers_waiting": 1});
    snapshot_from(&items, &goals, &sessions, &questions, &tally)
}

#[test]
fn snapshot_splits_items_by_kind_and_drops_closed_rows() {
    let s = sample();
    let ids = |rows: &[super::data::DashRow]| rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&s.decisions), ["D3a"]);
    assert_eq!(ids(&s.actions), ["A10"]);
    assert_eq!(ids(&s.tasks), ["T21"]);
    assert_eq!(ids(&s.goals), ["G01"]);
    assert_eq!(ids(&s.sessions), ["S01"]);
    assert_eq!(ids(&s.qa), ["Q01"]);
    assert_eq!(s.decisions[0].note, "G01 · tm-03");
    assert!(
        s.decisions[0]
            .fields
            .iter()
            .any(|(k, v)| k == "title" && v.starts_with("Read-only"))
    );
    assert_eq!(
        s.tally,
        "2 open dashboard items: 1 question, 0 actions, 1 task, 1 answers waiting"
    );
}

#[tokio::test]
async fn fetch_dashboard_says_why_without_store_or_socket() {
    let socket = Path::new("/nonexistent/sock");
    let err = fetch_dashboard(Some(socket), None)
        .await
        .expect_err("no store");
    assert_eq!(err, STORE_NOT_SET);
    let err = fetch_dashboard(None, Some(Path::new("/tmp/store")))
        .await
        .expect_err("no socket");
    assert!(err.contains("#10899"), "{err}");
}

#[test]
fn fetch_is_due_once_per_refresh_and_never_twice_in_flight() {
    let mut s = ArchitectState::default();
    let t0 = Instant::now();
    assert!(s.fetch_due(t0));
    assert!(!s.fetch_due(t0 + REFRESH), "one in flight");
    s.apply(Err(STORE_NOT_SET.into()));
    assert!(!s.fetch_due(t0 + REFRESH / 2), "fresh");
    assert!(s.fetch_due(t0 + REFRESH));
}

#[test]
fn tab_keys_cycle_and_esc_closes_detail_then_returns_home() {
    let mut s = ArchitectState::default();
    s.apply(Ok(sample()));
    handle_key(&mut s, key(KeyCode::BackTab));
    assert_eq!(s.tab, Tab::Qa, "wraps left to the last of six tabs");
    handle_key(&mut s, key(KeyCode::Tab));
    assert_eq!(s.tab, Tab::Decisions, "wraps right");
    for want in [Tab::Actions, Tab::Goals, Tab::Sessions, Tab::Tasks, Tab::Qa] {
        handle_key(&mut s, key(KeyCode::Right));
        assert_eq!(s.tab, want);
    }
    handle_key(&mut s, key(KeyCode::Right));
    assert_eq!(s.tab, Tab::Decisions, "six tabs, no Live until S4");
    handle_key(&mut s, key(KeyCode::Enter));
    assert!(s.expanded);
    assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), None);
    assert!(!s.expanded);
    assert_eq!(
        handle_key(&mut s, key(KeyCode::Esc)),
        Some(ArchitectAction::Home)
    );
}

fn draw(state: &ArchitectState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(140, 20)).expect("test backend");
    terminal
        .draw(|f| super::render::render(f, f.area(), state))
        .expect("draw");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}

#[test]
fn architect_render_lists_rows_with_badges_and_counts() {
    let mut s = ArchitectState::default();
    s.apply(Ok(sample()));
    let screen = draw(&s);
    for want in [
        "ARCHITECT",
        "Decisions 1",
        "Goals 1",
        "Q&A 1",
        " READ-ONLY ",
        "D3a",
        " OPEN ",
        "Read-only Architect view?",
        "1 answers waiting",
    ] {
        assert!(screen.contains(want), "missing {want:?}");
    }
    assert!(!screen.contains("Live"), "no Live tab until S4");
}

#[test]
fn architect_render_says_why_when_the_store_is_not_set() {
    let mut s = ArchitectState::default();
    s.apply(Err(STORE_NOT_SET.into()));
    assert!(matches!(s.dashboard, Fetch::Failed(_)));
    let screen = draw(&s);
    assert!(screen.contains("--architect-store"), "{screen}");
}
