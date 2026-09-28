//! `POST /api/v1/build-lease/decisions` — the daemon's log of every build-lease
//! admission decision (#8261 increment two).
//!
//! Why: `tm build-lease` decides locally (a flock needs no daemon), but an
//! operator reconstructing why a build waited needs one place that saw every
//! decision on the machine with its readings. The daemon log is that place.
//! What: accepts the decision JSON `tm build-lease` posts and writes one
//! `tracing` INFO line (WARN for a timeout) carrying the verdict, command,
//! readings, reasons, degraded signals, holders and ceiling. It stores nothing
//! and gates nothing.
//! Test: `a_decision_is_logged_and_acknowledged`.

use std::sync::Arc;

use axum::{Json, Router, http::StatusCode, routing::post};
use serde_json::Value;

use super::state::DaemonState;

/// The route, merged by `builder_slot_routes::router` (`api.rs` sits at its
/// frozen line-cap budget).
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route("/api/v1/build-lease/decisions", post(log_decision))
}

/// Log one decision.
///
/// Test: `a_decision_is_logged_and_acknowledged`.
async fn log_decision(Json(body): Json<Value>) -> StatusCode {
    let field = |key: &str| body.get(key).map(ToString::to_string).unwrap_or_default();
    let verdict = body
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if verdict == "admitted" {
        tracing::info!(
            target: "build_lease",
            verdict,
            command = %field("command"),
            pid = %field("pid"),
            held = %field("held"),
            n_effective = %field("n_effective"),
            ceiling = %field("ceiling"),
            degraded = %field("degraded"),
            readings = %field("readings"),
            "build-lease admission decision"
        );
    } else {
        tracing::warn!(
            target: "build_lease",
            verdict,
            command = %field("command"),
            pid = %field("pid"),
            held = %field("held"),
            n_effective = %field("n_effective"),
            ceiling = %field("ceiling"),
            withheld = %field("withheld"),
            degraded = %field("degraded"),
            holders = %field("holders"),
            readings = %field("readings"),
            "build-lease admission decision"
        );
    }
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_decision_is_logged_and_acknowledged() {
        let body = serde_json::json!({
            "verdict": "timed-out",
            "command": "cargo test",
            "readings": "host h, memory pressure warn",
            "withheld": ["memory pressure is warn"],
            "ceiling": 4,
        });
        assert_eq!(log_decision(Json(body)).await, StatusCode::NO_CONTENT);
    }
}
