use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::{Constraint, Direction},
    style::Color,
};

/// A trusty-search payload with two indexes for rendering tests.
fn sample_search() -> SearchData {
    SearchData {
        version: "0.3.63".into(),
        uptime_secs: 7440,
        indexes: vec![
            IndexRow {
                id: "cto".into(),
                chunk_count: 1_200,
                root_path: "/tmp/cto".into(),
                ..Default::default()
            },
            IndexRow {
                id: "trusty".into(),
                chunk_count: 18_994,
                root_path: "/tmp/trusty".into(),
                ..Default::default()
            },
        ],
    }
}

/// A trusty-memory payload with two palaces for rendering tests.
fn sample_memory() -> MemoryData {
    MemoryData {
        version: "0.4.2".into(),
        palace_count: 2,
        total_drawers: 14,
        total_vectors: 8_400,
        total_kg_triples: 1_200,
        palaces: vec![
            PalaceRow {
                id: "default".into(),
                name: "default".into(),
                vector_count: 8_400,
                ..Default::default()
            },
            PalaceRow {
                id: "work".into(),
                name: "work".into(),
                vector_count: 0,
                ..Default::default()
            },
        ],
    }
}

#[test]
fn test_layout_wide() {
    // A terminal at or above the threshold splits the panels horizontally
    // into two equal halves.
    let (direction, constraints) = panel_layout(140);
    assert_eq!(direction, Direction::Horizontal);
    assert_eq!(
        constraints,
        [Constraint::Percentage(50), Constraint::Percentage(50)]
    );
    // Exactly at the boundary still counts as wide.
    assert_eq!(panel_layout(WIDE_LAYOUT_MIN_COLS).0, Direction::Horizontal);
}

#[test]
fn test_layout_narrow() {
    // Below the threshold the panels stack vertically.
    let (direction, constraints) = panel_layout(80);
    assert_eq!(direction, Direction::Vertical);
    assert_eq!(
        constraints,
        [Constraint::Percentage(50), Constraint::Percentage(50)]
    );
}

#[test]
fn test_offline_panel_renders() {
    // An offline panel must produce renderable lines (carrying the error)
    // and must not panic when fed to the frame renderer.
    let search: DaemonPanel<SearchData> = DaemonPanel {
        status: PanelStatus::Offline {
            last_error: "connection refused".into(),
        },
        base_url: "http://127.0.0.1:7878".into(),
    };
    let lines = search_panel_lines(&search);
    assert!(lines.iter().any(|l| l.contains("connection refused")));
    assert!(lines.iter().any(|l| l.contains("unreachable")));

    let memory: DaemonPanel<MemoryData> = DaemonPanel {
        status: PanelStatus::Offline {
            last_error: "timeout".into(),
        },
        base_url: "http://127.0.0.1:7070".into(),
    };
    let mlines = memory_panel_lines(&memory);
    assert!(mlines.iter().any(|l| l.contains("timeout")));

    // A whole-frame render with both panels offline must not panic.
    let state = DashboardState {
        search,
        memory,
        focus: Focus::Search,
        show_help: false,
        last_action: None,
    };
    let backend = TestBackend::new(130, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &state))
        .expect("offline render must not panic");
}

#[test]
fn test_uptime_format() {
    assert_eq!(format_uptime(7440), "2h 4m");
    assert_eq!(format_uptime(0), "0h 0m");
    assert_eq!(format_uptime(59), "0h 0m");
    assert_eq!(format_uptime(3600), "1h 0m");
    assert_eq!(format_uptime(3661), "1h 1m");
}

#[test]
fn test_format_count() {
    // Small counts are comma-grouped and exact.
    assert_eq!(format_count(0), "0");
    assert_eq!(format_count(900), "900");
    assert_eq!(format_count(1_200), "1,200");
    assert_eq!(format_count(9_999), "9,999");
    // Counts at or above 10k are abbreviated with one decimal.
    assert_eq!(format_count(19_400), "19.4k");
    assert_eq!(format_count(10_000), "10.0k");
}

#[test]
fn test_search_total_chunks() {
    assert_eq!(sample_search().total_chunks(), 20_194);
    assert_eq!(SearchData::default().total_chunks(), 0);
}

#[test]
fn test_search_panel_renders() {
    let panel = DaemonPanel {
        status: PanelStatus::Online(sample_search()),
        base_url: "http://127.0.0.1:7878".into(),
    };
    let lines = search_panel_lines(&panel);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Uptime:") && l.contains("2h 4m"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Indexes:") && l.contains('2'))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("cto") && l.contains("1,200"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("trusty") && l.contains("19.0k"))
    );
}

#[test]
fn test_memory_panel_renders() {
    let panel = DaemonPanel {
        status: PanelStatus::Online(sample_memory()),
        base_url: "http://127.0.0.1:7070".into(),
    };
    let lines = memory_panel_lines(&panel);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Palaces:") && l.contains('2'))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Vectors:") && l.contains("8,400"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("KG triples:") && l.contains("1,200"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("default") && l.contains("8,400"))
    );
}

#[test]
fn test_toggle_focus() {
    let mut state = DashboardState::new("http://a", "http://b");
    assert_eq!(state.focus, Focus::Search);
    state.toggle_focus();
    assert_eq!(state.focus, Focus::Memory);
    state.toggle_focus();
    assert_eq!(state.focus, Focus::Search);
}

#[test]
fn test_new_state_starts_connecting() {
    let state = DashboardState::new("http://a", "http://b");
    assert!(matches!(state.search.status, PanelStatus::Connecting));
    assert!(matches!(state.memory.status, PanelStatus::Connecting));
    assert_eq!(state.search.base_url, "http://a");
    assert_eq!(state.memory.base_url, "http://b");
}

#[test]
fn test_panel_starts_connecting() {
    let panel: DaemonPanel<SearchData> = DaemonPanel::new("http://x");
    assert!(matches!(panel.status, PanelStatus::Connecting));
    assert_eq!(panel.base_url, "http://x");
}

#[test]
fn test_panel_status_is_online() {
    let online: PanelStatus<u32> = PanelStatus::Online(1);
    assert!(online.is_online());
    let offline: PanelStatus<u32> = PanelStatus::Offline {
        last_error: "x".into(),
    };
    assert!(!offline.is_online());
    let connecting: PanelStatus<u32> = PanelStatus::Connecting;
    assert!(!connecting.is_online());
}

#[test]
fn test_reindex_target() {
    let mut state = DashboardState::new("http://a", "http://b");
    // No target while connecting.
    assert_eq!(state.reindex_target(), None);
    // Online search panel, focused: first index id is the target.
    state.search.status = PanelStatus::Online(sample_search());
    assert_eq!(state.reindex_target(), Some("cto".to_string()));
    // Memory focus disables the reindex target.
    state.focus = Focus::Memory;
    assert_eq!(state.reindex_target(), None);
}

#[test]
fn test_status_badge() {
    let online: PanelStatus<u32> = PanelStatus::Online(0);
    assert_eq!(status_badge(&online), ('●', "ONLINE", Color::Green));
    let connecting: PanelStatus<u32> = PanelStatus::Connecting;
    assert_eq!(
        status_badge(&connecting),
        ('◌', "CONNECTING", Color::Yellow)
    );
    let offline: PanelStatus<u32> = PanelStatus::Offline {
        last_error: "x".into(),
    };
    assert_eq!(status_badge(&offline), ('○', "OFFLINE", Color::Red));
}

#[test]
fn test_truncate() {
    assert_eq!(truncate("short", 16), "short");
    assert_eq!(truncate("0123456789abcdefghij", 8), "0123456…");
    assert_eq!(truncate("exactlyeight", 12), "exactlyeight");
}

#[test]
fn test_palace_row_project() {
    // Auto-registered description → basename of the path.
    let row = PalaceRow {
        id: "trusty-search".into(),
        name: "trusty-search".into(),
        description: Some("Auto-registered from /Users/masa/Projects/trusty-search".into()),
        ..Default::default()
    };
    assert_eq!(row.project(), "trusty-search");

    // No description → falls back to the palace name.
    let bare = PalaceRow {
        id: "p1".into(),
        name: "notes".into(),
        ..Default::default()
    };
    assert_eq!(bare.project(), "notes");

    // Unexpected description shape → fall back to the name.
    let weird = PalaceRow {
        id: "p2".into(),
        name: "weird".into(),
        description: Some("hand-made palace".into()),
        ..Default::default()
    };
    assert_eq!(weird.project(), "weird");

    // Trailing slash should not yield an empty basename.
    let trailing = PalaceRow {
        id: "p3".into(),
        name: "fallback".into(),
        description: Some("Auto-registered from /tmp/".into()),
        ..Default::default()
    };
    assert_eq!(trailing.project(), "fallback");
}

#[test]
fn test_index_row_project() {
    // A populated root_path yields its basename.
    let row = IndexRow {
        id: "trusty".into(),
        root_path: "/Users/masa/Projects/trusty-tools".into(),
        ..Default::default()
    };
    assert_eq!(row.project(), "trusty-tools");

    // An empty root_path falls back to the index id.
    let bare = IndexRow {
        id: "p1".into(),
        ..Default::default()
    };
    assert_eq!(bare.project(), "p1");

    // A trailing-slash root should still resolve to the directory name.
    let trailing = IndexRow {
        id: "p2".into(),
        root_path: "/tmp/repo/".into(),
        ..Default::default()
    };
    assert_eq!(trailing.project(), "repo");
}

#[test]
fn test_help_text_lists_bindings() {
    let text = help_text();
    for token in ["Tab", "r ", "?", "Esc", "q "] {
        assert!(text.contains(token), "help text missing {token}");
    }
}

#[test]
fn test_render_smoke() {
    // A full render of a fully-online dashboard, wide and narrow, must not
    // panic and must exercise the help overlay path.
    let state = DashboardState {
        search: DaemonPanel {
            status: PanelStatus::Online(sample_search()),
            base_url: "http://127.0.0.1:7878".into(),
        },
        memory: DaemonPanel {
            status: PanelStatus::Online(sample_memory()),
            base_url: "http://127.0.0.1:7070".into(),
        },
        focus: Focus::Memory,
        show_help: true,
        last_action: Some("reindex queued".into()),
    };
    for (w, h) in [(140u16, 30u16), (90, 40)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &state))
            .expect("render must not panic");
    }
}
