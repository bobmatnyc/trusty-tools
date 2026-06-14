//! Unit tests for the search TUI modules.

use super::event_loop::{ScopedReindexEvent, apply_reindex_event};
use super::nav::{
    filtered_sorted_indexes, navigate_down_visible, navigate_up_visible, new_log_lines_since,
    visible_index_ids, visible_selected_row,
};
use super::render::{
    ALL_LABEL, IndexListRow, IndexSortKey, SearchFocus, format_bytes, help_text, index_lines,
    render, sort_label, stats_lines, title_line,
};
use super::state::SearchTuiState;
use crate::monitor::dashboard::IndexRow;
use crate::monitor::search_client::ReindexEvent;
use crate::monitor::tui_common;
use crate::monitor::utils::{ActivityLog, DaemonStatus, timestamped};
use ratatui::{Terminal, backend::TestBackend};

/// A state with three indexes for selection / rendering tests.
fn sample_state() -> SearchTuiState {
    let mut state = SearchTuiState::new("http://127.0.0.1:7878");
    state.daemon_status = DaemonStatus::Online {
        version: "0.3.65".into(),
        uptime_secs: 7440,
    };
    state.indexes = vec![
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
        IndexRow {
            id: "duetto".into(),
            chunk_count: 900,
            root_path: "/tmp/duetto".into(),
            ..Default::default()
        },
    ];
    state
}

#[test]
fn test_new_state_defaults() {
    let state = SearchTuiState::new("http://127.0.0.1:7878");
    assert_eq!(state.base_url, "http://127.0.0.1:7878");
    assert!(matches!(state.daemon_status, DaemonStatus::Connecting));
    assert!(state.indexes.is_empty());
    assert_eq!(state.selected, 0);
    assert!(state.log.is_empty());
    assert!(state.input.is_empty());
    assert_eq!(state.focus, SearchFocus::List);
    assert!(!state.show_help);
}

#[test]
fn test_toggle_focus() {
    let mut state = SearchTuiState::new("http://x");
    assert_eq!(state.focus, SearchFocus::List);
    state.toggle_focus();
    assert_eq!(state.focus, SearchFocus::Input);
    state.toggle_focus();
    assert_eq!(state.focus, SearchFocus::List);
}

#[test]
fn test_selected_clamp() {
    let mut state = sample_state();
    for _ in 0..10 {
        state.select_down();
    }
    assert_eq!(state.selected, 3, "clamped to indexes.len()");
    for _ in 0..10 {
        state.select_up();
    }
    assert_eq!(state.selected, 0);
    state.selected = 3;
    state.indexes.truncate(1);
    state.clamp_selection();
    assert_eq!(state.selected, 1);
    state.indexes.clear();
    state.selected = 5;
    state.clamp_selection();
    assert_eq!(state.selected, 0);
}

#[test]
fn test_selected_id() {
    let mut state = sample_state();
    assert!(state.is_all_selected());
    assert_eq!(state.selected_id(), None);
    state.select_down();
    assert_eq!(state.selected_id(), Some("cto"));
    state.select_down();
    assert_eq!(state.selected_id(), Some("trusty"));
    state.indexes.clear();
    state.clamp_selection();
    assert_eq!(state.selected_id(), None);
}

#[test]
fn test_all_selector() {
    let mut state = sample_state();
    assert!(state.is_all_selected());
    assert_eq!(state.scope_filter(), None);
    state.select_down();
    assert!(!state.is_all_selected());
    assert_eq!(state.scope_filter(), Some("cto"));
    state.select_up();
    assert!(state.is_all_selected());
    assert_eq!(state.scope_filter(), None);

    state.sort_key = IndexSortKey::Name;
    let rows = index_lines(&state);
    assert_eq!(rows.len(), 4, "1 'All' row + 3 indexes");
    assert!(rows[0].is_all);
    assert!(rows[0].text.contains(ALL_LABEL));
    assert!(rows[0].selected, "'All' is selected by default");
    assert!(!rows[1].is_all);
    assert!(rows[1].text.contains("cto"));
}

#[test]
fn test_stats_lines() {
    let mut state = sample_state();
    let all = stats_lines(&state);
    assert!(
        all.iter()
            .any(|l| l.contains("Indexes:") && l.contains('3'))
    );
    assert!(all.iter().any(|l| l.contains("Total chunks:")));
    assert!(all.iter().any(|l| l.contains("cto")));
    assert!(all.iter().any(|l| l.contains("trusty")));

    state.select_down();
    let one = stats_lines(&state);
    assert!(
        one.iter()
            .any(|l| l.contains("Index:") && l.contains("cto"))
    );
    assert!(
        one.iter()
            .any(|l| l.contains("Chunks:") && l.contains("1,200"))
    );
    assert!(one.iter().any(|l| l.contains("/tmp/cto")));
}

#[test]
fn test_stats_lines_graph_section() {
    let mut state = sample_state();
    state.indexes[0].node_count = 4_821;
    state.indexes[0].edge_count = 12_034;
    state.indexes[0].edge_kinds = vec![
        ("CallsFunction".into(), 8_201),
        ("UsesType".into(), 2_411),
        ("Implements".into(), 1_422),
    ];
    state.indexes[0].community_count = 47;
    state.indexes[0].modularity = 0.712;
    state.select_down();
    let lines = stats_lines(&state);
    assert!(lines.iter().any(|l| l == "Graph:"));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Nodes:") && l.contains("4,821") && l.contains("Edges:"))
    );
    assert!(lines.iter().any(|l| l.contains("CallsFunction")));
    assert!(
        !lines.iter().any(|l| l.contains("Communities")),
        "Communities display must not surface in the stats panel"
    );
}

#[test]
fn test_stats_lines_no_graph_section() {
    let mut state = sample_state();
    state.indexes[0].node_count = 0;
    state.indexes[0].edge_count = 100;
    state.indexes[0].community_count = 5;
    state.select_down();
    let lines = stats_lines(&state);
    assert!(
        lines.iter().any(|l| l == "Graph:"),
        "Graph header should always appear"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("(no graph — press [r] to reindex)")),
        "empty-graph hint should appear when node_count == 0"
    );
    assert!(
        !lines.iter().any(|l| l.contains("Communities")),
        "Communities display was retired and must not surface"
    );
    assert!(
        !lines.iter().any(|l| l.contains("Nodes:")),
        "Nodes/Edges breakdown must stay hidden without nodes"
    );
}

#[test]
fn test_stats_lines_edge_kind_bars() {
    let mut state = sample_state();
    state.indexes[0].node_count = 100;
    state.indexes[0].edge_count = 200;
    state.indexes[0].edge_kinds = vec![
        ("Big".into(), 100),
        ("Half".into(), 50),
        ("Tiny".into(), 10),
    ];
    state.select_down();
    let lines = stats_lines(&state);
    let bar_lines: Vec<&String> = lines.iter().filter(|l| l.contains('█')).collect();
    assert_eq!(bar_lines.len(), 3, "expected one bar line per edge kind");
    let big_bars = bar_lines[0].matches('█').count();
    let half_bars = bar_lines[1].matches('█').count();
    let tiny_bars = bar_lines[2].matches('█').count();
    assert_eq!(big_bars, 14, "largest kind gets 14 bars");
    assert!(
        half_bars < big_bars && half_bars > tiny_bars,
        "half-sized kind ({half_bars}) sits between big ({big_bars}) and tiny ({tiny_bars})"
    );
    assert!(
        tiny_bars >= 1,
        "tiny kind should still get at least one bar"
    );
}

#[test]
fn test_log_append() {
    let mut state = SearchTuiState::new("http://x");
    for i in 0..(ActivityLog::MAX_ENTRIES + 50) {
        state.log.push(format!("event {i}"));
    }
    assert_eq!(state.log.len(), ActivityLog::MAX_ENTRIES);
}

#[test]
fn test_timestamped_format() {
    let line = timestamped("reindex started");
    assert!(line.starts_with('['));
    assert!(line.ends_with(" reindex started"));
    assert_eq!(line.as_bytes()[9], b']');
}

/// Tag a [`ReindexEvent`] with a test index id.
fn scoped(event: ReindexEvent) -> ScopedReindexEvent {
    ScopedReindexEvent {
        index_id: "cto".into(),
        event,
    }
}

#[test]
fn test_apply_reindex_event() {
    let mut state = SearchTuiState::new("http://x");
    apply_reindex_event(
        &mut state,
        scoped(ReindexEvent::Started { total_files: 1200 }),
    );
    apply_reindex_event(
        &mut state,
        scoped(ReindexEvent::Progress {
            indexed: 600,
            total_files: 1200,
        }),
    );
    apply_reindex_event(
        &mut state,
        scoped(ReindexEvent::Complete {
            total_chunks: 19_012,
            status: "complete".into(),
        }),
    );
    let lines: Vec<&String> = state.log.iter().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("reindex started: 1200 files"));
    assert!(lines[1].contains("600/1200 (50%)"));
    assert!(lines[2].contains("reindex complete: 19.0k chunks"));

    assert_eq!(state.log.tail_scoped(Some("cto"), 100).count(), 3);
    assert_eq!(state.log.tail_scoped(Some("trusty"), 100).count(), 0);

    apply_reindex_event(&mut state, scoped(ReindexEvent::Failed("disk full".into())));
    assert!(
        state
            .log
            .iter()
            .last()
            .expect("entry")
            .contains("reindex error: disk full")
    );
}

#[test]
fn test_left_panel_width() {
    use crate::monitor::tui_common::left_panel_width;
    assert_eq!(left_panel_width(200), tui_common::LEFT_PANEL_MAX);
    assert_eq!(left_panel_width(60), 20);
}

#[test]
fn test_index_lines() {
    let mut state = sample_state();
    state.sort_key = IndexSortKey::Name;
    let rows = index_lines(&state);
    assert_eq!(rows.len(), 4);
    assert!(rows[0].is_all);
    assert!(rows[0].selected);
    assert!(rows[0].text.starts_with('>'));
    assert!(rows[0].text.contains(ALL_LABEL));
    assert!(!rows[1].is_all && !rows[1].selected);
    assert!(rows[1].text.contains("cto"));
    assert!(rows[3].text.contains("trusty"));
    assert!(rows[3].text.contains("19.0k"));

    let mut empty = SearchTuiState::new("http://x");
    empty.daemon_status = DaemonStatus::Online {
        version: "0.3.65".into(),
        uptime_secs: 0,
    };
    let rows = index_lines(&empty);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].is_all);
    assert!(rows[1].text.contains("no indexes"));

    let connecting = SearchTuiState::new("http://x");
    assert!(matches!(connecting.daemon_status, DaemonStatus::Connecting));
    let rows = index_lines(&connecting);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].is_all);
    assert!(
        rows[1].text.contains("Loading…"),
        "connecting state must show Loading…, got: {:?}",
        rows[1].text
    );
}

#[test]
fn test_stats_lines_connecting_shows_loading() {
    let state = SearchTuiState::new("http://x");
    assert!(matches!(state.daemon_status, DaemonStatus::Connecting));
    let lines = stats_lines(&state);
    assert_eq!(lines, vec!["Loading…".to_string()]);
}

#[test]
fn test_truncate() {
    use crate::monitor::tui_common::truncate;
    assert_eq!(truncate("short", 12), "short");
    assert_eq!(truncate("a-very-long-index-id", 8), "a-very-…");
}

#[test]
fn test_title_line() {
    let state = sample_state();
    let title = title_line(&state);
    assert!(title.contains("trusty-search v"));
    assert!(title.contains("online"));
    assert!(title.contains("uptime: 2h 4m"));

    let mut offline = SearchTuiState::new("http://127.0.0.1:7878");
    offline.daemon_status = DaemonStatus::Offline {
        last_error: "refused".into(),
    };
    let title = title_line(&offline);
    assert!(title.contains("offline"));
    assert!(title.contains("http://127.0.0.1:7878"));
}

#[test]
fn test_help_text_lists_bindings() {
    let text = help_text();
    for token in ["Tab", "r ", "Enter", "?", "q ", "/", "s ", "g "] {
        assert!(text.contains(token), "help text missing {token}");
    }
}

#[test]
fn test_index_sort_key_cycle() {
    assert_eq!(IndexSortKey::default(), IndexSortKey::Activity);
    assert_eq!(IndexSortKey::Activity.next(), IndexSortKey::Name);
    assert_eq!(IndexSortKey::Name.next(), IndexSortKey::Count);
    assert_eq!(IndexSortKey::Count.next(), IndexSortKey::Activity);
    assert_eq!(sort_label(IndexSortKey::Activity), "Activity");
    assert_eq!(sort_label(IndexSortKey::Name), "Name");
    assert_eq!(sort_label(IndexSortKey::Count), "Chunks");
}

/// State with four indexes spanning two projects, varied chunk counts,
/// and varied last_indexed timestamps. Used by the sort / filter / group tests.
fn diverse_state() -> SearchTuiState {
    use chrono::{TimeZone, Utc};
    let mut state = SearchTuiState::new("http://127.0.0.1:7878");
    state.indexes = vec![
        IndexRow {
            id: "trusty-search".into(),
            chunk_count: 12,
            root_path: "/Users/masa/Projects/trusty-tools/trusty-search".into(),
            last_indexed: Some(Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
            ..Default::default()
        },
        IndexRow {
            id: "trusty-memory".into(),
            chunk_count: 3_775,
            root_path: "/Users/masa/Projects/trusty-tools/trusty-memory".into(),
            last_indexed: Some(Utc.with_ymd_and_hms(2026, 5, 18, 22, 29, 50).unwrap()),
            ..Default::default()
        },
        IndexRow {
            id: "claude-mpm".into(),
            chunk_count: 6_163,
            root_path: "/Users/masa/Projects/claude-mpm".into(),
            last_indexed: Some(Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap()),
            ..Default::default()
        },
        IndexRow {
            id: "notes".into(),
            chunk_count: 100,
            root_path: String::new(),
            last_indexed: None,
            ..Default::default()
        },
    ];
    state
}

#[test]
fn test_apply_sort_activity() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Activity;
    let rows = filtered_sorted_indexes(&state);
    assert_eq!(rows[0].id, "trusty-memory");
    assert_eq!(rows[1].id, "claude-mpm");
    assert_eq!(rows[2].id, "trusty-search");
    assert_eq!(rows[3].id, "notes");
}

#[test]
fn test_apply_sort_name() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Name;
    let rows = filtered_sorted_indexes(&state);
    let ids: Vec<&str> = rows.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["claude-mpm", "notes", "trusty-memory", "trusty-search"]
    );
}

#[test]
fn test_apply_sort_chunks() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Count;
    let rows = filtered_sorted_indexes(&state);
    assert_eq!(rows[0].id, "claude-mpm");
    assert_eq!(rows[1].id, "trusty-memory");
    assert_eq!(rows[2].id, "notes");
    assert_eq!(rows[3].id, "trusty-search");
}

#[test]
fn test_apply_filter() {
    let mut state = diverse_state();
    state.filter = "TRUSTY".into();
    let rows = filtered_sorted_indexes(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|i| i.id.contains("trusty")));

    state.filter = "claude-mpm".into();
    let rows = filtered_sorted_indexes(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "claude-mpm");

    state.filter = "nothing-here".into();
    assert!(filtered_sorted_indexes(&state).is_empty());

    state.filter.clear();
    assert_eq!(filtered_sorted_indexes(&state).len(), 4);
}

#[test]
fn test_index_lines_grouped() {
    let mut state = diverse_state();
    state.group_by_project = true;
    state.sort_key = IndexSortKey::Name;
    let rows = index_lines(&state);

    assert!(rows[0].is_all);

    let headers: Vec<&IndexListRow> = rows.iter().filter(|r| r.is_header).collect();
    assert!(
        !headers.is_empty(),
        "grouping must emit at least one header"
    );
    for h in &headers {
        assert!(h.text.contains("──"));
        assert!(!h.selected);
    }
    let header_text: String = headers
        .iter()
        .map(|h| h.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(header_text.contains("trusty-memory") || header_text.contains("trusty-search"));
    assert!(header_text.contains("claude-mpm"));

    state.filter = "claude".into();
    let rows = index_lines(&state);
    let headers: Vec<&IndexListRow> = rows.iter().filter(|r| r.is_header).collect();
    assert_eq!(headers.len(), 1);
    assert!(headers[0].text.contains("claude-mpm"));
}

#[test]
fn test_format_bytes() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(2_048), "2.0 KB");
    assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    assert!(format_bytes(2 * 1024 * 1024 * 1024).ends_with("GB"));
}

#[test]
fn test_scroll_offset() {
    let mut state = sample_state();
    for row in 0..=state.last_row() {
        state.selected = row;
        state.sync_scroll(8);
        assert_eq!(state.scroll_offset, 0, "no scroll while the list fits");
    }

    state.indexes = (0..40)
        .map(|n| IndexRow {
            id: format!("idx-{n}"),
            chunk_count: 1,
            root_path: String::new(),
            ..Default::default()
        })
        .collect();
    let window = 5;
    for row in 0..=state.last_row() {
        state.selected = row;
        state.sync_scroll(window);
        assert!(
            row >= state.scroll_offset && row < state.scroll_offset + window,
            "row {row} must be inside [{}, {})",
            state.scroll_offset,
            state.scroll_offset + window,
        );
    }
    assert_eq!(state.scroll_offset, state.last_row() + 1 - window);

    for row in (0..=state.last_row()).rev() {
        state.selected = row;
        state.sync_scroll(window);
        assert!(
            row >= state.scroll_offset && row < state.scroll_offset + window,
            "row {row} must stay visible while scrolling up",
        );
    }
    assert_eq!(state.scroll_offset, 0, "back at the top");
}

#[test]
fn test_visible_index_ids() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Name;
    let ids = visible_index_ids(&state);
    assert_eq!(ids[0], tui_common::ALL_SENTINEL);
    assert_eq!(
        &ids[1..],
        &[
            "claude-mpm".to_string(),
            "notes".to_string(),
            "trusty-memory".to_string(),
            "trusty-search".to_string(),
        ]
    );

    state.filter = "trusty".into();
    let ids = visible_index_ids(&state);
    assert_eq!(ids[0], tui_common::ALL_SENTINEL);
    assert_eq!(ids.len(), 3, "All + 2 trusty-* indexes");
}

#[test]
fn test_navigate_visible() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Name;
    assert_eq!(state.selected, 0);
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("claude-mpm"));
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("notes"));
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("trusty-memory"));
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("trusty-search"));
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("trusty-search"));
    navigate_up_visible(&mut state);
    assert_eq!(state.selected_id(), Some("trusty-memory"));
    navigate_up_visible(&mut state);
    navigate_up_visible(&mut state);
    navigate_up_visible(&mut state);
    assert!(state.is_all_selected());
    navigate_up_visible(&mut state);
    assert!(state.is_all_selected());

    state.filter = "trusty".into();
    state.selected = 0;
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("trusty-memory"));
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("trusty-search"));
    navigate_down_visible(&mut state);
    assert_eq!(state.selected_id(), Some("trusty-search"));
}

#[test]
fn test_visible_selected_row_follows_sort() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Name;
    let pos = state
        .indexes
        .iter()
        .position(|i| i.id == "claude-mpm")
        .expect("index");
    state.selected = pos + 1;
    assert_eq!(state.selected, 3, "original index puts claude-mpm at 3");
    assert_eq!(
        visible_selected_row(&state),
        1,
        "claude-mpm is the first non-All row after Name sort",
    );

    state.selected = 0;
    assert_eq!(visible_selected_row(&state), 0);

    state.sort_key = IndexSortKey::Count;
    let pos = state
        .indexes
        .iter()
        .position(|i| i.id == "notes")
        .expect("index");
    state.selected = pos + 1;
    assert_eq!(visible_selected_row(&state), 3);
}

#[test]
fn test_visible_selected_row_follows_group() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Name;
    state.group_by_project = true;
    let pos = state
        .indexes
        .iter()
        .position(|i| i.id == "trusty-memory")
        .expect("index");
    state.selected = pos + 1;
    let expected = index_lines(&state)
        .iter()
        .position(|row| row.selected)
        .expect("trusty-memory must appear in the grouped layout");
    assert_eq!(visible_selected_row(&state), expected);
    assert!(expected > 0, "highlight is not on the All row");
}

#[test]
fn test_sync_scroll_to_follows_sorted_order() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Name;
    state.selected = 1;
    let visible_row = visible_selected_row(&state);
    assert_eq!(visible_row, 4, "trusty-search is the last visible row");
    state.sync_scroll_to(visible_row, 3);
    assert_eq!(state.scroll_offset, 2);
}

#[test]
fn test_clamp_to_visible() {
    let mut state = diverse_state();
    state.sort_key = IndexSortKey::Name;
    let pos = state
        .indexes
        .iter()
        .position(|i| i.id == "claude-mpm")
        .expect("index");
    state.selected = pos + 1;
    state.filter = "trusty".into();
    state.clamp_to_visible();
    assert_eq!(state.selected, 0, "selection dropped to All");

    state.filter = "trusty".into();
    let pos = state
        .indexes
        .iter()
        .position(|i| i.id == "trusty-memory")
        .expect("index");
    state.selected = pos + 1;
    state.clamp_to_visible();
    assert_eq!(state.selected_id(), Some("trusty-memory"));
}

#[test]
fn test_render_smoke() {
    let mut state = sample_state();
    state.log.push("daemon started");
    state.log.push_scoped("cto", "reindex started: 1200 files");
    state
        .log
        .push_scoped("trusty", "search \"fn embed\" → 5 results");
    state.input = "fn authenticate".into();
    state.focus = SearchFocus::Input;
    for (w, h) in [(120u16, 30u16), (80, 24)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("render (All) must not panic");
    }
    state.selected = 1;
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &mut state))
        .expect("render (single index) must not panic");

    state.indexes = (0..60)
        .map(|n| IndexRow {
            id: format!("idx-{n}"),
            chunk_count: 100,
            root_path: String::new(),
            ..Default::default()
        })
        .collect();
    state.selected = state.last_row();
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &mut state))
        .expect("overflowing list render must not panic");
    assert!(state.scroll_offset > 0, "long list scrolled to the cursor");

    state.show_help = true;
    state.daemon_status = DaemonStatus::Connecting;
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &mut state))
        .expect("help render must not panic");
}

#[test]
fn test_new_log_lines_since_watermark() {
    let lines: Vec<String> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(new_log_lines_since(&lines, Some("c")), &["d", "e"]);
    assert_eq!(new_log_lines_since(&lines, Some("e")), &[] as &[String]);
    assert_eq!(new_log_lines_since(&lines, Some("z")), lines.as_slice());
    assert_eq!(new_log_lines_since(&lines, None), lines.as_slice());
    assert!(new_log_lines_since(&[], Some("a")).is_empty());
}

#[test]
fn test_push_new_log_lines_skips_first_poll() {
    let mut state = SearchTuiState::new("http://x");
    assert!(state.log_first_poll);
    let lines: Vec<String> = ["info: daemon started", "info: index loaded"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if state.log_first_poll {
        state.log_watermark = lines.last().cloned();
        state.log_first_poll = false;
    }
    assert!(!state.log_first_poll);
    assert!(state.log.is_empty());
    let lines2: Vec<String> = [
        "info: daemon started",
        "info: index loaded",
        "info: watch triggered",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let new = new_log_lines_since(&lines2, state.log_watermark.as_deref());
    for line in new {
        state.log.push(line.clone());
    }
    state.log_watermark = lines2.last().cloned();
    assert_eq!(state.log.len(), 1);
    assert!(
        state
            .log
            .iter()
            .next()
            .map(|l| l.contains("watch triggered"))
            .unwrap_or(false)
    );
}
