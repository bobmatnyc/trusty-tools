//! Handler for `trusty-search query`.

use super::daemon_utils::daemon_base_url;
use anyhow::{bail, Result};
use colored::Colorize;

/// Classify how a query should be routed based on `--index` / `--indexes`.
///
/// Why: three distinct routing paths share the same `handle_query` entry point.
/// Classifying them upfront keeps the dispatch table readable.
///
/// What:
///   - `SingleIndex(id)` → `POST /indexes/<id>/search` (exact single target).
///   - `MultiIndex(ids)` → `POST /search` with `{"indexes": [...]}` fan-out.
///   - `AllIndexes`      → `POST /search` with no `indexes` filter (every index).
///
/// Test: the three paths are covered by `test_query_routing` in `query::tests`.
enum QueryTarget {
    SingleIndex(String),
    MultiIndex(Vec<String>),
    AllIndexes,
}

/// Resolve which indexes the query should target.
///
/// Why: extracted from `handle_query` so routing logic is testable in isolation
/// and the main function remains linear.
///
/// What: applies the following precedence:
///   1. `--index <id>` (explicit single) → `SingleIndex`.
///   2. `--indexes "*"` → `AllIndexes`.
///   3. `--indexes "a,b,c"` (comma-separated) → `MultiIndex([a, b, c])`.
///   4. `--indexes "single"` (no comma, not `*`) → `SingleIndex`.
///
/// Test: covered by `test_query_routing`.
fn classify_target(explicit_index: &Option<String>, indexes: &str) -> QueryTarget {
    if let Some(id) = explicit_index {
        return QueryTarget::SingleIndex(id.clone());
    }
    if indexes == "*" {
        return QueryTarget::AllIndexes;
    }
    if indexes.contains(',') {
        let ids: Vec<String> = indexes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if ids.len() == 1 {
            return QueryTarget::SingleIndex(ids.into_iter().next().unwrap());
        }
        return QueryTarget::MultiIndex(ids);
    }
    QueryTarget::SingleIndex(indexes.to_string())
}

/// Render the human-readable result list for a `query` or `search` response.
///
/// Why: both single-index and multi-index responses share the same `results`
/// array shape; a single renderer avoids drift between the two display paths.
/// What: prints the intent/latency header and per-result file:start-end with
/// a 7-line compact snippet. `target_label` is the display name shown in the
/// header (index id for single, "multi-index" for fan-out).
/// Test: run `trusty-search query "fn authenticate"` and observe formatted output.
fn render_text(query: &str, target_label: &str, body_json: &serde_json::Value, full: bool) {
    let empty: Vec<serde_json::Value> = Vec::new();
    let results = body_json
        .get("results")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let intent = body_json
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let latency = body_json
        .get("latency_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    println!(
        "{} [{}] {} {}",
        "→".cyan(),
        target_label.dimmed(),
        query.bold(),
        format!(
            "(intent={}, {}ms, {} results)",
            intent,
            latency,
            results.len()
        )
        .dimmed()
    );
    if results.is_empty() {
        println!("  {}", "(no matches)".dimmed());
    }
    for (i, r) in results.iter().enumerate() {
        let file = r.get("file").and_then(|v| v.as_str()).unwrap_or("?");
        let start = r.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
        let end = r.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
        let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let reason = r
            .get("match_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        // Multi-index responses carry an `index_id` field; show it when present.
        let index_tag = r
            .get("index_id")
            .and_then(|v| v.as_str())
            .map(|id| format!(" [{}]", id))
            .unwrap_or_default();
        println!(
            "[{}]{} {}:{}-{}  {}",
            i + 1,
            index_tag,
            file,
            start,
            end,
            format!("(score: {:.3}, {})", score, reason).dimmed()
        );
        let snippet = if full {
            r.get("content").and_then(|v| v.as_str()).unwrap_or("")
        } else {
            r.get("compact_snippet")
                .and_then(|v| v.as_str())
                .or_else(|| r.get("content").and_then(|v| v.as_str()))
                .unwrap_or("")
        };
        for line in snippet.lines().take(if full { usize::MAX } else { 7 }) {
            println!("    {}", line);
        }
        if !full && snippet.lines().count() > 7 {
            println!("    {}", "...".dimmed());
        }
    }
}

/// Execute the `trusty-search query` subcommand.
///
/// Why: routes single-index, multi-index, and all-index queries to the correct
/// daemon endpoint so `--indexes "*"` and `--indexes "a,b"` work as documented.
///
/// What:
///   - Single target → `POST /indexes/<id>/search`.
///   - Comma-list or `"*"` → `POST /search` (global fan-out) with an optional `indexes` filter list.
///
/// Test: run `trusty-search query "x" --indexes "*"` against a multi-index daemon and
/// assert results arrive from more than one index.
pub async fn handle_query(
    explicit_index: &Option<String>,
    global_json: bool,
    query: String,
    indexes: String,
    top_k: usize,
    full: bool,
) -> Result<()> {
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;
    let client = trusty_common::server::daemon_http_client()?;

    match classify_target(explicit_index, &indexes) {
        QueryTarget::SingleIndex(id) => {
            let url = format!("{}/indexes/{}/search", base, id);
            let body = serde_json::json!({"text": query, "top_k": top_k});
            let resp = client.post(&url).json(&body).send().await;
            let body_json: serde_json::Value = match resp {
                Ok(r) if r.status().is_success() => {
                    r.json().await.unwrap_or_else(|_| serde_json::json!({}))
                }
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {
                    bail!("index '{}' not found on daemon", id);
                }
                Ok(r) => bail!("daemon returned {}", r.status()),
                Err(e) => bail!("could not reach daemon at {}: {e}", base),
            };
            if global_json {
                println!("{}", body_json);
            } else {
                render_text(&query, &id, &body_json, full);
            }
        }

        QueryTarget::MultiIndex(ids) => {
            let url = format!("{}/search", base);
            let body = serde_json::json!({"query": query, "top_k": top_k, "indexes": ids.clone()});
            let resp = client.post(&url).json(&body).send().await;
            let body_json: serde_json::Value = match resp {
                Ok(r) if r.status().is_success() => {
                    r.json().await.unwrap_or_else(|_| serde_json::json!({}))
                }
                Ok(r) => bail!("daemon returned {} for multi-index search", r.status()),
                Err(e) => bail!("could not reach daemon at {}: {e}", base),
            };
            if global_json {
                println!("{}", body_json);
            } else {
                let label = ids.join(",");
                render_text(&query, &label, &body_json, full);
            }
        }

        QueryTarget::AllIndexes => {
            let url = format!("{}/search", base);
            let body = serde_json::json!({"query": query, "top_k": top_k});
            let resp = client.post(&url).json(&body).send().await;
            let body_json: serde_json::Value = match resp {
                Ok(r) if r.status().is_success() => {
                    r.json().await.unwrap_or_else(|_| serde_json::json!({}))
                }
                Ok(r) => bail!("daemon returned {} for all-indexes search", r.status()),
                Err(e) => bail!("could not reach daemon at {}: {e}", base),
            };
            if global_json {
                println!("{}", body_json);
            } else {
                render_text(&query, "*", &body_json, full);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: pins the routing logic so regressions in classify_target are caught
    /// before they reach users.
    /// What: exercises all four input shapes and asserts the correct variant is
    /// returned.
    /// Test: this test.
    #[test]
    fn test_query_routing_explicit_index_wins() {
        // --index <id> always wins regardless of --indexes.
        let target = classify_target(&Some("my-project".to_string()), "*");
        assert!(matches!(target, QueryTarget::SingleIndex(ref s) if s == "my-project"));
    }

    #[test]
    fn test_query_routing_star_means_all() {
        let target = classify_target(&None, "*");
        assert!(matches!(target, QueryTarget::AllIndexes));
    }

    #[test]
    fn test_query_routing_comma_separated_produces_multi() {
        let target = classify_target(&None, "a,b,c");
        match target {
            QueryTarget::MultiIndex(ids) => {
                assert_eq!(ids, vec!["a", "b", "c"]);
            }
            other => panic!(
                "expected MultiIndex, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_query_routing_single_name_produces_single() {
        let target = classify_target(&None, "my-index");
        assert!(matches!(target, QueryTarget::SingleIndex(ref s) if s == "my-index"));
    }

    #[test]
    fn test_query_routing_comma_with_spaces_trimmed() {
        let target = classify_target(&None, "a , b , c");
        match target {
            QueryTarget::MultiIndex(ids) => {
                assert_eq!(ids, vec!["a", "b", "c"]);
            }
            other => panic!(
                "expected MultiIndex, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_query_routing_single_element_comma_list_collapses_to_single() {
        // "a," or "a, " should not produce MultiIndex([a]) but SingleIndex(a).
        let target = classify_target(&None, "a,");
        assert!(matches!(target, QueryTarget::SingleIndex(ref s) if s == "a"));
    }
}
