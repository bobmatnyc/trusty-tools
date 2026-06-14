//! Unit tests for `memory_tui` — state, view builders, and render smoke.
//!
//! Why: keeps the 1300-line test suite in its own file (classified as a
//! test/benchmark file by the SLOC gate) so `mod.rs` stays within the 500-SLOC
//! production cap while preserving full test coverage.
//! What: mirrors the original inline `#[cfg(test)] mod tests { … }` block;
//! every test function is unchanged from the pre-split file.
//! Test: this file IS the test suite — `cargo test -p trusty-common
//! --features monitor-tui` picks it up automatically.

use super::*;
use crate::monitor::dashboard::{MemoryData, PalaceRow};
use crate::monitor::memory_client::{DrawerInfo, MemoryDetail, MemoryEvent};
use crate::monitor::tui_common;
use crate::monitor::tui_common::{left_panel_width, truncate};
use crate::monitor::utils::{ActivityLog, DaemonStatus, timestamped};
use ratatui::{Terminal, backend::TestBackend};
use state::backoff_delay;

/// A state with two palaces and aggregate stats for rendering tests.
fn sample_state() -> MemoryTuiState {
    let mut state = MemoryTuiState::new("http://127.0.0.1:7070");
    state.daemon_status = DaemonStatus::Online {
        version: "0.1.54".into(),
        uptime_secs: 0,
    };
    state.palaces = vec![
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
            // Non-zero KG triple count keeps the palace visible — the
            // empty-palace filter drops rows with zero vectors AND zero
            // triples.
            kg_triple_count: 42,
            ..Default::default()
        },
    ];
    state.status = Some(MemoryData {
        version: "0.1.54".into(),
        palace_count: 2,
        total_drawers: 14,
        total_vectors: 8_400,
        total_kg_triples: 1_200,
        palaces: state.palaces.clone(),
    });
    state
}

#[test]
fn test_new_state_defaults() {
    let state = MemoryTuiState::new("http://127.0.0.1:7070");
    assert_eq!(state.base_url, "http://127.0.0.1:7070");
    assert!(matches!(state.daemon_status, DaemonStatus::Connecting));
    assert!(state.status.is_none());
    assert!(state.palaces.is_empty());
    assert_eq!(state.selected, 0);
    assert!(state.log.is_empty());
    assert_eq!(state.focus, MemoryFocus::List);
    assert!(!state.show_help);
}

#[test]
fn test_toggle_focus() {
    let mut state = MemoryTuiState::new("http://x");
    assert_eq!(state.focus, MemoryFocus::List);
    state.toggle_focus();
    assert_eq!(state.focus, MemoryFocus::Input);
    state.toggle_focus();
    assert_eq!(state.focus, MemoryFocus::List);
}

#[test]
fn test_selected_clamp() {
    let mut state = sample_state();
    // The list has 1 ("All") + 2 palaces = 3 rows; the cursor stops at 2.
    for _ in 0..10 {
        state.select_down();
    }
    assert_eq!(state.selected, 2, "clamped to palaces.len()");
    for _ in 0..10 {
        state.select_up();
    }
    assert_eq!(state.selected, 0);
    // A shrunk palace list re-clamps the cursor (1 "All" + 1 palace = 1).
    state.selected = 2;
    state.palaces.truncate(1);
    state.clamp_selection();
    assert_eq!(state.selected, 1);
    // An empty list leaves only the "All" row at cursor 0.
    state.palaces.clear();
    state.selected = 9;
    state.clamp_selection();
    assert_eq!(state.selected, 0);
}

#[test]
fn test_selected_id() {
    let mut state = sample_state();
    // Cursor 0 is "All" — no single palace.
    assert!(state.is_all_selected());
    assert_eq!(state.selected_id(), None);
    // Cursor 1 is the first palace.
    state.select_down();
    assert_eq!(state.selected_id(), Some("default"));
    state.select_down();
    assert_eq!(state.selected_id(), Some("work"));
    state.palaces.clear();
    state.clamp_selection();
    assert_eq!(state.selected_id(), None);
}

#[test]
fn test_all_selector() {
    let mut state = sample_state();
    // The default selection is the "All palaces" row.
    assert!(state.is_all_selected());
    assert_eq!(state.scope_filter(), None);
    // Moving down off row 0 picks a single palace and a scoped filter.
    state.select_down();
    assert!(!state.is_all_selected());
    assert_eq!(state.scope_filter(), Some("default"));
    state.select_up();
    assert!(state.is_all_selected());

    // The palace list always leads with the "All" row.
    let rows = palace_lines(&state);
    assert_eq!(rows.len(), 3, "1 'All' row + 2 palaces");
    assert!(rows[0].is_all);
    assert!(rows[0].text.contains(ALL_LABEL));
    assert!(rows[0].selected, "'All' is selected by default");
    assert!(!rows[1].is_all);
    assert!(rows[1].text.contains("default"));
}

#[test]
fn test_stats_lines() {
    let mut state = sample_state();
    // "All" selected → aggregate totals + per-palace breakdown.
    let all = stats_lines(&state);
    assert!(
        all.iter()
            .any(|l| l.contains("Palaces:") && l.contains('2'))
    );
    assert!(
        all.iter()
            .any(|l| l.contains("Vectors:") && l.contains("8,400"))
    );
    assert!(
        all.iter()
            .any(|l| l.contains("KG triples:") && l.contains("1,200"))
    );
    assert!(all.iter().any(|l| l.contains("default")));

    // A single palace selected → that palace's detail.
    state.select_down(); // cursor 1 → default
    let one = stats_lines(&state);
    assert!(
        one.iter()
            .any(|l| l.contains("Palace:") && l.contains("default"))
    );
    assert!(
        one.iter()
            .any(|l| l.contains("Vectors:") && l.contains("8,400"))
    );
    assert!(one.iter().any(|l| l.contains("Id:")));
}

#[test]
fn test_stats_lines_connecting_shows_loading() {
    let state = MemoryTuiState::new("http://x");
    assert!(matches!(state.daemon_status, DaemonStatus::Connecting));
    let lines = stats_lines(&state);
    assert_eq!(lines, vec!["Loading…".to_string()]);
}

#[test]
fn test_palace_row_display() {
    let palace = PalaceRow {
        id: "default".into(),
        name: "default".into(),
        vector_count: 8_400,
        ..Default::default()
    };
    let row = palace_row(&palace, true);
    assert!(row.starts_with("  "), "leading spinner+space: {row}");
    assert!(row.contains("default"));
    assert!(row.contains("8,400v"));

    let unselected = palace_row(&palace, false);
    assert!(unselected.starts_with(' '), "unselected: {unselected}");

    let nameless = PalaceRow {
        id: "p-xyz".into(),
        name: String::new(),
        vector_count: 0,
        ..Default::default()
    };
    let row = palace_row(&nameless, false);
    assert!(row.contains("p-xyz"));
    assert!(row.contains("0v"));

    let long = PalaceRow {
        id: "x".into(),
        name: "a-very-long-palace-name".into(),
        vector_count: 1,
        ..Default::default()
    };
    assert!(palace_row(&long, false).contains('…'));
}

#[test]
fn test_palace_lines() {
    let state = sample_state();
    let rows = palace_lines(&state);
    assert_eq!(rows.len(), 3);
    assert!(rows[0].is_all);
    assert!(rows[0].selected);
    assert!(rows[0].text.contains(ALL_LABEL));
    assert!(!rows[1].is_all && !rows[1].selected);
    assert!(rows[1].text.contains("default"));
    assert!(rows[2].text.contains("work"));

    let mut empty = MemoryTuiState::new("http://x");
    empty.daemon_status = DaemonStatus::Online {
        version: "0.1.54".into(),
        uptime_secs: 0,
    };
    let rows = palace_lines(&empty);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].is_all);
    assert!(rows[1].text.contains("no palaces"));

    let connecting = MemoryTuiState::new("http://x");
    assert!(matches!(connecting.daemon_status, DaemonStatus::Connecting));
    let rows = palace_lines(&connecting);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].is_all);
    assert!(
        rows[1].text.contains("Loading…"),
        "connecting state must show Loading…, got: {:?}",
        rows[1].text
    );
}

#[test]
fn test_log_append_dream() {
    let mut state = MemoryTuiState::new("http://x");
    apply_memory_event(
        &mut state,
        MemoryEvent::DreamCompleted {
            merged: 3,
            pruned: 1,
            compacted: 0,
        },
    );
    let lines: Vec<&String> = state.log.iter().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("SSE: dream_completed"));
    assert!(lines[0].starts_with('['), "header is timestamped");
    assert!(lines[1].contains("merged: 3"));
    assert!(lines[1].contains("pruned: 1"));
    assert!(lines[1].contains("compacted: 0"));
    assert!(lines[1].starts_with("  "));
}

#[test]
fn test_apply_memory_event() {
    let mut state = MemoryTuiState::new("http://x");
    apply_memory_event(
        &mut state,
        MemoryEvent::DrawerAdded {
            palace_id: "default".into(),
            drawer_count: 14,
            content_preview: "How the migration system handles…".into(),
        },
    );
    apply_memory_event(
        &mut state,
        MemoryEvent::DrawerDeleted {
            palace_id: "work".into(),
            drawer_count: 2,
        },
    );
    apply_memory_event(
        &mut state,
        MemoryEvent::PalaceCreated {
            name: "notes".into(),
        },
    );
    let lines: Vec<&String> = state.log.iter().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("drawer added → default (14)"));
    assert!(lines[0].contains("\"How the migration system handles…\""));
    assert!(lines[1].contains("drawer deleted → work (2)"));
    assert!(lines[2].contains("palace created → notes"));

    let default_feed: Vec<&String> = state.log.tail_scoped(Some("default"), 100).collect();
    assert_eq!(default_feed.len(), 2);
    assert!(
        default_feed
            .iter()
            .any(|l| l.contains("drawer added → default"))
    );
    assert!(
        default_feed
            .iter()
            .any(|l| l.contains("palace created → notes"))
    );
    assert!(
        !default_feed
            .iter()
            .any(|l| l.contains("drawer deleted → work"))
    );
}

#[test]
fn test_log_capacity() {
    let mut state = MemoryTuiState::new("http://x");
    for i in 0..(ActivityLog::MAX_ENTRIES + 30) {
        state.log.push(format!("event {i}"));
    }
    assert_eq!(state.log.len(), ActivityLog::MAX_ENTRIES);
}

#[test]
fn test_timestamped_format() {
    let line = timestamped("recall complete");
    assert!(line.starts_with('['));
    assert!(line.ends_with(" recall complete"));
    assert_eq!(line.as_bytes()[9], b']');
}

#[test]
fn test_left_panel_width() {
    assert_eq!(left_panel_width(200), tui_common::LEFT_PANEL_MAX);
    assert_eq!(left_panel_width(60), 20);
}

#[test]
fn test_truncate() {
    assert_eq!(truncate("work", 10), "work");
    assert_eq!(truncate("a-very-long-palace", 8), "a-very-…");
}

#[test]
fn test_title_line() {
    let state = sample_state();
    let title = title_line(&state);
    assert!(title.contains("trusty-memory v0.1.54"));
    assert!(title.contains("online"));

    let mut offline = MemoryTuiState::new("http://127.0.0.1:7070");
    offline.daemon_status = DaemonStatus::Offline {
        last_error: "refused".into(),
    };
    let title = title_line(&offline);
    assert!(title.contains("offline"));
    assert!(title.contains("http://127.0.0.1:7070"));
}

#[test]
fn test_palace_sort_key_cycle() {
    assert_eq!(PalaceSortKey::default(), PalaceSortKey::Activity);
    assert_eq!(PalaceSortKey::Activity.next(), PalaceSortKey::Name);
    assert_eq!(PalaceSortKey::Name.next(), PalaceSortKey::Count);
    assert_eq!(PalaceSortKey::Count.next(), PalaceSortKey::Activity);
    assert_eq!(sort_label(PalaceSortKey::Activity), "Activity");
    assert_eq!(sort_label(PalaceSortKey::Name), "Name");
    assert_eq!(sort_label(PalaceSortKey::Count), "Vectors");
}

fn diverse_state() -> MemoryTuiState {
    use chrono::{TimeZone, Utc};
    let mut state = MemoryTuiState::new("http://127.0.0.1:7070");
    state.palaces = vec![
        PalaceRow {
            id: "trusty-search".into(),
            name: "trusty-search".into(),
            vector_count: 12,
            last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
            description: Some(
                "Auto-registered from /Users/masa/Projects/trusty-tools/trusty-search".into(),
            ),
            ..Default::default()
        },
        PalaceRow {
            id: "trusty-memory".into(),
            name: "trusty-memory".into(),
            vector_count: 3_775,
            last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 18, 22, 29, 50).unwrap()),
            description: Some(
                "Auto-registered from /Users/masa/Projects/trusty-tools/trusty-memory".into(),
            ),
            ..Default::default()
        },
        PalaceRow {
            id: "claude-mpm".into(),
            name: "claude-mpm".into(),
            vector_count: 6_163,
            last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap()),
            description: Some("Auto-registered from /Users/masa/Projects/claude-mpm".into()),
            ..Default::default()
        },
        PalaceRow {
            id: "notes".into(),
            name: "notes".into(),
            vector_count: 100,
            last_write_at: None,
            description: None,
            ..Default::default()
        },
    ];
    state
}

#[test]
fn test_apply_sort_activity() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Activity;
    let rows = filtered_sorted_palaces(&state);
    assert_eq!(rows[0].id, "trusty-memory");
    assert_eq!(rows[1].id, "claude-mpm");
    assert_eq!(rows[2].id, "trusty-search");
    assert_eq!(rows[3].id, "notes");
}

#[test]
fn test_apply_sort_name() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Name;
    let rows = filtered_sorted_palaces(&state);
    let names: Vec<&str> = rows.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["claude-mpm", "notes", "trusty-memory", "trusty-search"]
    );
}

#[test]
fn test_apply_sort_vectors() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Count;
    let rows = filtered_sorted_palaces(&state);
    assert_eq!(rows[0].id, "claude-mpm");
    assert_eq!(rows[1].id, "trusty-memory");
    assert_eq!(rows[2].id, "notes");
    assert_eq!(rows[3].id, "trusty-search");
}

#[test]
fn test_apply_filter() {
    let mut state = diverse_state();
    state.filter = "TRUSTY".into();
    let rows = filtered_sorted_palaces(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|p| p.name.contains("trusty")));

    state.filter = "claude-mpm".into();
    let rows = filtered_sorted_palaces(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "claude-mpm");

    state.filter = "nothing-here".into();
    assert!(filtered_sorted_palaces(&state).is_empty());

    state.filter.clear();
    assert_eq!(filtered_sorted_palaces(&state).len(), 4);
}

#[test]
fn test_palace_lines_grouped() {
    let mut state = diverse_state();
    state.group_by_project = true;
    state.sort_key = PalaceSortKey::Name;
    let rows = palace_lines(&state);

    assert!(rows[0].is_all);

    let headers: Vec<&PalaceListRow> = rows.iter().filter(|r| r.is_header).collect();
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
    let rows = palace_lines(&state);
    let headers: Vec<&PalaceListRow> = rows.iter().filter(|r| r.is_header).collect();
    assert_eq!(headers.len(), 1);
    assert!(headers[0].text.contains("claude-mpm"));
}

#[test]
fn test_help_text_lists_bindings() {
    let text = help_text();
    for token in ["Tab", "d ", "Enter", "?", "q ", "/", "s ", "g "] {
        assert!(text.contains(token), "help text missing {token}");
    }
}

#[test]
fn test_scroll_offset() {
    let mut state = sample_state();
    for row in 0..=state.last_row() {
        state.selected = row;
        state.sync_scroll(6);
        assert_eq!(state.scroll_offset, 0, "no scroll while the list fits");
    }

    state.palaces = (0..40)
        .map(|n| PalaceRow {
            id: format!("p-{n}"),
            name: format!("palace-{n}"),
            vector_count: 1,
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
fn test_visible_palace_ids() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Name;
    let ids = visible_palace_ids(&state);
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
    let ids = visible_palace_ids(&state);
    assert_eq!(ids[0], tui_common::ALL_SENTINEL);
    assert_eq!(ids.len(), 3, "All + 2 trusty-* palaces");
}

#[test]
fn test_navigate_visible() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Name;
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
    state.sort_key = PalaceSortKey::Name;
    let pos = state
        .palaces
        .iter()
        .position(|p| p.id == "claude-mpm")
        .expect("palace");
    state.selected = pos + 1;
    assert_eq!(state.selected, 3, "original index puts claude-mpm at 3");
    assert_eq!(
        visible_selected_row(&state),
        1,
        "claude-mpm is the first non-All row after Name sort",
    );

    state.selected = 0;
    assert_eq!(visible_selected_row(&state), 0);

    state.sort_key = PalaceSortKey::Count;
    let pos = state
        .palaces
        .iter()
        .position(|p| p.id == "notes")
        .expect("palace");
    state.selected = pos + 1;
    assert_eq!(visible_selected_row(&state), 3);
}

#[test]
fn test_visible_selected_row_follows_group() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Name;
    state.group_by_project = true;
    let pos = state
        .palaces
        .iter()
        .position(|p| p.id == "trusty-memory")
        .expect("palace");
    state.selected = pos + 1;
    let expected = palace_lines(&state)
        .iter()
        .position(|row| row.selected)
        .expect("trusty-memory must appear in the grouped layout");
    assert_eq!(visible_selected_row(&state), expected);
    assert!(expected > 0, "highlight is not on the All row");
}

#[test]
fn test_sync_scroll_to_follows_sorted_order() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Name;
    state.selected = 1;
    let visible_row = visible_selected_row(&state);
    assert_eq!(visible_row, 4, "trusty-search is the last visible row");
    state.sync_scroll_to(visible_row, 3);
    assert_eq!(state.scroll_offset, 2);
}

#[test]
fn test_clamp_to_visible() {
    let mut state = diverse_state();
    state.sort_key = PalaceSortKey::Name;
    let pos = state
        .palaces
        .iter()
        .position(|p| p.id == "claude-mpm")
        .expect("palace");
    state.selected = pos + 1;
    state.filter = "trusty".into();
    state.clamp_to_visible();
    assert_eq!(state.selected, 0, "selection dropped to All");

    state.filter = "trusty".into();
    let pos = state
        .palaces
        .iter()
        .position(|p| p.id == "trusty-memory")
        .expect("palace");
    state.selected = pos + 1;
    state.clamp_to_visible();
    assert_eq!(state.selected_id(), Some("trusty-memory"));
}

#[test]
fn test_render_smoke() {
    let mut state = sample_state();
    state.log.push("SSE: dream_completed");
    state
        .log
        .push_scoped("default", "recall \"auth flow\" → 3 results");
    state.input = "auth flow".into();
    state.focus = MemoryFocus::Input;
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
        .expect("render (single palace) must not panic");

    state.palaces = (0..60)
        .map(|n| PalaceRow {
            id: format!("p-{n}"),
            name: format!("palace-{n}"),
            vector_count: 100,
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
fn test_palace_activity_state() {
    use chrono::{TimeZone, Utc};
    let now = Utc.with_ymd_and_hms(2026, 5, 22, 12, 0, 0).unwrap();

    let mut p = PalaceRow {
        id: "a".into(),
        name: "a".into(),
        vector_count: 1,
        is_compacting: true,
        ..Default::default()
    };
    assert_eq!(palace_activity_state(&p, now), PalaceActivity::Dreaming);

    p.is_compacting = false;
    p.last_write_at = Some(now - chrono::Duration::seconds(3));
    assert_eq!(palace_activity_state(&p, now), PalaceActivity::Indexing);

    p.last_write_at = Some(now - chrono::Duration::seconds(30));
    assert_eq!(palace_activity_state(&p, now), PalaceActivity::Active);

    p.last_write_at = Some(now - chrono::Duration::seconds(120));
    assert_eq!(palace_activity_state(&p, now), PalaceActivity::Idle);

    p.last_write_at = None;
    assert_eq!(palace_activity_state(&p, now), PalaceActivity::Idle);

    assert_eq!(PalaceActivity::Idle.prefix(0), ' ');
    assert_eq!(PalaceActivity::Active.prefix(0), '⠿');
    assert_eq!(PalaceActivity::Error.prefix(0), '✗');
    let i0 = PalaceActivity::Indexing.prefix(0);
    let i1 = PalaceActivity::Indexing.prefix(1);
    assert_ne!(i0, i1, "indexing spinner advances per tick");
    let d0 = PalaceActivity::Dreaming.prefix(0);
    let d1 = PalaceActivity::Dreaming.prefix(1);
    assert_ne!(d0, d1, "dreaming spinner advances per tick");

    assert_eq!(PalaceActivity::Idle.color(), None);
    assert_eq!(
        PalaceActivity::Indexing.color(),
        Some(ratatui::style::Color::Yellow)
    );
    assert_eq!(
        PalaceActivity::Active.color(),
        Some(ratatui::style::Color::Cyan)
    );
    assert_eq!(
        PalaceActivity::Dreaming.color(),
        Some(ratatui::style::Color::Magenta)
    );
    assert_eq!(
        PalaceActivity::Error.color(),
        Some(ratatui::style::Color::Red)
    );
}

#[test]
fn test_filter_empty_palaces() {
    let mut state = MemoryTuiState::new("http://x");
    state.palaces = vec![
        PalaceRow {
            id: "vec-only".into(),
            name: "vec-only".into(),
            vector_count: 10,
            ..Default::default()
        },
        PalaceRow {
            id: "kg-only".into(),
            name: "kg-only".into(),
            kg_triple_count: 5,
            ..Default::default()
        },
        PalaceRow {
            id: "drawer-only".into(),
            name: "drawer-only".into(),
            drawer_count: 18,
            ..Default::default()
        },
        PalaceRow {
            id: "empty".into(),
            name: "empty".into(),
            ..Default::default()
        },
    ];
    let visible = filtered_sorted_palaces(&state);
    assert_eq!(visible.len(), 3, "only truly empty palace dropped");
    assert!(visible.iter().any(|p| p.id == "vec-only"));
    assert!(visible.iter().any(|p| p.id == "kg-only"));
    assert!(
        visible.iter().any(|p| p.id == "drawer-only"),
        "drawer-only palace must be visible"
    );
    assert!(!visible.iter().any(|p| p.id == "empty"));

    let rows = palace_lines(&state);
    assert!(!rows.iter().any(|r| r.text.contains("empty")));
    assert!(rows.iter().any(|r| r.text.contains("drawer-o")));
}

#[test]
fn test_palace_row_with_activity() {
    let p = PalaceRow {
        id: "default".into(),
        name: "default".into(),
        vector_count: 8_400,
        ..Default::default()
    };
    let row = palace_row_with_activity(&p, PalaceActivity::Indexing, 0);
    assert_eq!(row.chars().next(), Some(INDEXING_SPINNER[0]));
    assert!(row.contains("default"));
    assert!(row.contains("8,400v"));

    let ind = view::palace_row_indented_with_activity(&p, PalaceActivity::Active, 0);
    assert!(ind.starts_with(' '));
    assert!(ind.contains('⠿'));
    assert!(ind.contains("default"));
}

#[test]
fn test_palace_lines_activity() {
    use chrono::{TimeZone, Utc};
    let now = Utc.with_ymd_and_hms(2026, 5, 22, 12, 0, 0).unwrap();
    let mut state = MemoryTuiState::new("http://x");
    state.palaces = vec![
        PalaceRow {
            id: "indexing".into(),
            name: "indexing".into(),
            vector_count: 1,
            last_write_at: Some(now - chrono::Duration::seconds(2)),
            ..Default::default()
        },
        PalaceRow {
            id: "dreaming".into(),
            name: "dreaming".into(),
            vector_count: 1,
            is_compacting: true,
            ..Default::default()
        },
    ];
    let rows = palace_lines_at(&state, now, 0);
    assert_eq!(rows[0].activity, None);
    assert_eq!(rows[1].activity, Some(PalaceActivity::Indexing));
    assert_eq!(rows[2].activity, Some(PalaceActivity::Dreaming));
}

#[test]
fn test_stats_graph_section() {
    use chrono::{TimeZone, Utc};
    let mut state = MemoryTuiState::new("http://x");
    state.daemon_status = DaemonStatus::Online {
        version: "0.1.54".into(),
        uptime_secs: 0,
    };
    state.palaces = vec![PalaceRow {
        id: "p1".into(),
        name: "p1".into(),
        vector_count: 1_234,
        kg_triple_count: 567,
        node_count: 4_321,
        edge_count: 12_345,
        community_count: 7,
        last_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 22, 11, 59, 50).unwrap()),
        ..Default::default()
    }];
    state.selected = 1;
    let lines = stats_lines(&state);
    let joined = lines.join("\n");
    assert!(joined.contains("Knowledge Graph"));
    assert!(joined.contains("Nodes:"));
    assert!(joined.contains("4,321"));
    assert!(joined.contains("Edges:"));
    assert!(joined.contains("12.3k"));
    assert!(joined.contains("Triples:"));
    assert!(joined.contains("567"));
    assert!(joined.contains("Last write:"));
    assert!(joined.contains("State:"));
}

#[test]
fn test_format_relative_time() {
    use chrono::{TimeZone, Utc};
    let now = Utc.with_ymd_and_hms(2026, 5, 22, 12, 0, 0).unwrap();
    assert_eq!(
        format_relative_time(now, now - chrono::Duration::seconds(1)),
        "just now"
    );
    assert_eq!(
        format_relative_time(now, now - chrono::Duration::seconds(30)),
        "30s ago"
    );
    assert_eq!(
        format_relative_time(now, now - chrono::Duration::minutes(2)),
        "2m ago"
    );
    assert_eq!(
        format_relative_time(now, now - chrono::Duration::hours(5)),
        "5h ago"
    );
    assert_eq!(
        format_relative_time(now, now - chrono::Duration::days(3)),
        "3d ago"
    );
    assert_eq!(
        format_relative_time(now, now + chrono::Duration::seconds(10)),
        "just now"
    );
}

#[test]
fn test_spinner_tick_returns_value() {
    let _t = spinner_tick();
}

#[test]
fn dream_backoff_allows_first_attempt() {
    let backoff = DreamBackoff::new();
    assert!(backoff.ready(std::time::Instant::now()));
    assert_eq!(backoff.consecutive_failures(), 0);
    assert_eq!(
        backoff.remaining(std::time::Instant::now()),
        std::time::Duration::ZERO
    );
}

#[test]
fn dream_backoff_blocks_within_window() {
    let mut backoff = DreamBackoff::new();
    let t0 = std::time::Instant::now();
    let logged = backoff.record_failure(t0);
    assert!(logged, "first failure must be loud");
    assert!(!backoff.ready(t0 + std::time::Duration::from_secs(1)));
    assert!(backoff.ready(t0 + DREAM_BACKOFF_INITIAL));
}

#[test]
fn dream_backoff_remaining_reports_window() {
    let mut backoff = DreamBackoff::new();
    let t0 = std::time::Instant::now();
    backoff.record_failure(t0);
    let r = backoff.remaining(t0);
    assert!(r <= DREAM_BACKOFF_INITIAL && r > std::time::Duration::from_secs(0));
}

#[test]
fn dream_backoff_resets_on_success() {
    let mut backoff = DreamBackoff::new();
    let t0 = std::time::Instant::now();
    backoff.record_failure(t0);
    backoff.record_failure(t0);
    assert_eq!(backoff.consecutive_failures(), 2);
    backoff.record_success();
    assert_eq!(backoff.consecutive_failures(), 0);
    assert!(backoff.ready(t0));
    assert!(backoff.record_failure(t0));
}

#[test]
fn dream_backoff_logs_only_first() {
    let mut backoff = DreamBackoff::new();
    let t0 = std::time::Instant::now();
    assert!(backoff.record_failure(t0), "first failure is loud");
    assert!(
        !backoff.record_failure(t0),
        "subsequent failures are suppressed"
    );
    assert!(
        !backoff.record_failure(t0),
        "still suppressed after several failures"
    );
}

#[test]
fn dream_backoff_delay_doubles_and_caps() {
    assert_eq!(backoff_delay(1), DREAM_BACKOFF_INITIAL);
    assert_eq!(backoff_delay(2), DREAM_BACKOFF_INITIAL * 2);
    assert_eq!(backoff_delay(3), DREAM_BACKOFF_INITIAL * 4);
    assert_eq!(backoff_delay(30), DREAM_BACKOFF_MAX);
    assert_eq!(backoff_delay(0), DREAM_BACKOFF_INITIAL);
}

#[test]
fn dream_backoff_doubles_then_caps() {
    let mut backoff = DreamBackoff::new();
    let t0 = std::time::Instant::now();
    let mut last = std::time::Duration::ZERO;
    for _ in 0..10 {
        backoff.record_failure(t0);
        let r = backoff.remaining(t0);
        assert!(r >= last || r == DREAM_BACKOFF_MAX);
        last = r;
    }
    assert!(last <= DREAM_BACKOFF_MAX);
}

fn sample_drawer(idx: usize, tags: &[&str]) -> DrawerInfo {
    use chrono::{TimeZone, Utc};
    DrawerInfo {
        id: format!("{idx:08x}-aaaa-bbbb-cccc-dddddddddddd"),
        created_at: Some(
            Utc.with_ymd_and_hms(2026, 5, 1, 12, idx as u32 % 60, 0)
                .unwrap(),
        ),
        creator: crate::monitor::memory_client::creator_label(
            &tags.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        ),
        tags: tags.iter().map(|s| (*s).to_string()).collect(),
        snippet: None,
    }
}

fn sample_drawer_with_snippet(idx: usize, tags: &[&str], snippet: &str) -> DrawerInfo {
    let mut d = sample_drawer(idx, tags);
    d.snippet = Some(snippet.to_string());
    d
}

#[test]
fn drawer_state_default_page_size() {
    let state = DrawerListState::new();
    assert!(state.palace_id.is_none());
    assert!(state.drawers.is_empty());
    assert_eq!(state.offset, 0);
    assert!(!state.loading);
    assert!(state.last_error.is_none());
    assert_eq!(state.page(), 0);
    assert_eq!(DRAWER_PAGE_SIZE, 20);
}

#[test]
fn drawer_state_reset_on_palace_change() {
    let mut state = DrawerListState {
        palace_id: Some("old".into()),
        drawers: vec![sample_drawer(1, &[])],
        offset: 40,
        loading: false,
        last_error: Some("stale".into()),
    };
    state.reset_for(Some("new".into()));
    assert_eq!(state.palace_id.as_deref(), Some("new"));
    assert!(state.drawers.is_empty());
    assert_eq!(state.offset, 0);
    assert!(state.loading, "should mark loading after reset");
    assert!(state.last_error.is_none());

    state.reset_for(None);
    assert!(state.palace_id.is_none());
}

#[test]
fn drawer_state_pagination() {
    let mut state = DrawerListState::new();
    state.drawers = (0..DRAWER_PAGE_SIZE)
        .map(|i| sample_drawer(i, &[]))
        .collect();
    state.next_page();
    assert_eq!(state.offset, DRAWER_PAGE_SIZE);
    assert_eq!(state.page(), 1);
    assert!(state.loading);

    state.loading = false;
    state.prev_page();
    assert_eq!(state.offset, 0);
    assert_eq!(state.page(), 0);
    assert!(state.loading);

    state.loading = false;
    state.prev_page();
    assert_eq!(state.offset, 0);
    assert!(!state.loading);

    state.drawers = vec![sample_drawer(0, &[])];
    state.next_page();
    assert_eq!(
        state.offset, 0,
        "end-of-list page should not advance past last",
    );
}

#[test]
fn drawer_row_layout() {
    let drawer = sample_drawer(0xab, &["msg:from=cto"]);
    let row = format_drawer_row(&drawer);
    assert!(
        row.starts_with("000000a…") || row.starts_with("000000ab"),
        "row should start with truncated id, got: {row}",
    );
    assert!(row.contains("05-01"), "row should carry MM-DD: {row}");
    assert!(row.contains("msg:from=cto"), "creator missing: {row}");

    let bare = sample_drawer(1, &[]);
    let row = format_drawer_row(&bare);
    assert!(
        row.contains("—"),
        "missing em-dash for no-creator row: {row}"
    );

    let mut undated = sample_drawer(2, &[]);
    undated.created_at = None;
    let row = format_drawer_row(&undated);
    assert!(row.contains("--"), "missing `--` for undated row: {row}");
}

#[test]
fn drawer_row_includes_snippet() {
    let with_snippet =
        sample_drawer_with_snippet(3, &["msg:from=cto"], "JWT middleware added to auth flow");
    let row = format_drawer_row(&with_snippet);
    assert!(
        row.contains("msg:from=cto"),
        "creator must still appear before snippet: {row}",
    );
    assert!(
        row.contains("JWT middleware added to auth flow"),
        "snippet must be appended: {row}",
    );

    let bare = sample_drawer(4, &["msg:from=cto"]);
    let row = format_drawer_row(&bare);
    assert!(
        !row.ends_with("  "),
        "no-snippet row must not have trailing whitespace: {row:?}",
    );

    let empty = sample_drawer_with_snippet(5, &["msg:from=cto"], "   ");
    let row = format_drawer_row(&empty);
    assert!(
        !row.ends_with("  "),
        "whitespace-only snippet must be elided: {row:?}",
    );

    let long = "x".repeat(200);
    let big = sample_drawer_with_snippet(6, &["msg:from=cto"], &long);
    let row = format_drawer_row(&big);
    assert!(
        row.contains('…'),
        "long snippet must be truncated with `…`: {row}",
    );
}

#[test]
fn drawer_panel_lines_renders_no_palace() {
    let state = sample_state();
    assert!(state.drawer_list.palace_id.is_none());
    let lines = drawer_panel_lines(&state, 0);
    assert!(lines.is_empty(), "no-scope path should render no lines");
}

#[test]
fn drawer_panel_lines_renders_loading_then_rows() {
    let mut state = sample_state();
    state.drawer_list.palace_id = Some("default".into());
    state.drawer_list.loading = true;
    let lines = drawer_panel_lines(&state, 0);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("loading"));

    state.drawer_list.loading = false;
    state.drawer_list.drawers = vec![
        sample_drawer(1, &["msg:from=cto"]),
        sample_drawer(2, &["creator:client=mpm"]),
    ];
    let lines = drawer_panel_lines(&state, 14);
    assert_eq!(lines.len(), 3, "header + 2 rows");
    assert!(lines[0].contains("drawers 1–2"));
    assert!(lines[0].contains("page 1"));
    assert!(lines[1].contains("msg:from=cto"));
    assert!(lines[2].contains("creator:client=mpm"));
}

#[test]
fn drawer_panel_lines_renders_error() {
    let mut state = sample_state();
    state.drawer_list.palace_id = Some("default".into());
    state.drawer_list.loading = false;
    state.drawer_list.last_error = Some("connection refused".into());
    let lines = drawer_panel_lines(&state, 0);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("drawers unavailable"));
    assert!(lines[0].contains("connection refused"));
}

#[test]
fn test_focus_tab_cycle() {
    assert_eq!(MemoryFocus::default(), MemoryFocus::List);
    let mut focus = MemoryFocus::List;
    focus = focus.next();
    assert_eq!(focus, MemoryFocus::DrawerPane);
    focus = focus.next();
    assert_eq!(focus, MemoryFocus::Input);
    focus = focus.next();
    assert_eq!(focus, MemoryFocus::List);
}

#[test]
fn test_state_cycle_focus_resets_drawer_cursor() {
    let mut state = sample_state();
    state.drawer_cursor = 5;
    state.cycle_focus();
    assert_eq!(state.focus, MemoryFocus::DrawerPane);
    assert_eq!(state.drawer_cursor, 5, "cursor preserved while in pane");
    state.cycle_focus();
    assert_eq!(state.focus, MemoryFocus::Input);
    assert_eq!(state.drawer_cursor, 5, "cursor preserved while away");
    state.cycle_focus();
    assert_eq!(state.focus, MemoryFocus::List);
    assert_eq!(state.drawer_cursor, 0, "cursor resets on return to list");
}

#[test]
fn test_drawer_cursor_clamp() {
    let mut state = sample_state();
    state.drawer_list.drawers = (0..3).map(|i| sample_drawer(i, &[])).collect();
    state.drawer_cursor_up();
    assert_eq!(state.drawer_cursor, 0);
    state.drawer_cursor_down();
    state.drawer_cursor_down();
    state.drawer_cursor_down();
    state.drawer_cursor_down();
    assert_eq!(state.drawer_cursor, 2, "clamped at last index");
    state.drawer_list.drawers.truncate(1);
    state.clamp_drawer_cursor();
    assert_eq!(state.drawer_cursor, 0, "clamped to new last index");
    state.drawer_list.drawers.clear();
    state.drawer_cursor = 5;
    state.clamp_drawer_cursor();
    assert_eq!(state.drawer_cursor, 0);
    state.drawer_cursor_down();
    assert_eq!(state.drawer_cursor, 0);
}

#[test]
fn test_drawer_detail_modal_lifecycle() {
    let mut state = sample_state();
    state.drawer_detail_open = true;
    state.drawer_detail_idx = 3;
    state.drawer_detail_scroll = 17;
    state.drawer_detail_loading = true;
    state.drawer_detail_memories = vec![MemoryDetail {
        id: "x".into(),
        content: "y".into(),
        tags: vec![],
        created_at: None,
    }];
    state.close_drawer_detail();
    assert!(!state.drawer_detail_open);
    assert!(state.drawer_detail_memories.is_empty());
    assert_eq!(state.drawer_detail_scroll, 0);
    assert!(!state.drawer_detail_loading);
}

#[test]
fn test_drawer_detail_body_layout() {
    use chrono::{TimeZone, Utc};
    let mut state = sample_state();
    state.drawer_detail_memories = vec![
        MemoryDetail {
            id: "abc-123".into(),
            content: "First memory body".into(),
            tags: vec!["msg:from=cto".into(), "tag:type=note".into()],
            created_at: Some(Utc.with_ymd_and_hms(2026, 5, 20, 12, 34, 56).unwrap()),
        },
        MemoryDetail {
            id: "def-456".into(),
            content: "Second memory body".into(),
            tags: vec![],
            created_at: None,
        },
    ];
    let body = drawer_detail_body(&state);
    assert!(
        body.contains("Drawer: abc-123"),
        "missing id header: {body}"
    );
    assert!(body.contains("2026-05-20 12:34:56 UTC"));
    assert!(body.contains("msg:from=cto"));
    assert!(body.contains("tag:type=note"));
    assert!(body.contains("First memory body"));
    assert!(
        body.contains("──────────────────────────────────────"),
        "missing memory separator: {body}",
    );
    assert!(body.contains("Drawer: def-456"));
    assert!(body.contains("(no timestamp)"));
    assert!(body.contains("(none)"));
    assert!(body.contains("Second memory body"));
}

#[test]
fn test_drawer_detail_body_loading() {
    let mut state = sample_state();
    state.drawer_detail_loading = true;
    assert_eq!(drawer_detail_body(&state), "Loading…");
    state.drawer_detail_loading = false;
    assert_eq!(drawer_detail_body(&state), "(no memories returned)");
}

#[test]
fn test_render_drawer_pane_focused_title() {
    let mut state = sample_state();
    state.selected = 1;
    state.focus = MemoryFocus::DrawerPane;
    state.drawer_list.palace_id = Some("default".into());
    state.drawer_list.drawers = vec![sample_drawer(0, &["msg:from=cto"])];
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &mut state))
        .expect("render with drawer focus must not panic");
    let buffer = terminal.backend().buffer();
    let content: String = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        content.contains("DRAWER ▶"),
        "expected DRAWER ▶ marker in rendered output",
    );
}

#[test]
fn test_render_with_drawer_detail_open() {
    use chrono::{TimeZone, Utc};
    let mut state = sample_state();
    state.selected = 1;
    state.drawer_list.palace_id = Some("default".into());
    state.drawer_list.drawers = vec![DrawerInfo {
        id: "abc12345-rest-of-uuid".into(),
        ..Default::default()
    }];
    state.drawer_detail_open = true;
    state.drawer_detail_idx = 0;
    state.drawer_detail_memories = vec![MemoryDetail {
        id: "abc12345-rest-of-uuid".into(),
        content: "Verbatim memory body for the detail pane".into(),
        tags: vec!["msg:from=cto".into()],
        created_at: Some(Utc.with_ymd_and_hms(2026, 5, 20, 12, 34, 56).unwrap()),
    }];
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|f| render(f, &mut state))
        .expect("render with detail pane open must not panic");
    let buffer = terminal.backend().buffer();
    let content: String = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        content.contains("DETAIL"),
        "expected DETAIL pane title in rendered output: {content}",
    );
    assert!(
        content.contains("abc12345"),
        "expected drawer-id prefix in DETAIL title: {content}",
    );
    assert!(
        content.contains("STATISTICS"),
        "expected STATISTICS panel to remain visible in split layout",
    );
}

#[test]
fn drawer_panel_lines_renders_empty_palace() {
    let mut state = sample_state();
    state.drawer_list.palace_id = Some("default".into());
    state.drawer_list.loading = false;
    let lines = drawer_panel_lines(&state, 0);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("no drawers yet"));
}
