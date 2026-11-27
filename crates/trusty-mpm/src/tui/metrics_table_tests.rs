//! #10908: the service view renders `by_*` breakdowns as nested top-N tables.

use std::time::Instant;

use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};
use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth};

use super::render::{metric_lines, render};
use super::services::ServiceId;
use super::state::{HomeState, MetricsUpdate, View};

/// The trusty-events payload, in the shape `check_by_convention` accepts.
fn events_metrics() -> Value {
    json!({
        "uptime_secs": 4,
        "events": 12,
        "by_source": {"code": 3, "mpm": 9},
        "messages": {"sent": 1, "received": 1},
        "by_project": {
            "trusty-tools": {
                "events": 9,
                "by_domain": {"hook": 2, "message": 7},
            },
            // Sorts first by key, so its last place proves the order is the rank.
            "a-small-project": {"events": 1},
            "unattributed": {"events": 3, "by_domain": {}},
        },
    })
}

/// Snapshot: plain fields first, then breakdowns, each sorted by key; counts
/// sort high to low, bodies by their largest count and list their scalars, and
/// nested breakdowns indent under their entry. Plain objects expand instead of
/// collapsing to "{N fields}".
#[test]
fn by_breakdowns_render_as_nested_tables_snapshot() {
    let expected = [
        "  events: 12",
        "  messages:",
        "    received: 1",
        "    sent: 1",
        "  uptime_secs: 4",
        "  by_project (3):",
        "    trusty-tools     events=9",
        "      by_domain (2):",
        "        message  7",
        "        hook     2",
        "    unattributed     events=3",
        "      by_domain: (none)",
        "    a-small-project  events=1",
        "  by_source (2):",
        "    mpm   9",
        "    code  3",
    ];
    assert_eq!(metric_lines(&events_metrics()), expected);
}

/// A breakdown longer than the top-N limit lists the largest entries and
/// counts the rest; a breakdown inside a plain field is a table too.
#[test]
fn a_long_breakdown_keeps_the_top_entries_and_counts_the_rest() {
    let metrics = json!({
        "totals": {
            "requests": 28,
            "by_status": {"200": 10, "404": 1, "500": 2, "201": 7, "204": 3, "301": 4, "302": 1},
        },
    });
    let expected = [
        "  totals:",
        "    requests: 28",
        "    by_status (7):",
        "      200  10",
        "      201   7",
        "      301   4",
        "      204   3",
        "      500   2",
        "      … 2 more",
    ];
    assert_eq!(metric_lines(&metrics), expected);
}

/// Past the line cap the list ends in a row counting what it hid, so a later
/// breakdown is never dropped without a trace.
#[test]
fn a_cut_metrics_list_says_how_much_it_hides() {
    let fields: serde_json::Map<String, Value> =
        (0..50).map(|i| (format!("f{i:02}"), json!(i))).collect();
    let lines = metric_lines(&Value::Object(fields));
    assert_eq!(lines.len(), 40, "{lines:?}");
    assert_eq!(lines[38], "  f38: 38");
    assert_eq!(lines[39], "  … 11 more lines");
}

/// The drawn service view carries the table rows, not the old summary.
#[test]
fn service_view_draws_breakdown_rows() {
    let mut state = HomeState::default();
    state.apply_metrics(
        MetricsUpdate {
            id: ServiceId::Search,
            result: Ok(ConsoleMetricsReport {
                service_id: "trusty-events".into(),
                display_name: "Events".into(),
                version: "0.1.0".into(),
                status: ServiceHealth::Ok,
                metrics: events_metrics(),
                metrics_schema_version: 1,
                collected_at_unix: None,
            }),
        },
        Instant::now(),
    );
    state.view = View::Service(ServiceId::Search);
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).expect("test backend");
    terminal.draw(|f| render(f, &state)).expect("draw");
    let buffer = terminal.backend().buffer();
    let text: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect();
    let joined = text.join("\n");
    assert!(joined.contains("by_source (2):"), "{joined}");
    assert!(joined.contains("message  7"), "{joined}");
    assert!(!joined.contains("fields}"), "{joined}");
}
