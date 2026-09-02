//! `POST /upgrade` handler.
//!
//! Why: split out of `health.rs` in #6688, which pushed that file past the
//! 500-SLOC production cap. The two endpoints shared a file only by history —
//! `/health` is a 2s-interval probe and `/upgrade` drives `cargo install` and a
//! self-restart, and nothing but the axum `State` type is common to them.
//! What: `upgrade_handler` and its typed request body, moved verbatim.
//! Test: unchanged by the move — the handler reaches crates.io and `cargo
//! install`, so its coverage is the MCP `upgrade` tool's end-to-end path and
//! the manual `curl` in `upgrade_handler`'s own doc.
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

use super::state::SearchAppState;

/// Request body for `POST /upgrade` (issue #537).
///
/// Why: typed body avoids raw JSON field extraction in the handler, and serde
/// provides friendly error messages for malformed requests.
/// What: mirrors the MCP tool schema: `check` (default true) and `confirm`.
/// Test: the MCP `upgrade` tool calls this endpoint.
#[derive(Deserialize)]
pub(super) struct UpgradeRequest {
    #[serde(default = "bool_true")]
    check: bool,
    #[serde(default)]
    confirm: bool,
}

/// `POST /upgrade` — check for or install a new trusty-search version (issue #537).
///
/// Why: Exposes the upgrade workflow over HTTP so the MCP dispatcher (which
/// calls the daemon's REST API) can trigger an upgrade and receive the response
/// before the daemon self-exits. Never silently auto-installs.
///
/// What:
/// - `check=true` or `confirm=false`: query crates.io and return version info.
/// - `confirm=true`: install via `cargo install --locked`, health-gate, then
///   schedule a 500 ms delayed exit (to flush this response) and return the
///   result. When launchd-supervised the daemon exits non-zero so launchd
///   respawns with the new binary. When unsupervised a restart hint is returned.
///
/// Test: manual via `curl -X POST http://127.0.0.1:$(trusty-search port)/upgrade \
///  -H 'Content-Type: application/json' -d '{"check":true}'`.
pub(super) async fn upgrade_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(body): Json<UpgradeRequest>,
) -> Json<serde_json::Value> {
    let crate_name = env!("CARGO_PKG_NAME");
    let current = env!("CARGO_PKG_VERSION");

    let info = trusty_common::update::check_crates_io(crate_name, current).await;

    let (latest, is_update) = match &info {
        Some(u) => (u.latest.as_str(), true),
        None => (current, false),
    };

    if body.check || !body.confirm {
        let msg = if is_update {
            format!(
                "Update available: {crate_name} {latest} (you have {current}). \
                 POST with confirm=true to install."
            )
        } else {
            format!("{crate_name} {current} is already up to date.")
        };
        return Json(serde_json::json!({
            "status": "checked",
            "current": current,
            "latest": latest,
            "update_available": is_update,
            "message": msg
        }));
    }

    if !is_update {
        return Json(serde_json::json!({
            "status": "up_to_date",
            "current": current,
            "message": format!("{crate_name} {current} is already up to date.")
        }));
    }

    let latest_owned = latest.to_string();
    let crate_name_owned = crate_name.to_string();
    let update_slot = state.update_available.clone();
    let response = serde_json::json!({
        "status": "installing",
        "current": current,
        "latest": latest_owned,
        "message": format!(
            "Installing {crate_name} {latest_owned} — daemon will restart \
             under launchd (or print a restart hint if not supervised)."
        )
    });

    // Spawn the install on a delayed task so this handler can return the
    // response to the HTTP client (and thus to the MCP caller) before the
    // process might exit. 500 ms gives the TCP stack time to flush.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        match trusty_common::update::upgrade_and_restart(&crate_name_owned, &crate_name_owned).await
        {
            Ok(Some(hint)) => {
                tracing::info!("{hint}");
                eprintln!("{hint}");
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("upgrade_and_restart failed: {e:#}");
                eprintln!("[trusty-search] upgrade failed: {e:#}");
                if let Ok(mut g) = update_slot.lock() {
                    *g = None;
                }
            }
        }
    });

    Json(response)
}

fn bool_true() -> bool {
    true
}
