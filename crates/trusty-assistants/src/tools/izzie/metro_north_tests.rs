//! Tests for the Metro-North feed->output mappers.
//!
//! Why: `execute` fetches the live protobuf feed; the logic worth pinning is
//! departure selection, downstream-arrival matching, line filtering, sorting,
//! limiting, alert filtering, and Eastern-time formatting. All are pure and
//! tested here against hand-built `Feed` fixtures.
//! What: Constructs `Feed`/`TripUpdate`/`StopTimeUpdate`/`Alert` directly and
//! asserts on the shaped JSON, plus arg validation and an `#[ignore]` live
//! smoke test.
//! Test: This file IS the test module for `metro_north`.

use super::super::gtfs_rt::{Alert, StopTimeUpdate, TripUpdate};
use super::super::stations::match_station;
use super::*;

fn stop(
    id: &str,
    seq: u32,
    depart: Option<i64>,
    arrive: Option<i64>,
    track: Option<&str>,
) -> StopTimeUpdate {
    StopTimeUpdate {
        stop_id: Some(id.to_string()),
        stop_sequence: Some(seq),
        arrival: arrive,
        departure: depart,
        assigned_track: track.map(str::to_string),
    }
}

fn trip(id: &str, route: &str, stops: Vec<StopTimeUpdate>) -> TripUpdate {
    TripUpdate {
        trip_id: Some(id.to_string()),
        route_id: Some(route.to_string()),
        start_date: Some("20260722".to_string()),
        start_time: None,
        stops,
    }
}

#[test]
fn format_eastern_hhmm_known_epoch() {
    // 2026-07-22 18:42:00 UTC -> 14:42 EDT (UTC-4 in July).
    let s = format_eastern_hhmm(1_753_209_720).unwrap();
    assert_eq!(s, "14:42");
}

#[test]
fn build_schedule_orders_filters_and_limits() {
    let gct = match_station("Grand Central").unwrap(); // stop_id "1"
    let now = 1_000_000_000;
    let feed = Feed {
        trip_updates: vec![
            // New Haven (route 3) departs later.
            trip(
                "later",
                "3",
                vec![stop("1", 1, Some(now + 600), None, Some("24"))],
            ),
            // Hudson (route 1) departs sooner.
            trip(
                "sooner",
                "1",
                vec![stop("1", 1, Some(now + 120), None, None)],
            ),
            // A past departure -> excluded.
            trip("past", "2", vec![stop("1", 1, Some(now - 300), None, None)]),
            // Different origin -> excluded.
            trip(
                "other",
                "2",
                vec![stop("74", 1, Some(now + 60), None, None)],
            ),
        ],
        alerts: vec![],
    };

    // No line filter: soonest-first, both future trips, limit 5.
    let out = build_schedule(&feed, gct, None, None, 5, now);
    let deps = out["departures"].as_array().unwrap();
    assert_eq!(deps.len(), 2, "past + wrong-origin trips excluded");
    assert_eq!(deps[0]["trip_id"], "sooner");
    assert_eq!(deps[0]["line"], "Hudson");
    assert_eq!(deps[1]["trip_id"], "later");
    assert_eq!(deps[1]["line"], "New Haven");
    assert_eq!(deps[1]["track"], "24");

    // Limit truncates to the soonest.
    let limited = build_schedule(&feed, gct, None, None, 1, now);
    assert_eq!(limited["departures"].as_array().unwrap().len(), 1);
    assert_eq!(limited["departures"][0]["trip_id"], "sooner");

    // Line filter keeps only the New Haven trip.
    let nh = build_schedule(&feed, gct, None, Some("New Haven"), 5, now);
    let nh_deps = nh["departures"].as_array().unwrap();
    assert_eq!(nh_deps.len(), 1);
    assert_eq!(nh_deps[0]["trip_id"], "later");
}

#[test]
fn build_schedule_requires_downstream_destination() {
    let gct = match_station("Grand Central").unwrap(); // "1"
    let stamford = match_station("Stamford").unwrap(); // "124"
    let now = 1_000_000_000;
    let feed = Feed {
        trip_updates: vec![
            // Serves GCT then Stamford downstream -> included with arrival.
            trip(
                "to-stamford",
                "3",
                vec![
                    stop("1", 1, Some(now + 100), None, Some("30")),
                    stop("124", 8, None, Some(now + 3000), None),
                ],
            ),
            // Serves GCT but NOT Stamford -> excluded when `to` is set.
            trip(
                "to-nowhere",
                "1",
                vec![stop("1", 1, Some(now + 200), None, None)],
            ),
        ],
        alerts: vec![],
    };

    let out = build_schedule(&feed, gct, Some(stamford), None, 5, now);
    let deps = out["departures"].as_array().unwrap();
    assert_eq!(deps.len(), 1, "only the trip serving Stamford survives");
    assert_eq!(deps[0]["trip_id"], "to-stamford");
    assert_eq!(deps[0]["arrive_epoch"], now + 3000);
    assert!(deps[0]["arrive"].is_string());
    assert_eq!(out["to"], "Stamford");
}

#[test]
fn build_alerts_filters_by_line() {
    let feed = Feed {
        trip_updates: vec![],
        alerts: vec![
            Alert {
                route_ids: vec!["1".to_string()], // Hudson
                header: Some("Hudson delays".to_string()),
                description: Some("Signal problems".to_string()),
            },
            Alert {
                route_ids: vec!["3".to_string()], // New Haven
                header: Some("New Haven suspended".to_string()),
                description: None,
            },
            Alert {
                route_ids: vec![], // system-wide -> always surfaces
                header: Some("Holiday schedule".to_string()),
                description: None,
            },
        ],
    };

    let all = build_alerts(&feed, None);
    assert_eq!(all["count"], 3);

    let hudson = build_alerts(&feed, Some("Hudson"));
    let items = hudson["alerts"].as_array().unwrap();
    // Hudson-specific + the system-wide alert.
    assert_eq!(items.len(), 2);
    let headers: Vec<&str> = items
        .iter()
        .map(|a| a["header"].as_str().unwrap())
        .collect();
    assert!(headers.contains(&"Hudson delays"));
    assert!(headers.contains(&"Holiday schedule"));
    assert!(!headers.contains(&"New Haven suspended"));
    assert_eq!(items[0]["lines"][0], "Hudson");
}

#[test]
fn parse_query_validates_stations() {
    // Missing `from`.
    assert!(GetTrainScheduleTool::parse_query(&json!({})).is_err());
    // Unknown origin.
    assert!(GetTrainScheduleTool::parse_query(&json!({ "from": "Narnia" })).is_err());
    // Valid; limit clamps.
    let q = GetTrainScheduleTool::parse_query(&json!({
        "from": "Stamford", "to": "Grand Central", "line": "New Haven", "limit": 99
    }))
    .unwrap();
    assert_eq!(q.from.stop_id, "124");
    assert_eq!(q.to.unwrap().stop_id, "1");
    assert_eq!(q.line.as_deref(), Some("New Haven"));
    assert_eq!(q.limit, 10);
    // Unknown destination errors.
    assert!(
        GetTrainScheduleTool::parse_query(&json!({ "from": "Stamford", "to": "Narnia" })).is_err()
    );
}

#[test]
fn tools_report_their_names() {
    assert_eq!(GetTrainScheduleTool::new().name(), "get_train_schedule");
    assert_eq!(GetTrainAlertsTool::new().name(), "get_train_alerts");
}

/// Live smoke test against the MTA feed — gated `#[ignore]` (network).
#[tokio::test]
#[ignore = "hits the live MTA Metro-North GTFS-RT feed; run manually with --ignored"]
async fn live_alerts_smoke() {
    let tool = GetTrainAlertsTool::new();
    let result = tool.execute(json!({})).await;
    assert!(!result.is_error(), "live alerts call failed: {result:?}");
    let parsed: serde_json::Value = serde_json::from_str(result.content()).unwrap();
    assert!(parsed["alerts"].is_array());
}
