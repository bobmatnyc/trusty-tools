//! Post-`--force` reindex health check (blue-green safety net).
//!
//! Why: the daemon's reindex mutates the in-memory `CodeIndexer` in place (no
//! shadow slot), so a broken rebuild only surfaces as "search returns nothing"
//! hours later; this check surfaces it immediately after a `--force`.
//! What: `verify_reindex_health` fetches the new chunk count and runs a sanity
//! query, erroring if either looks wrong.
//! Test: covered indirectly by `index --force` integration tests.

use super::options::ReindexOutcome;
use crate::commands::format::format_with_commas;
use anyhow::Result;
use colored::Colorize;

/// After a `--force` reindex, fetch the new chunk count and run a sanity
/// query. Exits 1 if either looks wrong.
///
/// Why: the daemon's reindex mutates the in-memory `CodeIndexer` in place
/// (no shadow slot). If the rebuild produces a broken index, the only signal
/// the user has is "search returns nothing" hours later. This check surfaces
/// that immediately.
/// What: fetches `/status` for the chunk count, then probes the search
/// endpoint with common tokens. Returns an error if either check fails.
/// Test: covered indirectly by `index --force` integration tests.
pub(super) async fn verify_reindex_health(
    client: &reqwest::Client,
    base: &str,
    index_id: &str,
    outcome: &ReindexOutcome,
    prior: Option<u64>,
) -> Result<()> {
    // 1) Chunk count via /status.
    let status_url = format!("{}/indexes/{}/status", base, index_id);
    let new_chunks = match client.get(&status_url).send().await {
        Ok(r) if r.status().is_success() => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("chunk_count").and_then(|n| n.as_u64()))
            .unwrap_or(0),
        _ => 0,
    };

    // 2) Sanity query: pick something that hits virtually any source tree.
    let search_url = format!("{}/indexes/{}/search", base, index_id);
    let probes = ["fn", "function", "def", "class", "the"];
    let mut got_hit = false;
    for probe in probes {
        let body = serde_json::json!({ "text": probe, "top_k": 1 });
        if let Ok(resp) = client.post(&search_url).json(&body).send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let n = json
                        .get("results")
                        .and_then(|r| r.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    if n > 0 {
                        got_hit = true;
                        break;
                    }
                }
            }
        }
    }

    let healthy = new_chunks > 0 && got_hit && outcome.errors == 0;
    let was = prior
        .map(|p| format!(" (was {})", format_with_commas(p)))
        .unwrap_or_default();
    if healthy {
        println!(
            "{} Reindex complete: {} chunks{}",
            "\u{2713}".green(),
            format_with_commas(new_chunks),
            was
        );
        Ok(())
    } else {
        anyhow::bail!(
            "Reindex produced unhealthy index: {} chunks{}, sanity query {} \u{2014} \
             old index NOT preserved (daemon reindex is in-place; \
             see crates/trusty-search/src/service/reindex.rs)",
            format_with_commas(new_chunks),
            was,
            if got_hit { "ok" } else { "returned 0 results" }
        );
    }
}
