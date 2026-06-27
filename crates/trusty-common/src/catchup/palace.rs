//! Palace drawer inspection for the DOC-28 catch-up system (#1762).
//!
//! Why: surfaces recent memory palace activity as one of three catch-up sources
//! so the operator is reminded of context stored in trusty-memory during the gap.
//! What: [`fetch_recent_palace_drawers`] calls the trusty-memory HTTP API and
//! returns parsed [`DrawerSummary`] values. Fail-open: if the daemon is
//! unreachable or returns non-200, a warning is emitted to stderr and an empty
//! vec is returned — palace inspection never blocks catch-up.
//! Test: `drawer_response_parsing`, `drawer_since_filter`, `drawer_empty_array`.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// A single memory palace drawer surfaced by the catch-up system.
///
/// Why: callers render this into the "Recent Memory" section of the catch-up
/// digest so the operator is reminded of stored context.
/// What: minimal drawer metadata — title, tags, and creation timestamp.
/// Test: `drawer_response_parsing`.
#[derive(Debug, Clone)]
pub struct DrawerSummary {
    /// Human-readable title of the drawer.
    pub title: String,
    /// Tags associated with the drawer.
    pub tags: Vec<String>,
    /// UTC creation timestamp (None if absent or unparseable).
    pub created_at: Option<DateTime<Utc>>,
}

/// Raw drawer record as returned by the trusty-memory API.
///
/// Why: isolates JSON deserialization from the public type.
/// What: maps to the API response object; unknown fields are ignored.
/// Test: `drawer_response_parsing`.
#[derive(Debug, Deserialize)]
struct RawDrawer {
    #[serde(default)]
    title: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    created_at: Option<String>,
}

impl From<RawDrawer> for DrawerSummary {
    fn from(r: RawDrawer) -> Self {
        let created_at = r
            .created_at
            .as_deref()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());
        DrawerSummary {
            title: r.title,
            tags: r.tags,
            created_at,
        }
    }
}

/// Fetch recent drawers from a trusty-memory palace, fail-open.
///
/// Why: palace drawers are one of three catch-up activity sources; failure to
/// reach the daemon must not abort the entire catch-up.
/// What: issues `GET {memory_url}/api/v1/palaces/{palace_id}/drawers?sort=created_desc&limit={limit}`;
/// if `since` is Some, filters client-side to drawers created after that timestamp.
/// On any HTTP error, connection refused, or non-200, logs a warning to stderr
/// and returns empty vec.
/// Test: `drawer_response_parsing`, `drawer_since_filter`, `drawer_empty_array`,
/// `live_drawer_fetch` (ignored; requires running daemon).
pub async fn fetch_recent_palace_drawers(
    memory_url: &str,
    palace_id: &str,
    limit: usize,
    since: Option<DateTime<Utc>>,
) -> Vec<DrawerSummary> {
    let url = format!(
        "{}/api/v1/palaces/{}/drawers?sort=created_desc&limit={}",
        memory_url.trim_end_matches('/'),
        palace_id,
        limit
    );
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("catchup: could not build HTTP client: {e}");
            return vec![];
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("catchup: could not reach trusty-memory at {memory_url}: {e}");
            return vec![];
        }
    };
    if !resp.status().is_success() {
        eprintln!(
            "catchup: trusty-memory returned {} for palace drawer query",
            resp.status()
        );
        return vec![];
    }
    let raw: Vec<RawDrawer> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("catchup: could not parse palace drawer response: {e}");
            return vec![];
        }
    };
    let mut drawers: Vec<DrawerSummary> = raw.into_iter().map(DrawerSummary::from).collect();
    if let Some(since_ts) = since {
        drawers.retain(|d| d.created_at.is_some_and(|ts| ts > since_ts));
    }
    drawers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_raw(title: &str, tags: Vec<&str>, created_at: Option<&str>) -> RawDrawer {
        RawDrawer {
            title: title.to_string(),
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
            created_at: created_at.map(|s| s.to_string()),
        }
    }

    #[test]
    fn drawer_response_parsing() {
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

    /// Live-daemon test: mark #[ignore] so CI doesn't require the daemon running.
    #[tokio::test]
    #[ignore]
    async fn live_drawer_fetch() {
        let drawers =
            fetch_recent_palace_drawers("http://127.0.0.1:7990", "test-palace", 5, None).await;
        // Just verify it returns without panicking.
        let _ = drawers;
    }
}
