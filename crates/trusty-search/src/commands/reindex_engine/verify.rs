//! Post-`--force` reindex health check (blue-green safety net).
//!
//! Why: the daemon's reindex mutates the in-memory `CodeIndexer` in place (no
//! shadow slot), so a broken rebuild only surfaces as "search returns nothing"
//! hours later; this check surfaces it immediately after a `--force`.
//! What: `verify_reindex_health` fetches the new chunk count and runs a sanity
//! query over the daemon socket (#9214), erroring if either looks wrong — or
//! if either could not be read at all.
//! Test: `a_status_that_cannot_be_read_is_not_verified`,
//! `a_healthy_index_verifies`.

use super::options::ReindexOutcome;
use crate::commands::daemon_rpc::{call, index_status, rpc_error};
use crate::commands::format::format_with_commas;
use anyhow::{Context as _, Result};
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::queries::METHOD_QUERY;

/// After a `--force` reindex, fetch the new chunk count and run a sanity
/// query. Exits 1 if either looks wrong.
///
/// Why: the daemon's reindex mutates the in-memory `CodeIndexer` in place
/// (no shadow slot). If the rebuild produces a broken index, the only signal
/// the user has is "search returns nothing" hours later. This check surfaces
/// that immediately. #9214: a status or query that could not be READ is
/// reported as "could not verify", never as "0 chunks" or "0 results" — that
/// misreport told the operator a healthy index was broken.
/// What: `search.index.status` for the chunk count, then `search.query` with
/// common tokens. Returns an error if either check fails or cannot run.
/// Test: `a_status_that_cannot_be_read_is_not_verified`,
/// `a_healthy_index_verifies`.
pub(super) async fn verify_reindex_health(
    client: &DaemonClient,
    index_id: &str,
    outcome: &ReindexOutcome,
    prior: Option<u64>,
) -> Result<()> {
    // 1) Chunk count via the status.
    // #9214: an unreadable status is "could not verify", not 0 chunks.
    let status = index_status(client, index_id)
        .await
        .map_err(rpc_error)
        .with_context(|| format!("could not verify index '{index_id}' after the reindex"))?;
    let new_chunks = status
        .get("chunk_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);

    // 2) Sanity query: pick something that hits virtually any source tree.
    let probes = ["fn", "function", "def", "class", "the"];
    let mut got_hit = false;
    for probe in probes {
        let params = serde_json::json!({
            "index_id": index_id,
            "body": { "text": probe, "top_k": 1 },
        });
        // #9214: a failed query is "could not verify", not "no results".
        let json = call(client, METHOD_QUERY, params).await.with_context(|| {
            format!("could not verify index '{index_id}': the sanity query failed")
        })?;
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
