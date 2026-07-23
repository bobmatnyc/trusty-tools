//! `get_weather` — current conditions + short forecast + severe alerts (#3052).
//!
//! Why: Izzie's `izzie-weather` skill promises real weather data with no API
//! key, but the tool it names never existed. This implements it against two
//! keyless endpoints: Open-Meteo (global forecast + geocoding) and the US
//! National Weather Service (severe-weather alerts). No credential is required
//! for either, so the tool is always registerable and never depends on env
//! secrets.
//! What: `GetWeatherTool` accepts a place name OR explicit lat/lon, geocodes a
//! name through Open-Meteo when needed, fetches current + daily forecast, and
//! best-effort overlays active NWS alerts (US only; a failure there degrades
//! to "no alerts" rather than failing the whole call). Returns structured JSON
//! (never prose) so the model can phrase it per the skill's response style.
//! Test: The HTTP-free mapping/labelling/arg-parsing helpers are unit-tested
//! against fixture JSON; the live path is covered by an `#[ignore]`
//! integration test.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

const GEOCODE_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
const NWS_ALERTS_URL: &str = "https://api.weather.gov/alerts/active";
/// NWS requires a descriptive User-Agent; requests without one are rejected.
const NWS_USER_AGENT: &str = "trusty-agents-izzie (https://github.com/bobmatnyc/trusty-tools)";

/// Default location when the caller supplies neither a place nor coordinates:
/// Masa's home base per the `izzie-weather` skill.
const DEFAULT_PLACE: &str = "Hastings-on-Hudson, NY";
const DEFAULT_LAT: f64 = 41.0053;
const DEFAULT_LON: f64 = -73.8779;

/// Parsed `get_weather` arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherArgs {
    /// A place name to geocode, when no explicit coordinates were given.
    pub location: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Forecast horizon in days (clamped to 1..=7).
    pub days: u8,
}

/// Parse and validate the raw tool arguments.
///
/// Why: Keeping arg parsing pure (no I/O) lets a unit test pin the defaulting
/// and clamping rules without a network round-trip.
/// What: Reads optional `location`, `latitude`, `longitude`, and `days`;
/// clamps `days` into 1..=7 (default 3). All fields are optional — an empty
/// object yields the home-base default resolved later in `execute`.
/// Test: `parse_args_defaults_and_clamps`, `parse_args_reads_coordinates`.
pub fn parse_args(args: &Value) -> WeatherArgs {
    let location = args
        .get("location")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let latitude = args.get("latitude").and_then(Value::as_f64);
    let longitude = args.get("longitude").and_then(Value::as_f64);
    let days = args
        .get("days")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 7) as u8;
    WeatherArgs {
        location,
        latitude,
        longitude,
        days,
    }
}

/// Map a WMO weather-interpretation code to a short human label.
///
/// Why: Open-Meteo reports conditions as WMO codes; the model wants words.
/// What: Covers the documented WMO code table; unknown codes fall back to a
/// numeric label so we never fabricate a condition.
/// Test: `weather_code_labels_known_and_unknown`.
pub fn weather_code_label(code: i64) -> String {
    let label = match code {
        0 => "Clear sky",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 | 63 | 65 => "Rain",
        66 | 67 => "Freezing rain",
        71 | 73 | 75 => "Snow",
        77 => "Snow grains",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        other => return format!("WMO code {other}"),
    };
    label.to_string()
}

/// Extract the first geocoding hit as `(display_name, lat, lon)`.
///
/// Why: Turning a place name into coordinates is the first Open-Meteo call;
/// isolating the JSON mapping keeps it testable against a captured response.
/// What: Reads `results[0]`; assembles a "Name, Admin1, Country" label from
/// whatever fields are present. Returns `None` when there are no results.
/// Test: `geocode_first_result_maps_fields`, `geocode_first_result_empty`.
pub fn geocode_first_result(body: &Value) -> Option<(String, f64, f64)> {
    let first = body.get("results").and_then(Value::as_array)?.first()?;
    let lat = first.get("latitude").and_then(Value::as_f64)?;
    let lon = first.get("longitude").and_then(Value::as_f64)?;
    let name = first.get("name").and_then(Value::as_str).unwrap_or("");
    let admin1 = first.get("admin1").and_then(Value::as_str).unwrap_or("");
    let country = first.get("country").and_then(Value::as_str).unwrap_or("");
    let label = [name, admin1, country]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Some((label, lat, lon))
}

/// Shape an Open-Meteo forecast response into the tool's structured output.
///
/// Why: The raw response is column-oriented (parallel arrays); the model wants
/// a compact per-day object plus a `current` summary.
/// What: Maps `current` fields and zips the `daily.*` arrays into day objects,
/// translating WMO codes to labels. Missing fields degrade to `null` rather
/// than aborting.
/// Test: `forecast_maps_current_and_daily`.
pub fn map_forecast(location: &str, lat: f64, lon: f64, body: &Value) -> Value {
    let current = body.get("current").cloned().unwrap_or(Value::Null);
    let current_out = json!({
        "temperature_f": current.get("temperature_2m").and_then(Value::as_f64),
        "feels_like_f": current.get("apparent_temperature").and_then(Value::as_f64),
        "humidity_pct": current.get("relative_humidity_2m").and_then(Value::as_f64),
        "wind_mph": current.get("wind_speed_10m").and_then(Value::as_f64),
        "precipitation_in": current.get("precipitation").and_then(Value::as_f64),
        "conditions": current
            .get("weather_code")
            .and_then(Value::as_i64)
            .map(weather_code_label),
    });

    let daily = body.get("daily");
    let dates = daily
        .and_then(|d| d.get("time"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let col = |name: &str| -> Vec<Value> {
        daily
            .and_then(|d| d.get(name))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let highs = col("temperature_2m_max");
    let lows = col("temperature_2m_min");
    let codes = col("weather_code");
    let precip_prob = col("precipitation_probability_max");
    let precip_sum = col("precipitation_sum");
    let get = |v: &[Value], i: usize| v.get(i).cloned().unwrap_or(Value::Null);

    let days: Vec<Value> = dates
        .iter()
        .enumerate()
        .map(|(i, date)| {
            json!({
                "date": date,
                "high_f": get(&highs, i),
                "low_f": get(&lows, i),
                "conditions": get(&codes, i).as_i64().map(weather_code_label),
                "precip_chance_pct": get(&precip_prob, i),
                "precip_in": get(&precip_sum, i),
            })
        })
        .collect();

    json!({
        "location": location,
        "latitude": lat,
        "longitude": lon,
        "current": current_out,
        "daily": days,
    })
}

/// Extract active NWS alerts as compact objects.
///
/// Why: The `izzie-weather` skill surfaces severe-weather alerts proactively;
/// the model needs event/severity/headline, not the full GeoJSON.
/// What: Maps each `features[].properties`; empty/missing features yield an
/// empty vector (the common no-alerts case).
/// Test: `nws_alerts_maps_features`, `nws_alerts_empty`.
pub fn map_nws_alerts(body: &Value) -> Vec<Value> {
    body.get("features")
        .and_then(Value::as_array)
        .map(|features| {
            features
                .iter()
                .filter_map(|f| f.get("properties"))
                .map(|p| {
                    json!({
                        "event": p.get("event").and_then(Value::as_str),
                        "severity": p.get("severity").and_then(Value::as_str),
                        "headline": p.get("headline").and_then(Value::as_str),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `get_weather` tool.
pub struct GetWeatherTool {
    client: reqwest::Client,
}

impl Default for GetWeatherTool {
    fn default() -> Self {
        Self::new()
    }
}

impl GetWeatherTool {
    /// Build the tool with a short-timeout HTTP client.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap_or_default();
        Self { client }
    }

    /// Geocode a place name to `(label, lat, lon)` via Open-Meteo.
    async fn geocode(&self, place: &str) -> anyhow::Result<Option<(String, f64, f64)>> {
        let resp = self
            .client
            .get(GEOCODE_URL)
            .query(&[("name", place), ("count", "1"), ("language", "en")])
            .send()
            .await?
            .error_for_status()?;
        let body: Value = resp.json().await?;
        Ok(geocode_first_result(&body))
    }

    /// Fetch the current + daily forecast for a coordinate.
    async fn forecast(&self, lat: f64, lon: f64, days: u8) -> anyhow::Result<Value> {
        let resp = self
            .client
            .get(FORECAST_URL)
            .query(&[
                ("latitude", lat.to_string()),
                ("longitude", lon.to_string()),
                (
                    "current",
                    "temperature_2m,relative_humidity_2m,apparent_temperature,precipitation,\
                     weather_code,wind_speed_10m"
                        .to_string(),
                ),
                (
                    "daily",
                    "weather_code,temperature_2m_max,temperature_2m_min,\
                     precipitation_probability_max,precipitation_sum"
                        .to_string(),
                ),
                ("temperature_unit", "fahrenheit".to_string()),
                ("wind_speed_unit", "mph".to_string()),
                ("precipitation_unit", "inch".to_string()),
                ("timezone", "auto".to_string()),
                ("forecast_days", days.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    /// Best-effort active NWS alerts for a coordinate (US only). Any failure
    /// degrades to an empty list — a weather answer must not break because the
    /// alerts service is unreachable or the point is outside the US.
    async fn nws_alerts(&self, lat: f64, lon: f64) -> Vec<Value> {
        let point = format!("{lat:.4},{lon:.4}");
        let result = self
            .client
            .get(NWS_ALERTS_URL)
            .header("User-Agent", NWS_USER_AGENT)
            .header("Accept", "application/geo+json")
            .query(&[("point", point.as_str())])
            .send()
            .await;
        match result {
            Ok(resp) => match resp.error_for_status() {
                Ok(ok) => ok
                    .json::<Value>()
                    .await
                    .map(|b| map_nws_alerts(&b))
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }
}

#[async_trait]
impl ToolExecutor for GetWeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current conditions plus a short daily forecast (and active US severe-weather alerts) for a location. Provide EITHER a place name via `location` OR explicit `latitude`/`longitude`. Keyless (Open-Meteo + National Weather Service). Defaults to Hastings-on-Hudson, NY when nothing is supplied. Temperatures in °F, wind in mph, precipitation in inches.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {
                            "type": "string",
                            "description": "Place name to look up, e.g. 'Stamford, CT' or 'Paris, France'. Omit if passing coordinates."
                        },
                        "latitude": { "type": "number", "description": "Latitude in decimal degrees (use with longitude)." },
                        "longitude": { "type": "number", "description": "Longitude in decimal degrees (use with latitude)." },
                        "days": { "type": "integer", "description": "Forecast horizon in days (1-7, default 3)." }
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let parsed = parse_args(&args);

        // Resolve coordinates: explicit lat/lon win; else geocode a place;
        // else fall back to the home-base default.
        let (label, lat, lon) = match (parsed.latitude, parsed.longitude) {
            (Some(lat), Some(lon)) => (
                parsed
                    .location
                    .unwrap_or_else(|| format!("{lat:.4},{lon:.4}")),
                lat,
                lon,
            ),
            _ => match parsed.location {
                Some(place) => match self.geocode(&place).await {
                    Ok(Some(hit)) => hit,
                    Ok(None) => {
                        return ToolResult::err(format!(
                            "No location found matching '{place}'. Try a more specific name (city, state/country)."
                        ));
                    }
                    Err(e) => {
                        return ToolResult::err(format!(
                            "Weather geocoding for '{place}' failed (service unreachable): {e}"
                        ));
                    }
                },
                None => (DEFAULT_PLACE.to_string(), DEFAULT_LAT, DEFAULT_LON),
            },
        };

        let forecast = match self.forecast(lat, lon, parsed.days).await {
            Ok(body) => body,
            Err(e) => {
                return ToolResult::err(format!(
                    "Weather forecast request failed (Open-Meteo unreachable): {e}"
                ));
            }
        };

        let mut out = map_forecast(&label, lat, lon, &forecast);
        let alerts = self.nws_alerts(lat, lon).await;
        if let Value::Object(ref mut map) = out {
            map.insert("alerts".to_string(), Value::Array(alerts));
            map.insert(
                "source".to_string(),
                Value::String("Open-Meteo (forecast) + NWS (alerts)".to_string()),
            );
        }

        match serde_json::to_string(&out) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("failed to serialize weather result: {e}")),
        }
    }
}

#[cfg(test)]
#[path = "weather_tests.rs"]
mod tests;
