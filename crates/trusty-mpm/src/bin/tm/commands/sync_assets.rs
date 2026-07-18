//! `tm sessions sync-assets <id>|--all` — force a re-deploy of a live managed
//! session's agents/skills/output-styles against the current catalog (issue
//! #2444).
//!
//! Why: extracted into its own file (mirroring `delete.rs`/`prune.rs`) since
//! `commands::managed` sits at the 500-SLOC production cap. `#2002`'s asset
//! deployment is one-shot at launch, so a long-lived session's deployed
//! `.claude/{agents,skills}` never re-syncs when the bundled/catalog source
//! changes underneath it — `tm sessions ls`'s `[stale-assets]` marker (see
//! `commands::managed::format_state_column`) flags exactly this. These two
//! functions are the fix.
//! What: [`session_sync_assets`] POSTs the per-session daemon route
//! (resolving `id_or_name` to a managed id via
//! [`super::managed_route::resolve_managed_match`] first); [`session_sync_assets_all`]
//! POSTs the fleet-wide route. Both print a one-line-per-session summary of
//! what changed.
//! Test: `cli_parses_sessions_sync_assets`, `cli_parses_sessions_sync_assets_all`
//! in `tests_behavior_d_tests.rs`; the HTTP round-trip is covered by
//! `sync_assets_route_*` in `tests/session_manager_mvp.rs`.

use serde::Deserialize;

/// Wire shape for one session's sync-assets outcome — mirrors
/// `daemon::managed_routes::sync_assets::SyncAssetsResponse` field-for-field.
#[derive(Debug, Deserialize)]
struct SyncAssetsResult {
    id: String,
    agents_deployed: Vec<String>,
    agents_skipped: Vec<String>,
    skills_deployed: Vec<String>,
    skills_skipped: Vec<String>,
    output_style_synced: bool,
}

/// Wire shape for the fleet-wide sync-assets response — mirrors
/// `daemon::managed_routes::sync_assets::SyncAllAssetsResponse`.
#[derive(Debug, Deserialize)]
struct SyncAllAssetsResult {
    synced: Vec<SyncAssetsResult>,
    skipped: Vec<String>,
    errors: Vec<String>,
}

/// Render one session's sync-assets result as a terminal-friendly line.
///
/// Why: shared by both the single-session and `--all` paths so their output
/// is byte-for-byte consistent per session.
/// What: `"<id>: N agent(s), M skill(s) refreshed" [+ ", output style synced"]`,
/// or `"<id>: already up to date"` when nothing changed.
fn render_result(r: &SyncAssetsResult) {
    if r.agents_deployed.is_empty() && r.skills_deployed.is_empty() {
        println!("{}: already up to date", r.id);
        return;
    }
    print!(
        "{}: {} agent(s), {} skill(s) refreshed",
        r.id,
        r.agents_deployed.len(),
        r.skills_deployed.len()
    );
    if !r.agents_skipped.is_empty() || !r.skills_skipped.is_empty() {
        print!(
            " ({} agent(s), {} skill(s) skipped — user-modified)",
            r.agents_skipped.len(),
            r.skills_skipped.len()
        );
    }
    if r.output_style_synced {
        print!(", output style synced");
    }
    println!();
}

/// `tm sessions sync-assets <id_or_name>` — re-sync ONE session's assets.
///
/// Why: the concrete fix for a single session flagged `[stale-assets]` on
/// `tm sessions ls` — re-run the same deployers launch uses, without a full
/// relaunch. `id_or_name` is resolved to a managed session id first (friendly
/// names, not just raw UUIDs, are accepted everywhere else in this CLI).
/// What: resolves `id_or_name` via [`super::managed_route::resolve_managed_match`]
/// (a `bail!` if it does not resolve to any managed session), POSTs
/// `/api/v1/sessions/managed/{id}/sync-assets`, and renders the result.
/// Test: `cli_parses_sessions_sync_assets`.
pub(crate) async fn session_sync_assets(
    client: &reqwest::Client,
    url: &str,
    id_or_name: String,
) -> anyhow::Result<()> {
    let Some(id) = super::managed_route::resolve_managed_match(client, url, &id_or_name).await
    else {
        anyhow::bail!("managed session '{id_or_name}' not found");
    };
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/sync-assets"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("managed session '{id}' not found");
    }
    let result: SyncAssetsResult = resp.error_for_status()?.json().await?;
    render_result(&result);
    Ok(())
}

/// `tm sessions sync-assets --all` — re-sync EVERY syncable session's assets.
///
/// Why: an operator refreshing a catalog/bundled skill or agent (a `tm
/// install`, or a fresh `tm` binary) wants to push it to every live session
/// in one call.
/// What: POSTs `/api/v1/sessions/managed/sync-assets`, prints one line per
/// synced session (via [`render_result`]), a `skipped` count (sessions with
/// no live workspace — `provisioning`/`decommissioned`), and any per-session
/// errors (a single session's failure never aborts the rest of the fleet
/// report).
/// Test: `cli_parses_sessions_sync_assets_all`.
pub(crate) async fn session_sync_assets_all(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/sync-assets"))
        .send()
        .await?;
    let result: SyncAllAssetsResult = resp.error_for_status()?.json().await?;

    if result.synced.is_empty() {
        println!("no syncable sessions found");
    }
    for r in &result.synced {
        render_result(r);
    }
    if !result.skipped.is_empty() {
        println!(
            "{} session(s) skipped (provisioning/decommissioned)",
            result.skipped.len()
        );
    }
    for err in &result.errors {
        eprintln!("error: {err}");
    }
    if !result.errors.is_empty() {
        anyhow::bail!(
            "{} of {} session(s) failed to sync",
            result.errors.len(),
            result.synced.len() + result.errors.len()
        );
    }
    Ok(())
}

// Unit tests live in sync_assets_tests.rs (test-file budget: 1500 SLOC),
// mirroring the managed.rs / managed_tests.rs split (#2457).
#[cfg(test)]
#[path = "sync_assets_tests.rs"]
mod tests;
