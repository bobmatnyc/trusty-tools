//! Tests for the combined search + memory health screen.
//!
//! Why: the health screen's wire projections, panel line builders, spinner
//! helpers, and ratatui render path are all covered here in one place so
//! a single `cargo test -p trusty-mpm` validates the whole surface.
//! What: unit tests for types, formatters, projections, state methods, and a
//! smoke-render test using `ratatui::backend::TestBackend`.
//! Test: this file is the test suite.

use super::activity::{DREAMING_SPINNER, INDEXING_SPINNER};
use super::probes::{
    project_edge_kinds, project_log_tail, project_memory_counts, project_palace_rows,
};
use super::screen::format_count_suffix;
use super::types::HealthWire;
use super::{
    CollectionRow, DEFAULT_SEARCH_URL, Daemon, HealthScreen, HealthTab, HealthUpdate,
    LOG_BUFFER_CAP, LogBuffer, PalaceActivity, PanelData, PanelState, activity_color, ascii_bar,
    client_for, collections_lines, collections_lines_at_tick, format_bytes, format_count,
    format_relative_time, format_rss, format_uptime, format_with_commas, header_lines,
    health_tab_lines, index_tab_lines, memory_panel_lines, palace_activity, palace_index_tab_lines,
    render, search_panel_lines, service_name, spinner_frame, tab_bar,
};
use ratatui::{Terminal, backend::TestBackend, style::Color};

/// Test-only stand-in for the trusty-memory daemon address: a port nobody
/// binds, used wherever a test previously relied on the removed
/// `DEFAULT_MEMORY_URL` constant (issue #2030 — the real default is now
/// resolved dynamically via discovery, not a fixed literal).
const TEST_MEMORY_URL: &str = "http://127.0.0.1:19999";

/// A sample online search payload for rendering tests.
fn sample_search() -> PanelData {
    PanelData {
        version: "0.3.67".into(),
        rss_mb: 1280,
        cpu_pct: 4.0,
        uptime_secs: 3720,
        disk_bytes: 2_469_606_195,
        count_a: 3,
        count_b: 71_000,
        count_c: 0,
        count_d: 0,
    }
}

/// A sample online memory payload for rendering tests.
fn sample_memory() -> PanelData {
    PanelData {
        version: "0.1.56".into(),
        rss_mb: 410,
        cpu_pct: 1.0,
        uptime_secs: 3720,
        disk_bytes: 104_857_600,
        count_a: 2,
        count_b: 8_400,
        count_c: 14,
        count_d: 1_200,
    }
}

#[test]
fn default_urls_are_local() {
    // Only the search daemon has a fixed local convention port; the memory
    // default is resolved dynamically via discovery (issue #2030), so there
    // is no `DEFAULT_MEMORY_URL` constant left to assert on.
    assert_eq!(DEFAULT_SEARCH_URL, "http://127.0.0.1:7878");
}

#[test]
fn health_wire_deserializes_partial_payload() {
    // A daemon on an older build omits the issue-#35 resource fields; the
    // wire shape must still deserialize, defaulting the missing fields.
    let wire: HealthWire = serde_json::from_value(serde_json::json!({
        "status": "ok",
        "version": "0.3.67",
    }))
    .expect("partial health payload must deserialize");
    assert_eq!(wire.version, "0.3.67");
    assert_eq!(wire.rss_mb, 0);
    assert_eq!(wire.uptime_secs, 0);
}

#[test]
fn project_memory_counts_reads_status_fields() {
    let status = serde_json::json!({
        "palace_count": 2,
        "total_vectors": 8400,
        "total_drawers": 14,
        "total_kg_triples": 1200,
    });
    assert_eq!(project_memory_counts(&status), (2, 8_400, 14, 1_200));
    // Absent fields default to zero rather than panicking.
    assert_eq!(project_memory_counts(&serde_json::json!({})), (0, 0, 0, 0));
}

#[test]
fn format_uptime_is_compact() {
    assert_eq!(format_uptime(0), "0h 0m");
    assert_eq!(format_uptime(3720), "1h 2m");
    assert_eq!(format_uptime(7_380), "2h 3m");
}

#[test]
fn format_bytes_picks_unit() {
    assert_eq!(format_bytes(512), "512B");
    assert_eq!(format_bytes(2_048), "2.0KB");
    assert_eq!(format_bytes(5_242_880), "5.0MB");
    assert_eq!(format_bytes(2_469_606_195), "2.3GB");
}

#[test]
fn format_rss_picks_unit() {
    assert_eq!(format_rss(410), "410MB");
    assert_eq!(format_rss(1_023), "1023MB");
    // 1280 MB / 1024 = 1.25 GB → one-decimal rounding yields 1.2GB.
    assert_eq!(format_rss(1_280), "1.2GB");
    assert_eq!(format_rss(1_536), "1.5GB");
}

#[test]
fn format_count_abbreviates_large() {
    assert_eq!(format_count(3), "3");
    assert_eq!(format_count(9_999), "9999");
    assert_eq!(format_count(71_000), "71.0k");
}

#[test]
fn panel_state_is_online() {
    assert!(PanelState::Online(PanelData::default()).is_online());
    assert!(!PanelState::Connecting.is_online());
    assert!(
        !PanelState::Offline {
            last_error: "x".into()
        }
        .is_online()
    );
}

#[test]
fn search_panel_lines_format_fields() {
    let lines = search_panel_lines(
        &PanelState::Online(sample_search()),
        "http://127.0.0.1:7878",
    );
    assert!(lines.iter().any(|l| l.contains("SEARCH [●] v0.3.67")));
    // 1280 MB renders as 1.2GB (1280 / 1024 = 1.25, rounded to one place).
    assert!(lines.iter().any(|l| l.contains("RSS: 1.2GB")));
    assert!(lines.iter().any(|l| l.contains("CPU: 4%")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Indexes: 3") && l.contains("Chunks: 71.0k"))
    );
    assert!(lines.iter().any(|l| l.contains("Disk: 2.3GB")));
    assert!(lines.iter().any(|l| l.contains("[S]start [X]stop")));
}

#[test]
fn memory_panel_lines_format_fields() {
    let lines = memory_panel_lines(
        &PanelState::Online(sample_memory()),
        "http://127.0.0.1:7071",
    );
    assert!(lines.iter().any(|l| l.contains("MEMORY [●] v0.1.56")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Palaces: 2") && l.contains("Vectors: 8400"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Drawers: 14") && l.contains("KG: 1200"))
    );
}

#[test]
fn panel_lines_render_each_state() {
    // Connecting: a single placeholder line naming the target.
    let connecting = search_panel_lines(&PanelState::Connecting, "http://x");
    assert_eq!(connecting.len(), 1);
    assert!(connecting[0].contains("connecting"));

    // Offline: carries the unreachable address and the captured error.
    let offline = memory_panel_lines(
        &PanelState::Offline {
            last_error: "connection refused".into(),
        },
        "http://127.0.0.1:7071",
    );
    assert!(offline.iter().any(|l| l.contains("OFFLINE")));
    assert!(offline.iter().any(|l| l.contains("connection refused")));
    assert!(offline.iter().any(|l| l.contains("retrying every 5s")));
    assert!(offline.iter().any(|l| l.contains("[S]start [X]stop")));
}

#[test]
fn online_panel_renders_missing_version_safely() {
    // A daemon that omitted `version` must not render `v` with nothing.
    let mut data = sample_search();
    data.version.clear();
    let lines = search_panel_lines(&PanelState::Online(data), "http://x");
    assert!(lines.iter().any(|l| l.contains("SEARCH [●] ?")));
}

#[test]
fn new_screen_starts_connecting() {
    let screen = HealthScreen::new("http://a", "http://b");
    assert_eq!(screen.search, PanelState::Connecting);
    assert_eq!(screen.memory, PanelState::Connecting);
    assert_eq!(screen.search_url, "http://a");
    assert_eq!(screen.focus, Daemon::Search);
}

#[test]
fn toggle_focus_cycles_panels() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    assert_eq!(screen.focus, Daemon::Search);
    screen.toggle_focus();
    assert_eq!(screen.focus, Daemon::Memory);
    screen.toggle_focus();
    assert_eq!(screen.focus, Daemon::Search);
}

#[test]
fn apply_update_routes_to_panel() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.apply_update(HealthUpdate {
        daemon: Daemon::Search,
        state: PanelState::Online(sample_search()),
    });
    assert!(screen.search.is_online());
    // The memory panel must be untouched by a search-targeted update.
    assert_eq!(screen.memory, PanelState::Connecting);

    screen.apply_update(HealthUpdate {
        daemon: Daemon::Memory,
        state: PanelState::Offline {
            last_error: "timeout".into(),
        },
    });
    assert!(matches!(screen.memory, PanelState::Offline { .. }));
    // The search panel must still be online.
    assert!(screen.search.is_online());
}

#[test]
fn focused_url_follows_focus() {
    let mut screen = HealthScreen::new("http://search", "http://memory");
    assert_eq!(screen.focused_url(), "http://search");
    screen.toggle_focus();
    assert_eq!(screen.focused_url(), "http://memory");
}

#[test]
fn health_client_stores_base_url() {
    let client = client_for(Daemon::Search, "http://127.0.0.1:7878");
    assert_eq!(client.base_url(), "http://127.0.0.1:7878");
}

#[tokio::test]
async fn poll_unreachable_daemon_is_offline() {
    // Port 0 is never bound; a poll against it must yield Offline carrying
    // a non-empty error string rather than panicking.
    let client = client_for(Daemon::Memory, "http://127.0.0.1:0");
    match client.poll().await {
        PanelState::Offline { last_error } => assert!(!last_error.is_empty()),
        other => panic!("expected Offline, got {other:?}"),
    }
}

#[test]
fn tab_default_is_health() {
    // The redesigned screen opens on the Health tab.
    assert_eq!(HealthTab::default(), HealthTab::Health);
}

#[test]
fn tab_switch_keys_route() {
    // `set_tab` stores the requested tab and auto-focuses the search
    // input only when switching to the Search tab (per the spec).
    let mut screen = HealthScreen::new("http://a", "http://b");
    assert!(!screen.search_input_focused);
    screen.set_tab(HealthTab::Logs);
    assert_eq!(screen.tab, HealthTab::Logs);
    assert!(!screen.search_input_focused);
    screen.set_tab(HealthTab::Search);
    assert_eq!(screen.tab, HealthTab::Search);
    assert!(screen.search_input_focused);
    screen.set_tab(HealthTab::Health);
    assert!(!screen.search_input_focused);
}

#[test]
fn log_buffer_starts_empty() {
    let buf = LogBuffer::new();
    assert!(buf.lines.is_empty());
    assert!(buf.auto_scroll);
    assert_eq!(buf.scroll_offset, 0);
}

#[test]
fn log_buffer_evicts_oldest() {
    // Pushing past the cap evicts the oldest entries; the newest stay.
    let mut buf = LogBuffer::new();
    for i in 0..(LOG_BUFFER_CAP + 10) {
        buf.push(format!("line {i}"));
    }
    assert_eq!(buf.lines.len(), LOG_BUFFER_CAP);
    assert_eq!(buf.lines.front().map(String::as_str), Some("line 10"));
    assert_eq!(
        buf.lines.back().map(String::as_str),
        Some(format!("line {}", LOG_BUFFER_CAP + 9)).as_deref()
    );
}

#[test]
fn log_buffer_replace_caps_at_limit() {
    // `replace` swaps in a fresh tail but never holds more than the cap.
    let mut buf = LogBuffer::new();
    let huge: Vec<String> = (0..(LOG_BUFFER_CAP * 2)).map(|i| format!("l{i}")).collect();
    buf.replace(huge, Some(9999));
    assert_eq!(buf.lines.len(), LOG_BUFFER_CAP);
    assert_eq!(buf.total_seen, 9999);
}

#[test]
fn log_buffer_scroll_clamps() {
    // ↑ disables auto-scroll and saturates at the top; ↓ re-enables auto-scroll
    // when it returns to the tail.
    let mut buf = LogBuffer::new();
    for i in 0..5 {
        buf.push(format!("l{i}"));
    }
    assert!(buf.auto_scroll);
    buf.scroll_up();
    assert!(!buf.auto_scroll);
    assert_eq!(buf.scroll_offset, 1);
    for _ in 0..20 {
        buf.scroll_up();
    }
    // Saturates at `lines.len() - 1` (4 here).
    assert_eq!(buf.scroll_offset, 4);
    for _ in 0..10 {
        buf.scroll_down();
    }
    assert_eq!(buf.scroll_offset, 0);
    assert!(buf.auto_scroll);
}

#[test]
fn log_buffer_snap_to_tail() {
    let mut buf = LogBuffer::new();
    for i in 0..3 {
        buf.push(format!("l{i}"));
    }
    buf.scroll_up();
    buf.scroll_up();
    assert!(!buf.auto_scroll);
    buf.snap_to_tail();
    assert_eq!(buf.scroll_offset, 0);
    assert!(buf.auto_scroll);
}

#[test]
fn project_log_tail_reads_fields() {
    let body = serde_json::json!({
        "lines": ["a", "b", "c"],
        "total": 42u64,
    });
    let (lines, total) = project_log_tail(&body);
    assert_eq!(
        lines,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    assert_eq!(total, 42);
    // Absent `total` falls back to `lines.len()`.
    let body = serde_json::json!({ "lines": ["a", "b"] });
    let (_, total) = project_log_tail(&body);
    assert_eq!(total, 2);
    // Absent `lines` yields an empty list.
    let body = serde_json::json!({});
    let (lines, _) = project_log_tail(&body);
    assert!(lines.is_empty());
}

#[test]
fn project_palace_rows_reads_palaces() {
    let list = serde_json::json!([
        { "name": "default", "vector_count": 8400u64, "kg_triple_count": 1200u64 },
        { "name": "work",    "vector_count": 0u64,    "kg_triple_count": 42u64 },
    ]);
    let rows = project_palace_rows(&list);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "default");
    assert_eq!(rows[0].count, 8400);
    assert_eq!(rows[0].kg_count, 1200);
    assert!(rows[0].ok);
    assert!(rows[0].note.is_empty());
    assert_eq!(rows[1].id, "work");
    assert_eq!(rows[1].count, 0);
    assert_eq!(rows[1].kg_count, 42);
    assert!(project_palace_rows(&serde_json::json!({})).is_empty());
}

#[test]
fn project_palace_rows_filters_empty() {
    let list = serde_json::json!([
        { "name": "live",    "vector_count": 10u64, "kg_triple_count": 0u64 },
        { "name": "empty",   "vector_count": 0u64,  "kg_triple_count": 0u64 },
        { "name": "kg-only", "vector_count": 0u64,  "kg_triple_count": 5u64 },
        { "name": "both",    "vector_count": 3u64,  "kg_triple_count": 7u64 },
        { "name": "stub" },
    ]);
    let rows = project_palace_rows(&list);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["live", "kg-only", "both"]);
}

#[test]
fn format_count_suffix_handles_zero_and_value() {
    assert_eq!(format_count_suffix(0, 'v'), "--v");
    assert_eq!(format_count_suffix(0, 'g'), "--g");
    assert_eq!(format_count_suffix(42, 'v'), "42v");
    assert_eq!(format_count_suffix(12_345, 'g'), "12.3kg");
}

#[test]
fn service_name_matches_focus() {
    assert_eq!(service_name(Daemon::Search), "trusty-search");
    assert_eq!(service_name(Daemon::Memory), "trusty-memory");
}

#[test]
fn header_lines_show_focus_summary() {
    let mut screen = HealthScreen::new(DEFAULT_SEARCH_URL, TEST_MEMORY_URL);
    screen.search = PanelState::Online(sample_search());
    let lines = header_lines(&screen);
    assert!(lines[0].contains("trusty-search"));
    assert!(lines[0].contains("v0.3.67"));
    assert!(lines[0].contains("ONLINE"));
    assert!(lines[1].contains("RSS:"));
    assert!(lines[1].contains("CPU:"));
    assert!(lines[1].contains("Uptime:"));

    screen.search = PanelState::Offline {
        last_error: "connection refused".into(),
    };
    let lines = header_lines(&screen);
    assert!(lines[0].contains("OFFLINE"));
    assert!(lines[1].contains("connection refused"));
}

#[test]
fn tab_bar_marks_active() {
    let bar = tab_bar(HealthTab::Logs);
    let active_count = bar.iter().filter(|(_, a)| *a).count();
    assert_eq!(active_count, 1);
    let logs_active = bar
        .iter()
        .find(|(l, _)| l.contains("LOGS"))
        .map(|(_, a)| *a)
        .unwrap();
    assert!(logs_active);
}

#[test]
fn collection_row_default_is_empty() {
    let row = CollectionRow::default();
    assert!(row.id.is_empty());
    assert_eq!(row.count, 0);
    assert!(!row.ok);
}

#[test]
fn collections_lines_format_each_row() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.search_collections = vec![
        CollectionRow {
            id: "cto".into(),
            count: 1_200,
            note: "indexed".into(),
            ok: true,
            ..Default::default()
        },
        CollectionRow {
            id: "trusty".into(),
            count: 18_994,
            note: "indexed".into(),
            ok: true,
            ..Default::default()
        },
    ];
    let lines = collections_lines(&screen);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with(">"));
    assert!(lines[1].starts_with(" "));
    assert!(lines[0].contains("cto"));
    assert!(lines[1].contains("trusty"));
}

#[test]
fn collections_lines_show_graph_count_for_memory() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.focus = Daemon::Memory;
    screen.memory_collections = vec![CollectionRow {
        id: "default".into(),
        count: 12,
        kg_count: 34,
        ok: true,
        ..Default::default()
    }];
    let lines = collections_lines(&screen);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains("12v"),
        "expected `12v` in {line:?}",
        line = lines[0]
    );
    assert!(
        lines[0].contains("34g"),
        "expected `34g` in {line:?}",
        line = lines[0]
    );
    assert!(lines[0].contains("default"));
    assert!(!lines[0].contains("ready"));
    assert!(!lines[0].contains("["));
}

#[test]
fn collections_lines_show_dashes_for_zero_counts() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.focus = Daemon::Memory;
    screen.memory_collections = vec![CollectionRow {
        id: "empty".into(),
        count: 0,
        kg_count: 0,
        ok: true,
        ..Default::default()
    }];
    let lines = collections_lines(&screen);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains("--v"),
        "expected `--v` for zero vectors in {line:?}",
        line = lines[0]
    );
    assert!(
        lines[0].contains("--g"),
        "expected `--g` for zero KG triples in {line:?}",
        line = lines[0]
    );
    assert!(!lines[0].contains("0v"));
    assert!(!lines[0].contains("0g"));
}

#[test]
fn collections_for_focus() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.search_collections = vec![CollectionRow {
        id: "i".into(),
        ..Default::default()
    }];
    screen.memory_collections = vec![
        CollectionRow {
            id: "p1".into(),
            ..Default::default()
        },
        CollectionRow {
            id: "p2".into(),
            ..Default::default()
        },
    ];
    assert_eq!(screen.focused_collections().len(), 1);
    screen.toggle_focus();
    assert_eq!(screen.focused_collections().len(), 2);
}

#[test]
fn select_collection_saturates() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.search_collections = vec![
        CollectionRow {
            id: "a".into(),
            ..Default::default()
        },
        CollectionRow {
            id: "b".into(),
            ..Default::default()
        },
    ];
    screen.select_collection_down();
    assert_eq!(screen.selected_collection, 1);
    screen.select_collection_down();
    assert_eq!(screen.selected_collection, 1);
    screen.select_collection_up();
    assert_eq!(screen.selected_collection, 0);
    screen.select_collection_up();
    assert_eq!(screen.selected_collection, 0);
}

#[test]
fn select_collection_clamps_after_shrink() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.search_collections = vec![CollectionRow {
        id: "a".into(),
        ..Default::default()
    }];
    screen.selected_collection = 99;
    screen.clamp_collection_selection();
    assert_eq!(screen.selected_collection, 0);
    screen.search_collections.clear();
    screen.clamp_collection_selection();
    assert_eq!(screen.selected_collection, 0);
}

#[test]
fn ascii_bar_fills_proportionally() {
    assert_eq!(ascii_bar(0.0, 10), "░░░░░░░░░░");
    assert_eq!(ascii_bar(1.0, 10), "██████████");
    let half = ascii_bar(0.5, 10);
    assert_eq!(half.chars().filter(|c| *c == '█').count(), 5);
    assert_eq!(half.chars().filter(|c| *c == '░').count(), 5);
    assert_eq!(ascii_bar(2.0, 4), "████");
    assert_eq!(ascii_bar(-1.0, 4), "░░░░");
}

#[test]
fn health_tab_lines_show_gauges() {
    let mut screen = HealthScreen::new(DEFAULT_SEARCH_URL, TEST_MEMORY_URL);
    screen.search = PanelState::Online(sample_search());
    let lines = health_tab_lines(&screen);
    assert!(lines.iter().any(|l| l.starts_with("Memory ")));
    assert!(lines.iter().any(|l| l.starts_with("Disk   ")));
    assert!(lines.iter().any(|l| l.contains("Embedder")));
    assert!(lines.iter().any(|l| l.contains("CoreML")));
}

#[test]
fn format_relative_time_handles_known_offsets() {
    assert_eq!(format_relative_time(None), "never");
    assert_eq!(format_relative_time(Some("not-a-time")), "never");
    let now = chrono::Utc::now();
    let mk = |d: chrono::Duration| (now - d).to_rfc3339();
    assert_eq!(
        format_relative_time(Some(&mk(chrono::Duration::minutes(5)))),
        "5m ago"
    );
    assert_eq!(
        format_relative_time(Some(&mk(chrono::Duration::hours(2)))),
        "2h ago"
    );
    assert_eq!(
        format_relative_time(Some(&mk(chrono::Duration::days(3)))),
        "3d ago"
    );
    let future = (now + chrono::Duration::minutes(5)).to_rfc3339();
    assert_eq!(format_relative_time(Some(&future)), "just now");
}

#[test]
fn project_edge_kinds_sorts_desc() {
    let stats = serde_json::json!({
        "edge_kinds": {
            "CallsFunction": 8201u64,
            "Implements":    1422u64,
            "UsesType":      2411u64,
        }
    });
    let kinds = project_edge_kinds(&stats);
    assert_eq!(
        kinds,
        vec![
            ("CallsFunction".to_string(), 8201),
            ("UsesType".to_string(), 2411),
            ("Implements".to_string(), 1422),
        ]
    );
    assert!(project_edge_kinds(&serde_json::json!({})).is_empty());
}

#[test]
fn collections_lines_show_relative_time() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    let ts = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    screen.search_collections = vec![CollectionRow {
        id: "trusty".into(),
        count: 71_000,
        note: String::new(),
        ok: true,
        last_indexed: Some(ts),
        ..Default::default()
    }];
    let lines = collections_lines(&screen);
    assert!(lines[0].contains("[5m ago]"));
}

#[test]
fn index_tab_lines_show_graph_stats() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.search_collections = vec![CollectionRow {
        id: "trusty".into(),
        count: 71_000,
        ok: true,
        disk_bytes: 2_469_606_195,
        node_count: 4_821,
        edge_count: 12_034,
        community_count: 47,
        modularity: 0.712,
        has_context_embedding: true,
        ..Default::default()
    }];
    let lines = index_tab_lines(&screen);
    assert!(lines.iter().any(|l| l.starts_with("Chunks:")));
    assert!(lines.iter().any(|l| l.contains("Disk: 2.3GB")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Context:") && l.contains("embedded"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Nodes:") && l.contains("Edges:"))
    );
    assert!(
        !lines.iter().any(|l| l.contains("Modularity")),
        "Modularity must not appear in the index tab"
    );
    assert!(
        !lines.iter().any(|l| l.contains("Communities")),
        "Communities section must not appear in the index tab"
    );
}

#[test]
fn index_tab_lines_show_edge_kind_bars() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.search_collections = vec![CollectionRow {
        id: "trusty".into(),
        edge_kinds: vec![
            ("CallsFunction".to_string(), 8_201),
            ("UsesType".to_string(), 2_411),
            ("Implements".to_string(), 1_422),
        ],
        ..Default::default()
    }];
    let lines = index_tab_lines(&screen);
    let calls = lines.iter().find(|l| l.contains("CallsFunction")).unwrap();
    let impls = lines.iter().find(|l| l.contains("Implements")).unwrap();
    let calls_blocks = calls.chars().filter(|c| *c == '█').count();
    let impls_blocks = impls.chars().filter(|c| *c == '█').count();
    assert!(calls_blocks >= impls_blocks);
    assert!(calls_blocks > 0);
}

#[test]
fn index_tab_lines_empty_when_no_selection() {
    let screen = HealthScreen::new("http://a", "http://b");
    let lines = index_tab_lines(&screen);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("no collection selected"));
}

#[test]
fn palace_activity_from_recent_write() {
    let mut row = CollectionRow {
        id: "p".into(),
        count: 10,
        ok: true,
        ..Default::default()
    };
    assert_eq!(palace_activity(&row), PalaceActivity::Idle);

    let mut bad = row.clone();
    bad.ok = false;
    assert_eq!(palace_activity(&bad), PalaceActivity::Error);

    row.last_write_at = Some((chrono::Utc::now() - chrono::Duration::seconds(2)).to_rfc3339());
    assert_eq!(palace_activity(&row), PalaceActivity::Indexing);

    row.last_write_at = Some((chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339());
    assert_eq!(palace_activity(&row), PalaceActivity::Active);

    row.last_write_at = Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
    assert_eq!(palace_activity(&row), PalaceActivity::Idle);

    row.last_write_at = Some("not-a-date".into());
    assert_eq!(palace_activity(&row), PalaceActivity::Idle);
}

/// Why: When the memory daemon flags a palace as compacting the MEMORY
/// tab must render the dreaming spinner regardless of how recently the
/// palace was written to. The compacting flag must therefore dominate
/// the timestamp heuristic.
/// What: Sets `is_compacting = true` on a row with a fresh write and
/// asserts the activity classifier returns `Dreaming`.
/// Test: This test itself.
#[test]
fn palace_activity_marks_compacting_as_dreaming() {
    let row = CollectionRow {
        id: "p".into(),
        count: 10,
        ok: true,
        is_compacting: true,
        last_write_at: Some(chrono::Utc::now().to_rfc3339()),
        ..Default::default()
    };
    assert_eq!(palace_activity(&row), PalaceActivity::Dreaming);

    let bad = CollectionRow {
        ok: false,
        is_compacting: true,
        ..row.clone()
    };
    assert_eq!(palace_activity(&bad), PalaceActivity::Error);
}

/// Why: The new wire fields (`node_count`, `edge_count`,
/// `community_count`, `is_compacting`) must surface verbatim through the
/// projection so the renderer can rely on the row alone.
/// What: Feeds a synthetic `/api/v1/palaces` payload carrying every new
/// field and asserts each surfaces with the expected typed value.
/// Test: This test itself.
#[test]
fn project_palace_rows_reads_is_compacting() {
    let list = serde_json::json!([
        {
            "name": "main",
            "vector_count": 100u64,
            "kg_triple_count": 50u64,
            "node_count": 42u64,
            "edge_count": 84u64,
            "community_count": 3u64,
            "is_compacting": true,
        },
    ]);
    let rows = project_palace_rows(&list);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].node_count, 42);
    assert_eq!(rows[0].edge_count, 84);
    assert_eq!(rows[0].community_count, 3);
    assert!(rows[0].is_compacting);
}

#[test]
fn spinner_frame_for_each_state() {
    assert_eq!(spinner_frame(PalaceActivity::Idle, 0), None);
    assert_eq!(spinner_frame(PalaceActivity::Error, 0), Some('✗'));
    assert_eq!(spinner_frame(PalaceActivity::Active, 0), Some('⠿'));
    assert!(spinner_frame(PalaceActivity::Indexing, 0).is_some());
    assert!(spinner_frame(PalaceActivity::Dreaming, 0).is_some());
}

#[test]
fn spinner_frame_cycles_through_indexing_frames() {
    let f0 = spinner_frame(PalaceActivity::Indexing, 0).unwrap();
    let f1 = spinner_frame(PalaceActivity::Indexing, 1).unwrap();
    let f_wrap = spinner_frame(PalaceActivity::Indexing, INDEXING_SPINNER.len()).unwrap();
    assert_ne!(f0, f1);
    assert_eq!(f0, f_wrap);
}

#[test]
fn spinner_frame_cycles_through_dreaming_frames() {
    let f0 = spinner_frame(PalaceActivity::Dreaming, 0).unwrap();
    let f_wrap = spinner_frame(PalaceActivity::Dreaming, DREAMING_SPINNER.len()).unwrap();
    assert_eq!(f0, f_wrap);
}

#[test]
fn activity_colour_is_distinct_per_state() {
    assert_eq!(activity_color(PalaceActivity::Idle), Color::Reset);
    let colours = [
        activity_color(PalaceActivity::Indexing),
        activity_color(PalaceActivity::Dreaming),
        activity_color(PalaceActivity::Active),
        activity_color(PalaceActivity::Error),
    ];
    for c in &colours {
        assert_ne!(*c, Color::Reset);
    }
    let mut sorted = colours.to_vec();
    sorted.sort_by_key(|c| format!("{c:?}"));
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        4,
        "every non-idle state needs a unique colour"
    );
}

#[test]
fn collections_lines_at_tick_shows_indexing_spinner() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.focus = Daemon::Memory;
    screen.memory_collections = vec![CollectionRow {
        id: "fresh".into(),
        count: 1,
        kg_count: 0,
        ok: true,
        last_write_at: Some((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()),
        ..Default::default()
    }];
    let lines = collections_lines_at_tick(&screen, 0);
    assert_eq!(lines.len(), 1);
    let expected = INDEXING_SPINNER[0];
    assert!(
        lines[0].contains(expected),
        "expected indexing spinner {expected} in {line:?}",
        line = lines[0],
    );
}

#[test]
fn collections_lines_at_tick_idle_palace_has_no_spinner() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.focus = Daemon::Memory;
    screen.memory_collections = vec![CollectionRow {
        id: "idle".into(),
        count: 1,
        kg_count: 0,
        ok: true,
        last_write_at: None,
        ..Default::default()
    }];
    let lines = collections_lines_at_tick(&screen, 0);
    for ch in INDEXING_SPINNER.iter().chain(DREAMING_SPINNER.iter()) {
        assert!(
            !lines[0].contains(*ch),
            "idle palace must not show spinner glyph {ch} in {line:?}",
            line = lines[0],
        );
    }
    assert!(!lines[0].contains('⠿'));
    assert!(!lines[0].contains('✗'));
}

#[test]
fn format_with_commas_groups_thousands() {
    assert_eq!(format_with_commas(0), "0");
    assert_eq!(format_with_commas(42), "42");
    assert_eq!(format_with_commas(1_234), "1,234");
    assert_eq!(format_with_commas(1_234_567), "1,234,567");
    assert_eq!(format_with_commas(1_000_000_000), "1,000,000,000");
}

#[test]
fn project_palace_rows_reads_extended_fields() {
    let ts = chrono::Utc::now().to_rfc3339();
    let list = serde_json::json!([
        {
            "name": "main",
            "vector_count": 100u64,
            "kg_triple_count": 50u64,
            "drawer_count": 7u64,
            "wing_count": 3u64,
            "last_write_at": ts,
        },
    ]);
    let rows = project_palace_rows(&list);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].drawer_count, 7);
    assert_eq!(rows[0].wing_count, 3);
    assert_eq!(rows[0].last_write_at.as_deref(), Some(ts.as_str()));
}

#[test]
fn palace_index_tab_lines_shows_graph_section() {
    let row = CollectionRow {
        id: "main".into(),
        count: 12_345,
        kg_count: 6_789,
        drawer_count: 42,
        wing_count: 3,
        ok: true,
        ..Default::default()
    };
    let lines = palace_index_tab_lines(&row);
    assert!(lines.iter().any(|l| l.starts_with("Vectors:")));
    assert!(lines.iter().any(|l| l.contains("12,345")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Drawers:") && l.contains("42"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Wings:") && l.contains("3"))
    );
    assert!(lines.iter().any(|l| l.contains("Knowledge Graph")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Triples:") && l.contains("6,789"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Nodes:") && l.contains("N/A"))
    );
    assert!(
        !lines.iter().any(|l| l.contains("Communities")),
        "Communities display must not appear in palace index tab"
    );
    assert!(lines.iter().any(|l| l.contains("Activity")));
    assert!(lines.iter().any(|l| l.starts_with("State:")));
}

#[test]
fn palace_index_tab_lines_formats_last_write() {
    let ts = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
    let row = CollectionRow {
        id: "main".into(),
        count: 1,
        ok: true,
        last_write_at: Some(ts),
        ..Default::default()
    };
    let lines = palace_index_tab_lines(&row);
    let last_line = lines
        .iter()
        .find(|l| l.starts_with("Last write:"))
        .expect("must include Last write line");
    assert!(last_line.contains("ago") || last_line.contains("just now"));
    let state_line = lines
        .iter()
        .find(|l| l.starts_with("State:"))
        .expect("must include State line");
    assert!(state_line.contains("active"));

    let idle = CollectionRow {
        id: "x".into(),
        count: 1,
        ok: true,
        last_write_at: None,
        ..Default::default()
    };
    let lines = palace_index_tab_lines(&idle);
    assert!(lines.iter().any(|l| l.contains("never")));
    assert!(lines.iter().any(|l| l.contains("idle")));
}

#[test]
fn index_tab_lines_routes_to_palace_when_focus_memory() {
    let mut screen = HealthScreen::new("http://a", "http://b");
    screen.focus = Daemon::Memory;
    screen.memory_collections = vec![CollectionRow {
        id: "main".into(),
        count: 10,
        kg_count: 5,
        ok: true,
        ..Default::default()
    }];
    let lines = index_tab_lines(&screen);
    assert!(lines.iter().any(|l| l.contains("Knowledge Graph")));
    assert!(
        !lines
            .iter()
            .any(|l| l == "-- Graph ----------------------------------------------")
    );
}

#[test]
fn render_health_smoke() {
    let mut screen = HealthScreen::new(DEFAULT_SEARCH_URL, TEST_MEMORY_URL);
    screen.search = PanelState::Online(sample_search());
    screen.memory = PanelState::Offline {
        last_error: "connection refused".into(),
    };
    screen.focus = Daemon::Memory;
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &screen))
        .expect("health render must not panic");
}
