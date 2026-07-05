//! `tm session delete <id> [--force]` — hard-delete a managed session RECORD (#2012).
//!
//! Why: extracted from `commands::managed` (which sits at the 500-SLOC
//! production cap) into its own file, mirroring how `commands::prune` already
//! separates the `prune-idle` verb from the rest of the managed-session CLI
//! handlers.
//! What: [`session_delete`] POSTs the daemon's hard-delete endpoint and renders
//! the 404/409/success outcomes for the terminal.
//! Test: HTTP path covered by `delete_route_*` in tests/session_manager_mvp.rs;
//! CLI parse by `cli_parses_session_delete`.

/// `tm session delete <id> [--force]` — hard-delete a managed session RECORD (#2012).
///
/// Why: distinct from `decommission` (stop runtime + maybe remove workspace +
/// tombstone) — this permanently drops the record from the store via the
/// existing tombstone-compaction primitive, for an operator who wants a
/// mis-provisioned or stale record gone outright rather than left as a
/// `Decommissioned` tombstone forever. Fail-closed: refuses a RUNNING session
/// unless `--force` is passed, printing an actionable message telling the
/// operator to stop it first (or force it).
/// What: POSTs `/api/v1/sessions/managed/{id}/delete?force=<bool>`. A 404
/// prints "not found"; a 409 (the running-session guard) prints the daemon's
/// actionable reason and returns an `Err` so scripts see a non-zero exit;
/// success prints a confirmation naming the pre-deletion tmux name and state.
/// Test: HTTP path covered by `delete_route_*` in tests/session_manager_mvp.rs;
/// CLI parse by `cli_parses_session_delete`.
pub(crate) async fn session_delete(
    client: &reqwest::Client,
    url: &str,
    id: String,
    force: bool,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/delete"))
        .query(&[("force", force.to_string())])
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    if resp.status() == reqwest::StatusCode::CONFLICT {
        let msg = resp.text().await.unwrap_or_default();
        eprintln!("error: {msg}");
        return Err(anyhow::anyhow!("delete refused: {msg}"));
    }
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    let id_display = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(id.as_str());
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let prior_state = body.get("state").and_then(|v| v.as_str()).unwrap_or("?");
    println!("deleted {id_display} ({name}) [was {prior_state}] — record removed from store");
    Ok(())
}
