//! Izzie platform-hosted tools: weather + Metro-North (epic #3052).
//!
//! Why: The `izzie-weather` and `izzie-metro-north` skills reference three
//! tools — `get_weather`, `get_train_schedule`, `get_train_alerts` — that were
//! declared by the persona but never implemented, so the assistant could only
//! ever refuse ("I don't have a weather tool"), the exact anti-pattern those
//! skills warn against. This module implements them as ordinary
//! platform-hosted `ToolExecutor`s, registered alongside git / ticketing /
//! `system_status` in the persona dispatch path and admitted per-persona by
//! the existing `[tools].allow` gate.
//! What: `izzie_tools()` returns the three executors as a `Vec<Arc<dyn
//! ToolExecutor>>` for one-call registration. All three are keyless (Open-Meteo
//! + NWS for weather; the keyless MTA GTFS-Realtime feed for trains); any
//! optional gateway credential is read from an env var with graceful
//! degradation, never hardcoded.
//! Test: Per-tool unit tests live beside each implementation; this module's
//! `izzie_tools_exposes_expected_names` pins the registered name set.

use std::sync::Arc;

use crate::tools::traits::ToolExecutor;

pub mod gtfs_rt;
pub mod metro_north;
pub mod stations;
pub mod weather;

pub use metro_north::{GetTrainAlertsTool, GetTrainScheduleTool};
pub use weather::GetWeatherTool;

/// Construct the three izzie platform-hosted tools for registration.
///
/// Why: Mirrors `git_tools()` — one call hands the dispatch path every tool in
/// this family so the registration site stays a single line.
/// What: Returns `get_weather`, `get_train_schedule`, `get_train_alerts` as
/// boxed executors. Each builds its own short-timeout HTTP client; construction
/// never performs I/O, so this is safe to call at session start.
/// Test: `izzie_tools_exposes_expected_names`.
pub fn izzie_tools() -> Vec<Arc<dyn ToolExecutor>> {
    vec![
        Arc::new(GetWeatherTool::new()),
        Arc::new(GetTrainScheduleTool::new()),
        Arc::new(GetTrainAlertsTool::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn izzie_tools_exposes_expected_names() {
        let tools = izzie_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            vec!["get_weather", "get_train_schedule", "get_train_alerts"]
        );
    }
}
