//! Socket helpers for the auto-discovery pipeline (#9214).
//!
//! Why: isolates the small async helpers that talk to the daemon so the
//!      orchestrator in `mod.rs` stays focused on the scan loop. They used the
//!      daemon's HTTP API; #9214 moves them onto its Unix socket, so discovery
//!      no longer needs the HTTP address the daemon publishes.
//! What: exports `wait_for_daemon_ready` (polls `search.health` until it
//!       answers) and `fetch_known_index_ids` (`search.indexes.list` →
//!       `HashSet<String>`). Neither falls back to TCP.
//! Test: `wait_for_daemon_ready_gives_up_at_its_budget` and
//!       `fetch_known_index_ids_names_the_socket` below.

use std::time::Duration;
use trusty_search::service::daemon_client::DaemonClient;

/// Poll `search.health` until the daemon answers or `timeout` elapses.
///
/// Why: auto-discovery is spawned in parallel with `run_daemon`, so the socket
///      may not be bound when discovery starts. Without this, the first
///      `register_index_with_daemon` call would race and fail.
/// What: polls every 200 ms up to `timeout`. Returns true on first answer,
///       false at the deadline.
/// Test: `wait_for_daemon_ready_gives_up_at_its_budget`.
pub(super) async fn wait_for_daemon_ready(client: &DaemonClient, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if client.is_up().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Fetch the set of currently-registered index ids from the daemon.
///
/// Why: we must not re-register a project that is already known — the
///      registration is idempotent but the follow-up reindex would still run,
///      wasting CPU and contending with whatever else the daemon is doing.
/// What: `search.indexes.list` and parse the `indexes` array of strings.
/// Test: `fetch_known_index_ids_names_the_socket`.
pub(super) async fn fetch_known_index_ids(
    client: &DaemonClient,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let body = crate::commands::list::fetch_index_list(client).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// #9214: the readiness wait ends at its budget when nothing serves the
    /// socket — it neither hangs nor falls back to a TCP address.
    #[tokio::test]
    async fn wait_for_daemon_ready_gives_up_at_its_budget() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let client = DaemonClient::at(dir.path().join("absent.sock"));
        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let ready = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_daemon_ready(&client, budget),
        )
        .await
        .expect("the wait ends at its budget instead of hanging");
        let elapsed = started.elapsed();
        assert!(!ready, "nothing serves the socket");
        assert!(elapsed >= budget, "the wait gave up early: {elapsed:?}");
        assert!(
            elapsed < Duration::from_secs(1),
            "the wait overran: {elapsed:?}"
        );
    }

    /// #9214: a missing socket is an error naming it.
    #[tokio::test]
    async fn fetch_known_index_ids_names_the_socket() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let err = fetch_known_index_ids(&DaemonClient::at(&socket))
            .await
            .expect_err("no daemon answers")
            .to_string();
        assert!(err.contains(&socket.display().to_string()), "{err}");
    }
}
