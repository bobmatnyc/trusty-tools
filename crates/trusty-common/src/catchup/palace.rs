//! Palace drawer inspection for the DOC-28 catch-up system (#1762).
//!
//! Why: surfaces recent memory palace activity as one of three catch-up sources
//! so the operator is reminded of context stored in trusty-memory during the gap.
//! What: [`fetch_recent_palace_drawers`] calls the `memory_list` MCP tool via
//! [`crate::mcp::memory_rpc::call_memory_tool_at`] (discovery + JSON-RPC,
//! issue #2030 — `memory_url` is already resolved by the caller, e.g.
//! `crate::mcp::memory_rpc::resolve_memory_base_url`) and returns parsed
//! [`DrawerSummary`] values. `memory_list` itself sorts by importance DESC, so
//! this re-sorts client-side by `created_at` DESC to match the old
//! `sort=created_desc` REST contract callers depend on. Fail-open: if the
//! daemon is unreachable or the RPC errors, a warning is emitted to stderr and
//! `None` is returned so the caller can distinguish "unreachable" from
//! "reached, but genuinely empty" (`Some(vec![])`) — palace inspection never
//! blocks catch-up either way.
//! Test: `drawer_summary_from_raw`, `drawer_since_filter`, `drawer_empty_array`,
//! `parses_memory_list_result_and_sorts_newest_first`.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

/// A single memory palace drawer surfaced by the catch-up system.
///
/// Why: callers render this into the "Recent Memory" section of the catch-up
/// digest so the operator is reminded of stored context.
/// What: minimal drawer metadata — title (the drawer's content), tags, and
/// creation timestamp.
/// Test: `drawer_summary_from_raw`.
#[derive(Debug, Clone)]
pub struct DrawerSummary {
    /// Human-readable title of the drawer (the drawer's stored content).
    pub title: String,
    /// Tags associated with the drawer.
    pub tags: Vec<String>,
    /// UTC creation timestamp (None if absent or unparseable).
    pub created_at: Option<DateTime<Utc>>,
}

/// Raw drawer record as returned by the `memory_list` JSON-RPC result.
///
/// Why: isolates JSON deserialization from the public [`DrawerSummary`] type.
/// What: mirrors the payload built by
/// `trusty_memory::tools::memory_ops::handle_memory_list`; unknown fields are
/// ignored.
/// Test: `drawer_summary_from_raw`.
#[derive(Debug, Deserialize)]
struct RawDrawer {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    created_at: Option<String>,
}

/// The `result` payload of a `memory_list` JSON-RPC response.
#[derive(Debug, Default, Deserialize)]
struct MemoryListResult {
    #[serde(default)]
    drawers: Vec<RawDrawer>,
}

impl From<RawDrawer> for DrawerSummary {
    fn from(r: RawDrawer) -> Self {
        let created_at = r.created_at.as_deref().and_then(|s| s.parse().ok());
        DrawerSummary {
            title: r.content,
            tags: r.tags,
            created_at,
        }
    }
}

/// Parse a `memory_list` JSON-RPC `result` payload into drawers sorted
/// newest-first.
///
/// Why: isolated from the HTTP transport so the sort/parse contract is
/// unit-testable without a live daemon.
/// What: deserializes `result` into [`MemoryListResult`], projects into
/// [`DrawerSummary`], and sorts by `created_at` descending (drawers with no
/// parseable timestamp sort last).
/// Test: `parses_memory_list_result_and_sorts_newest_first`.
fn parse_memory_list_result(result: Value) -> anyhow::Result<Vec<DrawerSummary>> {
    let parsed: MemoryListResult = serde_json::from_value(result)?;
    let mut drawers: Vec<DrawerSummary> = parsed
        .drawers
        .into_iter()
        .map(DrawerSummary::from)
        .collect();
    // memory_list sorts by importance DESC; re-sort client-side by created_at
    // DESC to match the old `sort=created_desc` REST contract. Timestamp-less
    // drawers sort last.
    drawers.sort_by_key(|d| std::cmp::Reverse(d.created_at));
    Ok(drawers)
}

/// Fetch recent drawers from a trusty-memory palace, fail-open.
///
/// Why: palace drawers are one of three catch-up activity sources; failure to
/// reach the daemon must not abort the entire catch-up. Returning `Option`
/// (rather than always collapsing to an empty `Vec`) lets [`super::generate_catchup_context`]
/// render a distinct "unreachable" message instead of conflating it with a
/// genuinely empty result (issue #2030, item 5).
/// What: calls `memory_list` via
/// [`crate::mcp::memory_rpc::call_memory_tool_at`] against `memory_url`
/// (already resolved by the caller). On success, parses + sorts via
/// [`parse_memory_list_result`] and — when `since` is `Some` — filters
/// client-side to drawers created after that timestamp. Returns
/// `Some(drawers)` on success (possibly empty) or `None` on any
/// transport/HTTP/RPC/parse error (after logging a warning to stderr).
/// Test: `drawer_since_filter`, `drawer_empty_array`,
/// `live_drawer_fetch` (ignored; requires running daemon).
pub async fn fetch_recent_palace_drawers(
    memory_url: &str,
    palace_id: &str,
    limit: usize,
    since: Option<DateTime<Utc>>,
) -> Option<Vec<DrawerSummary>> {
    let params = json!({ "palace": palace_id, "limit": limit });
    let result = match crate::mcp::memory_rpc::call_memory_tool_at(
        memory_url,
        "memory_list",
        params,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("catchup: could not reach trusty-memory at {memory_url}: {e}");
            return None;
        }
    };
    let mut summaries = match parse_memory_list_result(result) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("catchup: could not parse memory_list response: {e}");
            return None;
        }
    };
    if let Some(since_ts) = since {
        summaries.retain(|d| d.created_at.is_some_and(|ts| ts > since_ts));
    }
    Some(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_raw(title: &str, tags: Vec<&str>, created_at: Option<&str>) -> RawDrawer {
        RawDrawer {
            content: title.to_string(),
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
            created_at: created_at.map(|s| s.to_string()),
        }
    }

    #[test]
    fn drawer_summary_from_raw() {
        let raw = make_raw(
            "My Drawer",
            vec!["rust", "mpm"],
            Some("2026-06-27T10:00:00Z"),
        );
        let summary = DrawerSummary::from(raw);
        assert_eq!(summary.title, "My Drawer");
        assert_eq!(summary.tags, vec!["rust", "mpm"]);
        assert!(summary.created_at.is_some());
    }

    #[test]
    fn drawer_since_filter() {
        // Build a list of two drawers and apply the since filter manually.
        let raw_drawers = vec![
            make_raw("Old", vec![], Some("2026-06-25T00:00:00Z")),
            make_raw("New", vec![], Some("2026-06-27T00:00:00Z")),
        ];
        let mut summaries: Vec<DrawerSummary> =
            raw_drawers.into_iter().map(DrawerSummary::from).collect();
        let since: DateTime<Utc> = "2026-06-26T00:00:00Z".parse().unwrap();
        summaries.retain(|d| d.created_at.is_some_and(|ts| ts > since));
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title, "New");
    }

    #[test]
    fn drawer_empty_array() {
        // Empty drawer list from API → empty vec.
        let raw: Vec<RawDrawer> = vec![];
        let summaries: Vec<DrawerSummary> = raw.into_iter().map(DrawerSummary::from).collect();
        assert!(summaries.is_empty());
    }

    #[test]
    fn drawer_missing_created_at_is_none() {
        let raw = make_raw("No Date", vec![], None);
        let s = DrawerSummary::from(raw);
        assert!(s.created_at.is_none());
    }

    #[test]
    fn parses_memory_list_result_and_sorts_newest_first() {
        let result = json!({
            "palace": "test-palace",
            "drawers": [
                {
                    "drawer_id": "old",
                    "content": "Old note",
                    "importance": 0.9,
                    "tags": ["a"],
                    "created_at": "2026-06-25T00:00:00Z",
                },
                {
                    "drawer_id": "new",
                    "content": "New note",
                    "importance": 0.1,
                    "tags": ["b"],
                    "created_at": "2026-06-27T00:00:00Z",
                },
            ],
        });
        let drawers = parse_memory_list_result(result).unwrap();
        assert_eq!(drawers.len(), 2);
        // Despite "Old note" having higher importance (the tool's native
        // sort), the newest-first re-sort must put "New note" first.
        assert_eq!(drawers[0].title, "New note");
        assert_eq!(drawers[1].title, "Old note");
    }

    /// Live-daemon test: mark #[ignore] so CI doesn't require the daemon running.
    /// Uses an unreachable port (never bound) rather than any hardcoded daemon
    /// default — the point of the test is only "does not panic", exercising the
    /// fail-open `None` path.
    #[tokio::test]
    #[ignore]
    async fn live_drawer_fetch() {
        let drawers =
            fetch_recent_palace_drawers("http://127.0.0.1:19999", "test-palace", 5, None).await;
        // Just verify it returns without panicking.
        let _ = drawers;
    }
}
