//! `get_train_schedule` + `get_train_alerts` — MTA Metro-North real-time
//! departures and service alerts (epic #3052).
//!
//! Why: Izzie's `izzie-metro-north` skill promises live train times and
//! service alerts with no API key, but the tools it names never existed. This
//! implements both against the MTA Metro-North GTFS-Realtime feed. The feed is
//! keyless as of 2023; if a deployment sits behind a gateway that still wants a
//! key, `MTA_API_KEY` is sent as `x-api-key` when set and simply omitted
//! otherwise (graceful degradation, never hardcoded). The feed URL itself is
//! overridable via `IZZIE_MNR_GTFS_RT_URL`.
//! What: `GetTrainScheduleTool` resolves origin/destination station names to
//! GTFS stop_ids (see `stations`), decodes the protobuf feed (see `gtfs_rt`),
//! and returns the next upcoming departures — with assigned track and, when a
//! destination is given, the downstream arrival. `GetTrainAlertsTool` returns
//! active alerts, optionally filtered to one line. Both emit structured JSON.
//! Test: The HTTP-free feed->output mappers are unit-tested against decoded
//! `Feed` fixtures; the live fetch path is an `#[ignore]` integration test.

use std::time::Duration;

use async_trait::async_trait;
use chrono::TimeZone;
use chrono_tz::America::New_York;
use serde_json::{Value, json};

use super::gtfs_rt::{Feed, decode_feed};
use super::stations::{Station, match_station, route_matches_line, route_name};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Default keyless MNR GTFS-Realtime feed (overridable via env). The feed
/// carries trip updates AND service alerts, so both tools read the same URL.
const DEFAULT_FEED_URL: &str = "https://api-endpoint.mta.info/Dataservice/mnrtrpke.pb";
const FEED_URL_ENV: &str = "IZZIE_MNR_GTFS_RT_URL";
const API_KEY_ENV: &str = "MTA_API_KEY";

/// Resolve the feed URL from env, falling back to the keyless default.
fn feed_url() -> String {
    std::env::var(FEED_URL_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_FEED_URL.to_string())
}

/// Format a Unix epoch (seconds) as Eastern-time `HH:MM`.
///
/// Why: Metro-North schedules are always quoted in Eastern time; the feed
/// carries UTC epochs.
/// What: Converts via `America/New_York` (handles DST). Returns `None` for a
/// non-representable timestamp rather than panicking.
/// Test: `format_eastern_hhmm_known_epoch`.
pub fn format_eastern_hhmm(epoch: i64) -> Option<String> {
    New_York
        .timestamp_opt(epoch, 0)
        .single()
        .map(|dt| dt.format("%H:%M").to_string())
}

/// Parsed `get_train_schedule` arguments after station resolution.
struct ScheduleQuery {
    from: &'static Station,
    to: Option<&'static Station>,
    line: Option<String>,
    limit: usize,
}

/// The soonest relevant departure from a station within one trip.
struct Departure {
    trip_id: Option<String>,
    route_id: Option<String>,
    depart_epoch: i64,
    track: Option<String>,
    arrive_epoch: Option<i64>,
}

/// Build the schedule result from a decoded feed (pure — no I/O).
///
/// Why: Separating feed->JSON shaping from the HTTP fetch makes the departure
/// selection, downstream-arrival matching, line filtering, sorting, and
/// limiting all unit-testable against a hand-built `Feed`.
/// What: For each trip, finds the origin stop with a future departure
/// (`>= now_epoch`); when a destination is set, keeps only trips that also
/// serve it later in the sequence and records that arrival; applies the
/// optional line filter; sorts by departure time; truncates to `limit`.
/// Test: `build_schedule_orders_filters_and_limits`,
/// `build_schedule_requires_downstream_destination`.
pub fn build_schedule(
    feed: &Feed,
    from: &Station,
    to: Option<&Station>,
    line: Option<&str>,
    limit: usize,
    now_epoch: i64,
) -> Value {
    let mut departures: Vec<Departure> = Vec::new();

    for tu in &feed.trip_updates {
        if let Some(l) = line
            && let Some(route) = tu.route_id.as_deref()
            && !route_matches_line(route, l)
        {
            continue;
        }

        // Find the origin stop with a usable future time + its sequence.
        let origin = tu.stops.iter().find(|s| {
            s.stop_id.as_deref() == Some(from.stop_id)
                && s.departure
                    .or(s.arrival)
                    .map(|t| t >= now_epoch)
                    .unwrap_or(false)
        });
        let Some(origin) = origin else { continue };
        let Some(depart_epoch) = origin.departure.or(origin.arrival) else {
            continue;
        };

        // Optional downstream destination arrival (must come after origin).
        let arrive_epoch = match to {
            Some(dest) => {
                let dest_stop = tu.stops.iter().find(|s| {
                    s.stop_id.as_deref() == Some(dest.stop_id)
                        && seq_after(origin.stop_sequence, s.stop_sequence)
                });
                match dest_stop {
                    Some(d) => d.arrival.or(d.departure),
                    // Destination requested but this trip does not serve it.
                    None => continue,
                }
            }
            None => None,
        };

        departures.push(Departure {
            trip_id: tu.trip_id.clone(),
            route_id: tu.route_id.clone(),
            depart_epoch,
            track: origin.assigned_track.clone(),
            arrive_epoch,
        });
    }

    departures.sort_by_key(|d| d.depart_epoch);
    departures.truncate(limit.max(1));

    let items: Vec<Value> = departures
        .iter()
        .map(|d| {
            json!({
                "trip_id": d.trip_id,
                "line": d.route_id.as_deref().map(route_name),
                "depart": format_eastern_hhmm(d.depart_epoch),
                "depart_epoch": d.depart_epoch,
                "track": d.track,
                "arrive": d.arrive_epoch.and_then(format_eastern_hhmm),
                "arrive_epoch": d.arrive_epoch,
            })
        })
        .collect();

    json!({
        "from": from.name,
        "to": to.map(|s| s.name),
        "line": line,
        "count": items.len(),
        "departures": items,
        "timezone": "America/New_York",
        "source": "MTA Metro-North GTFS-Realtime",
    })
}

/// True when `later` comes strictly after `earlier` in a trip's stop sequence.
/// When either sequence is absent, assume ordering holds (feed still lists
/// stops in travel order).
fn seq_after(earlier: Option<u32>, later: Option<u32>) -> bool {
    match (earlier, later) {
        (Some(e), Some(l)) => l > e,
        _ => true,
    }
}

/// Build the alerts result from a decoded feed (pure — no I/O).
///
/// Why: Alert filtering + shaping is testable without the network.
/// What: Maps each alert to `{ lines, header, description }`, applying the
/// optional line filter (an alert passes when ANY informed route matches, or
/// when it names no routes — system-wide alerts always surface).
/// Test: `build_alerts_filters_by_line`.
pub fn build_alerts(feed: &Feed, line: Option<&str>) -> Value {
    let items: Vec<Value> = feed
        .alerts
        .iter()
        .filter(|a| match line {
            Some(l) => {
                a.route_ids.is_empty() || a.route_ids.iter().any(|r| route_matches_line(r, l))
            }
            None => true,
        })
        .map(|a| {
            let lines: Vec<String> = a.route_ids.iter().map(|r| route_name(r)).collect();
            json!({
                "lines": lines,
                "header": a.header,
                "description": a.description,
            })
        })
        .collect();

    json!({
        "line": line,
        "count": items.len(),
        "alerts": items,
        "source": "MTA Metro-North GTFS-Realtime",
    })
}

/// Fetch the raw GTFS-RT feed bytes, attaching `MTA_API_KEY` only if present.
async fn fetch_feed_bytes(client: &reqwest::Client) -> anyhow::Result<Vec<u8>> {
    let mut req = client.get(feed_url());
    if let Ok(key) = std::env::var(API_KEY_ENV)
        && !key.trim().is_empty()
    {
        req = req.header("x-api-key", key);
    }
    let resp = req.send().await?.error_for_status()?;
    Ok(resp.bytes().await?.to_vec())
}

fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .unwrap_or_default()
}

/// The `get_train_schedule` tool.
pub struct GetTrainScheduleTool {
    client: reqwest::Client,
}

impl Default for GetTrainScheduleTool {
    fn default() -> Self {
        Self::new()
    }
}

impl GetTrainScheduleTool {
    pub fn new() -> Self {
        Self {
            client: build_http_client(),
        }
    }

    /// Resolve raw args into a validated query (station names -> stations).
    fn parse_query(args: &Value) -> Result<ScheduleQuery, String> {
        let from_raw = args
            .get("from")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "missing required argument `from` (origin station name)".to_string())?;
        let from = match_station(from_raw)
            .ok_or_else(|| format!("unknown origin station '{from_raw}'"))?;

        let to = match args.get("to").and_then(Value::as_str).map(str::trim) {
            Some(raw) if !raw.is_empty() => Some(
                match_station(raw).ok_or_else(|| format!("unknown destination station '{raw}'"))?,
            ),
            _ => None,
        };

        let line = args
            .get("line")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 10) as usize;

        Ok(ScheduleQuery {
            from,
            to,
            line,
            limit,
        })
    }
}

#[async_trait]
impl ToolExecutor for GetTrainScheduleTool {
    fn name(&self) -> &str {
        "get_train_schedule"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "get_train_schedule",
                "description": "Get upcoming real-time MTA Metro-North departures from a station, optionally toward a destination and/or on a specific line. Returns the next departures in Eastern time with assigned track (when posted) and downstream arrival time when a destination is given. Keyless GTFS-Realtime data.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Origin station name, e.g. 'Grand Central', 'Stamford', 'White Plains'. Partial names accepted." },
                        "to": { "type": "string", "description": "Optional destination station name to filter to trains that serve it and report the arrival time." },
                        "line": { "type": "string", "description": "Optional line filter: Hudson, Harlem, New Haven, New Canaan, Danbury, or Waterbury." },
                        "limit": { "type": "integer", "description": "How many upcoming departures to return (1-10, default 3)." }
                    },
                    "required": ["from"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let query = match Self::parse_query(&args) {
            Ok(q) => q,
            Err(e) => return ToolResult::err(e),
        };
        let bytes = match fetch_feed_bytes(&self.client).await {
            Ok(b) => b,
            Err(e) => {
                return ToolResult::err(format!(
                    "Metro-North feed request failed (service unreachable): {e}"
                ));
            }
        };
        let feed = match decode_feed(&bytes) {
            Ok(f) => f,
            Err(e) => return ToolResult::err(format!("failed to decode Metro-North feed: {e}")),
        };
        let now = chrono::Utc::now().timestamp();
        let out = build_schedule(
            &feed,
            query.from,
            query.to,
            query.line.as_deref(),
            query.limit,
            now,
        );
        match serde_json::to_string(&out) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("failed to serialize schedule: {e}")),
        }
    }
}

/// The `get_train_alerts` tool.
pub struct GetTrainAlertsTool {
    client: reqwest::Client,
}

impl Default for GetTrainAlertsTool {
    fn default() -> Self {
        Self::new()
    }
}

impl GetTrainAlertsTool {
    pub fn new() -> Self {
        Self {
            client: build_http_client(),
        }
    }
}

#[async_trait]
impl ToolExecutor for GetTrainAlertsTool {
    fn name(&self) -> &str {
        "get_train_alerts"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "get_train_alerts",
                "description": "Get active MTA Metro-North service alerts (delays, suspensions, advisories), optionally filtered to one line. System-wide alerts always surface. Keyless GTFS-Realtime data.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "line": { "type": "string", "description": "Optional line filter: Hudson, Harlem, New Haven, New Canaan, Danbury, or Waterbury." }
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let line = args
            .get("line")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let bytes = match fetch_feed_bytes(&self.client).await {
            Ok(b) => b,
            Err(e) => {
                return ToolResult::err(format!(
                    "Metro-North feed request failed (service unreachable): {e}"
                ));
            }
        };
        let feed = match decode_feed(&bytes) {
            Ok(f) => f,
            Err(e) => return ToolResult::err(format!("failed to decode Metro-North feed: {e}")),
        };
        let out = build_alerts(&feed, line.as_deref());
        match serde_json::to_string(&out) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("failed to serialize alerts: {e}")),
        }
    }
}

#[cfg(test)]
#[path = "metro_north_tests.rs"]
mod tests;
