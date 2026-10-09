//! Handler for `trusty-search query`.

use anyhow::{bail, Result};
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::queries::{METHOD_QUERY, METHOD_QUERY_ALL};

/// Classify how a query should be routed based on `--index` / `--indexes`.
///
/// Why: three distinct routing paths share the same `handle_query` entry point.
/// Classifying them upfront keeps the dispatch table readable.
///
/// What:
///   - `SingleIndex(id)` → `search.query` (exact single target).
///   - `MultiIndex(ids)` → `search.query.all` with `{"indexes": [...]}` fan-out.
///   - `AllIndexes`      → `search.query.all` with no `indexes` filter.
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
/// daemon method so `--indexes "*"` and `--indexes "a,b"` work as documented.
///
/// What: starts the daemon if needed, then over its Unix socket (#9214, no TCP
/// fallback):
///   - Single target → `search.query` (`POST /indexes/<id>/search`'s twin).
///   - Comma-list or `"*"` → `search.query.all` with an optional `indexes` filter.
///
/// Test: `query_names_the_socket_when_no_daemon_answers`.
pub async fn handle_query(
    explicit_index: &Option<String>,
    global_json: bool,
    query: String,
    indexes: String,
    top_k: usize,
    full: bool,
) -> Result<()> {
    let client = DaemonClient::resolve()?;
    super::daemon_guard::ensure_daemon_up(&client).await?;
    let target = classify_target(explicit_index, &indexes);
    let (label, body_json) = run_query(&client, &target, &query, top_k).await?;
    if global_json {
        println!("{}", body_json);
    } else {
        render_text(&query, &label, &body_json, full);
    }
    Ok(())
}

/// Send one query over the socket and return `(label, body)`.
///
/// Why: split from [`handle_query`] so the socket exchange is testable without
/// spawning a daemon.
/// What: `search.query` for one index, `search.query.all` otherwise. An
/// unreachable socket is an error naming the socket path; an unknown index
/// reads "not found"; any other refusal carries the daemon's message.
/// Test: `query_names_the_socket_when_no_daemon_answers`.
async fn run_query(
    client: &DaemonClient,
    target: &QueryTarget,
    query: &str,
    top_k: usize,
) -> Result<(String, serde_json::Value)> {
    let (label, method, params) = match target {
        QueryTarget::SingleIndex(id) => (
            id.clone(),
            METHOD_QUERY,
            serde_json::json!({"index_id": id, "body": {"text": query, "top_k": top_k}}),
        ),
        QueryTarget::MultiIndex(ids) => (
            ids.join(","),
            METHOD_QUERY_ALL,
            serde_json::json!({"query": query, "top_k": top_k, "indexes": ids}),
        ),
        QueryTarget::AllIndexes => (
            "*".to_string(),
            METHOD_QUERY_ALL,
            serde_json::json!({"query": query, "top_k": top_k}),
        ),
    };
    match client.call(method, params).await {
        Ok(body) => Ok((label, body)),
        Err(e) if e.is_not_found() => bail!("index '{label}' not found on daemon"),
        Err(e) if e.is_unreachable() => bail!("could not reach daemon: {e}"),
        Err(e) => bail!("daemon returned {e}"),
    }
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

    /// #9214: no daemon on the socket is an error naming the socket, never a
    /// dial of TCP.
    #[tokio::test]
    async fn query_names_the_socket_when_no_daemon_answers() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let client = DaemonClient::at(&socket);
        for target in [
            QueryTarget::SingleIndex("a".into()),
            QueryTarget::MultiIndex(vec!["a".into(), "b".into()]),
            QueryTarget::AllIndexes,
        ] {
            let err = run_query(&client, &target, "q", 5)
                .await
                .expect_err("no daemon answers")
                .to_string();
            assert!(err.contains(&socket.display().to_string()), "{err}");
        }
    }
}
