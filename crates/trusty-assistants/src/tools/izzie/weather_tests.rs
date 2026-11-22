//! Fixture-driven tests for the `get_weather` HTTP-free helpers.
//!
//! Why: `execute` does live I/O; the *logic* worth pinning is arg parsing,
//! WMO-code labelling, and the geocode/forecast/alert JSON mappings. Those are
//! pure and tested here against captured-shape fixtures — no network.
//! What: Exercises `parse_args`, `weather_code_label`, `geocode_first_result`,
//! `map_forecast`, and `map_nws_alerts`, plus an `#[ignore]` live smoke test.
//! Test: This file IS the test module for `weather`.

use super::*;
use serde_json::json;

#[test]
fn parse_args_defaults_and_clamps() {
    let a = parse_args(&json!({}));
    assert_eq!(a.location, None);
    assert_eq!(a.latitude, None);
    assert_eq!(a.days, 3);

    let clamped = parse_args(&json!({ "days": 99 }));
    assert_eq!(clamped.days, 7);
    let floored = parse_args(&json!({ "days": 0 }));
    assert_eq!(floored.days, 1);

    // Blank location string is treated as absent.
    let blank = parse_args(&json!({ "location": "   " }));
    assert_eq!(blank.location, None);
}

#[test]
fn parse_args_reads_coordinates() {
    let a = parse_args(&json!({ "latitude": 41.0, "longitude": -73.8, "days": 2 }));
    assert_eq!(a.latitude, Some(41.0));
    assert_eq!(a.longitude, Some(-73.8));
    assert_eq!(a.days, 2);

    let named = parse_args(&json!({ "location": "Stamford, CT" }));
    assert_eq!(named.location.as_deref(), Some("Stamford, CT"));
}

#[test]
fn weather_code_labels_known_and_unknown() {
    assert_eq!(weather_code_label(0), "Clear sky");
    assert_eq!(weather_code_label(2), "Partly cloudy");
    assert_eq!(weather_code_label(65), "Rain");
    assert_eq!(weather_code_label(95), "Thunderstorm");
    assert_eq!(weather_code_label(123), "WMO code 123");
}

#[test]
fn geocode_first_result_maps_fields() {
    let body = json!({
        "results": [
            { "name": "Stamford", "admin1": "Connecticut", "country": "United States",
              "latitude": 41.0534, "longitude": -73.5387 },
            { "name": "Other" }
        ]
    });
    let (label, lat, lon) = geocode_first_result(&body).expect("should match first result");
    assert_eq!(label, "Stamford, Connecticut, United States");
    assert!((lat - 41.0534).abs() < 1e-6);
    assert!((lon + 73.5387).abs() < 1e-6);
}

#[test]
fn geocode_first_result_empty() {
    assert!(geocode_first_result(&json!({ "results": [] })).is_none());
    assert!(geocode_first_result(&json!({})).is_none());
}

#[test]
fn forecast_maps_current_and_daily() {
    let body = json!({
        "current": {
            "temperature_2m": 72.4,
            "apparent_temperature": 70.1,
            "relative_humidity_2m": 55,
            "wind_speed_10m": 8.3,
            "precipitation": 0.0,
            "weather_code": 2
        },
        "daily": {
            "time": ["2026-07-22", "2026-07-23"],
            "temperature_2m_max": [81.0, 84.2],
            "temperature_2m_min": [63.0, 65.5],
            "weather_code": [3, 95],
            "precipitation_probability_max": [10, 80],
            "precipitation_sum": [0.0, 0.6]
        }
    });
    let out = map_forecast("Hastings-on-Hudson, NY", 41.0, -73.8, &body);
    assert_eq!(out["location"], "Hastings-on-Hudson, NY");
    assert_eq!(out["current"]["temperature_f"], 72.4);
    assert_eq!(out["current"]["conditions"], "Partly cloudy");
    let days = out["daily"].as_array().unwrap();
    assert_eq!(days.len(), 2);
    assert_eq!(days[0]["high_f"], 81.0);
    assert_eq!(days[0]["conditions"], "Overcast");
    assert_eq!(days[1]["conditions"], "Thunderstorm");
    assert_eq!(days[1]["precip_chance_pct"], 80);
    assert_eq!(days[1]["precip_in"], 0.6);
}

#[test]
fn forecast_tolerates_missing_daily() {
    let body = json!({ "current": { "temperature_2m": 60.0 } });
    let out = map_forecast("Nowhere", 0.0, 0.0, &body);
    assert_eq!(out["daily"].as_array().unwrap().len(), 0);
    assert_eq!(out["current"]["temperature_f"], 60.0);
    // Absent weather_code -> null conditions, not a fabricated label.
    assert!(out["current"]["conditions"].is_null());
}

#[test]
fn nws_alerts_maps_features() {
    let body = json!({
        "features": [
            { "properties": {
                "event": "Flood Watch",
                "severity": "Moderate",
                "headline": "Flood Watch until 8 PM"
            } }
        ]
    });
    let alerts = map_nws_alerts(&body);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0]["event"], "Flood Watch");
    assert_eq!(alerts[0]["severity"], "Moderate");
}

#[test]
fn nws_alerts_empty() {
    assert_eq!(map_nws_alerts(&json!({ "features": [] })).len(), 0);
    assert_eq!(map_nws_alerts(&json!({})).len(), 0);
}

#[test]
fn tool_reports_its_name() {
    assert_eq!(GetWeatherTool::new().name(), "get_weather");
}

/// Live smoke test against Open-Meteo — gated `#[ignore]` (requires network).
#[tokio::test]
#[ignore = "hits the live Open-Meteo API; run manually with --ignored"]
async fn live_weather_smoke() {
    let tool = GetWeatherTool::new();
    let result = tool
        .execute(json!({ "location": "Hastings-on-Hudson, NY", "days": 2 }))
        .await;
    assert!(!result.is_error(), "live weather call failed: {result:?}");
    let parsed: serde_json::Value = serde_json::from_str(result.content()).unwrap();
    assert!(parsed["current"]["temperature_f"].is_number());
}
