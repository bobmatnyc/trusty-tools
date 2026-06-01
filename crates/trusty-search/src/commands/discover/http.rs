//! HTTP helper functions for the auto-discovery pipeline.
//!
//! Why: isolates the two small async helpers that talk to the daemon's HTTP
//!      API so the orchestrator in `mod.rs` stays focused on the scan loop.
//! What: exports `wait_for_daemon_ready` (polls `/health` until ready) and
//!       `fetch_known_index_ids` (GET `/indexes` → `HashSet<String>`).
//! Test: both functions are side-effect-only; covered indirectly by the daemon
//!       startup integration tests.

use std::time::Duration;

/// Poll the daemon's `/health` endpoint until it returns 200 or the deadline
/// fires.
///
/// Why: auto-discovery is spawned in parallel with `run_daemon`, so the HTTP
///      listener may not be ready when discovery starts probing. Without this,
///      the first `register_index_with_daemon` call would race and fail.
/// What: polls every 200 ms up to `timeout`. Returns true on first success,
///       false if the deadline elapses with no successful response.
/// Test: side-effect-only; covered indirectly by the daemon startup integration
///       tests.
pub(super) async fn wait_for_daemon_ready(
    client: &reqwest::Client,
    base: &str,
    timeout: Duration,
) -> bool {
    let url = format!("{base}/health");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if client
            .get(&url)
            .send()
            .await
            .ok()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Fetch the set of currently-registered index ids from the daemon.
///
/// Why: we must not re-register a project that is already known — the daemon's
///      `POST /indexes` is idempotent but the follow-up reindex would still
///      run, wasting CPU and contending with whatever else the daemon is
///      doing.
/// What: GET `/indexes` and parse the `indexes` array of strings.
/// Test: covered indirectly by `auto_discover_and_index` integration runs.
pub(super) async fn fetch_known_index_ids(
    client: &reqwest::Client,
    base: &str,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let url = format!("{base}/indexes");
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("daemon returned {} for {url}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let set = body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    Ok(set)
}
