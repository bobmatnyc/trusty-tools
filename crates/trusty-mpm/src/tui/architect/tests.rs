//! Tests for the read-only Architect view and its Live tracker (#10966,
//! goal ar-G01).

use std::path::Path;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

use super::data::{DashSnapshot, STORE_NOT_SET, fetch_dashboard, snapshot_from};
use super::live::{LiveAgent, LiveSession, LiveSnapshot, Record, fetch_live, group, live_lines};
use super::{
    ArchitectAction, ArchitectState, ArchitectUpdate, Due, Fetch, REFRESH, Tab, handle_key,
};
use crate::client::{DaemonClient, SessionRow};

/// Apply one dashboard and one Live result, as two fetch tasks would.
fn feed(s: &mut ArchitectState, dashboard: Result<DashSnapshot, String>, live: LiveSnapshot) {
    s.apply(ArchitectUpdate::Dashboard(dashboard));
    s.apply(ArchitectUpdate::Live(Ok(live)));
}

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
    let both = Due {
        dashboard: true,
        live: true,
    };
    assert_eq!(s.fetch_due(t0), both);
    assert_eq!(s.fetch_due(t0 + REFRESH), Due::default(), "both in flight");
    feed(&mut s, Err(STORE_NOT_SET.into()), LiveSnapshot::default());
    assert_eq!(s.fetch_due(t0 + REFRESH / 2), Due::default(), "fresh");
    assert_eq!(s.fetch_due(t0 + REFRESH), both);
}

// #10966: the Live tracker refreshes while a dashboard fetch is still out.
#[test]
fn slow_dashboard_fetch_does_not_hold_the_live_tracker() {
    let mut s = ArchitectState::default();
    let t0 = Instant::now();
    s.fetch_due(t0);
    s.apply(ArchitectUpdate::Live(Ok(live_sample())));
    assert!(matches!(s.live, Fetch::Ready(_)), "Live lands alone");
    assert_eq!(s.dashboard, Fetch::Waiting, "dashboard still out");
    let due = s.fetch_due(t0 + REFRESH);
    assert!(due.live, "Live is due again");
    assert!(!due.dashboard, "the dashboard fetch is still in flight");
}

// #10966: an `Err` must replace an earlier `Ready`, never keep it on screen.
#[test]
fn failed_live_fetch_replaces_the_earlier_sessions_on_screen() {
    let mut s = ArchitectState::default();
    s.apply(ArchitectUpdate::Live(Ok(live_sample())));
    s.apply(ArchitectUpdate::Live(Err(
        "mpm.sessions.list: timeout".into()
    )));
    s.open(Tab::Live);
    let screen = draw(&s);
    assert!(screen.contains("mpm.sessions.list: timeout"), "{screen}");
    for stale in ["tm-03", "ts-01", "engineer"] {
        assert!(!screen.contains(stale), "stale row {stale:?} still drawn");
    }
}

#[test]
fn tab_keys_cycle_and_esc_closes_detail_then_returns_home() {
    let mut s = ArchitectState::default();
    feed(&mut s, Ok(sample()), LiveSnapshot::default());
    handle_key(&mut s, key(KeyCode::BackTab));
    assert_eq!(s.tab, Tab::Live, "wraps left to the last of seven tabs");
    handle_key(&mut s, key(KeyCode::Tab));
    assert_eq!(s.tab, Tab::Decisions, "wraps right");
    for want in [
        Tab::Actions,
        Tab::Goals,
        Tab::Sessions,
        Tab::Tasks,
        Tab::Qa,
        Tab::Live,
    ] {
        handle_key(&mut s, key(KeyCode::Right));
        assert_eq!(s.tab, want);
    }
    handle_key(&mut s, key(KeyCode::Right));
    assert_eq!(s.tab, Tab::Decisions, "Live is the last tab");
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
    feed(&mut s, Ok(sample()), live_sample());
    let screen = draw(&s);
    for want in [
        "ARCHITECT",
        "Decisions 1",
        "Goals 1",
        "Q&A 1",
        "Live 2",
        " READ-ONLY ",
        "D3a",
        " OPEN ",
        "Read-only Architect view?",
        "1 answers waiting",
    ] {
        assert!(screen.contains(want), "missing {want:?}");
    }
}

#[test]
fn architect_render_says_why_when_the_store_is_not_set() {
    let mut s = ArchitectState::default();
    feed(&mut s, Err(STORE_NOT_SET.into()), LiveSnapshot::default());
    assert!(matches!(s.dashboard, Fetch::Failed(_)));
    let screen = draw(&s);
    assert!(screen.contains("--architect-store"), "{screen}");
}

fn session_row(id: &str, name: &str, workdir: &str, status: &str) -> SessionRow {
    serde_json::from_value(json!({
        "id": id, "workdir": workdir, "status": status, "tmux_name": name
    }))
    .expect("session row")
}

fn record(agent: &str, owner: &str, status: &str, age: i64) -> Record {
    serde_json::from_value(
        json!({"agent": agent, "owner": owner, "status": status, "age_secs": age}),
    )
    .expect("record")
}

const ID_A: &str = "6f1c1f2e-8b7d-4c39-9d5e-3e1f0a2b4c5d";
const ID_B: &str = "0a2b4c5d-3e1f-4c39-9d5e-6f1c1f2e8b7d";

#[test]
fn group_attaches_agents_by_owner_label() {
    let rows = [
        session_row(ID_A, "tm-03", "/w/a", "Active"),
        session_row(ID_B, "ts-01", "/w/a", "Detached"),
    ];
    let records = [
        record("engineer", "session `tm-03`", "running", 360),
        record("qa", "session `tm-03`", "queued", 60),
        record(
            "ops",
            "a session the daemon holds no record of",
            "running",
            5,
        ),
    ];
    let snap = group(&rows, &records);
    assert_eq!(snap.sessions.len(), 2);
    let agents: Vec<&str> = snap.sessions[0]
        .agents
        .iter()
        .map(|a| a.agent.as_str())
        .collect();
    assert_eq!(agents, ["engineer", "qa"]);
    assert_eq!(snap.sessions[0].id, ID_A);
    assert!(snap.sessions[1].agents.is_empty());
    assert_eq!(snap.sessions[1].status, "Detached");
}

fn live_sample() -> LiveSnapshot {
    let agent = |a: &str, st: &str, age| LiveAgent {
        agent: a.into(),
        status: st.into(),
        age_secs: age,
    };
    LiveSnapshot {
        sessions: vec![
            LiveSession {
                id: ID_A.into(),
                name: "tm-03".into(),
                status: "Active".into(),
                workdir: "/w/a".into(),
                agents: vec![
                    agent("engineer", "running", 360),
                    agent("qa", "running", 61),
                ],
                step: Some("cargo test -p trusty-mpm".into()),
            },
            LiveSession {
                id: ID_B.into(),
                name: "ts-01".into(),
                status: "Detached".into(),
                workdir: "/w/b".into(),
                agents: vec![],
                step: None,
            },
        ],
    }
}

#[test]
fn live_lines_draw_sessions_with_nested_agents() {
    let text: Vec<String> = live_lines(&live_sample(), 0)
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert!(text[0].starts_with("● tm-03"), "{text:?}");
    assert!(text[0].contains("2 agents"));
    assert!(text[1].contains("step  cargo test -p trusty-mpm"));
    assert!(text[2].starts_with("│  ├ engineer") && text[2].contains("6m"));
    assert!(text[3].starts_with("│  └ qa") && text[3].contains("1m"));
    assert!(text[4].starts_with("○ ts-01") && text[4].contains("no agents"));
    assert!(text[5].contains("step  -"), "unmanaged session: {text:?}");
}

#[test]
fn selected_session_names_the_live_row_only_on_the_live_tab() {
    let mut s = ArchitectState::default();
    feed(&mut s, Ok(sample()), live_sample());
    assert_eq!(s.selected_session(), None, "Decisions tab");
    s.open(Tab::Live);
    handle_key(&mut s, key(KeyCode::Down));
    assert_eq!(s.selected_session().as_deref(), Some(ID_B));
    assert_eq!(s.row_count(), 2);
}

#[test]
fn live_tab_draws_the_tracker_list_only() {
    let mut s = ArchitectState::default();
    feed(&mut s, Ok(sample()), live_sample());
    s.open(Tab::Live);
    let screen = draw(&s);
    for want in ["tm-03", "engineer", "ts-01", "step  -"] {
        assert!(screen.contains(want), "missing {want:?}");
    }
    assert!(!screen.contains("D3a"), "no dashboard rows on Live");
}

#[tokio::test]
async fn fetch_live_reads_the_daemon_socket_only() {
    // An HTTP client is refused before any call; nothing listens on port 1.
    let err = fetch_live(&DaemonClient::new("http://127.0.0.1:1"), None)
        .await
        .expect_err("no socket");
    assert!(err.contains("#10899"), "{err}");
}
