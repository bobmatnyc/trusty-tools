//! Palace drawer inspection for the DOC-28 catch-up system (#1762).
//!
//! Why: surfaces recent memory palace activity as one of three catch-up sources
//! so the operator is reminded of context stored in trusty-memory during the gap.
//! What: [`fetch_recent_palace_drawers`] calls the `memory_list` MCP tool via
//! [`crate::memory_rpc::call_memory_tool_at`] over the daemon's Unix socket
//! (#6286 — `memory_socket` is already resolved by the caller, e.g. by
//! `crate::memory_rpc::resolve_memory_socket`) and returns parsed
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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A single memory palace drawer surfaced by the catch-up system.
///
/// Why: callers render this into the "Recent Memory" section of the catch-up
/// digest so the operator is reminded of stored context.
/// What: minimal drawer metadata — title (the drawer's content), tags, and
/// creation timestamp.
/// Test: `drawer_summary_from_raw`.
#[derive(Debug, Clone, Serialize)]
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

/// Did the daemon answer that the palace does not exist (#9026)?
///
/// Why: a project whose palace was never created is common — the slug is
/// derived from the repo, and nothing creates the palace until the first
/// write. The daemon reports that as a coded not-found refusal; treating it
/// as an outage wrote one stderr line per catch-up run for every such project.
/// What: true only for a [`crate::memory_rpc::MemoryRpcError`] whose code is
/// the not-found code. A transport failure, or any other refusal, is false.
/// Test: `an_absent_palace_is_reached_and_empty_not_unreachable`.
fn is_absent_palace(e: &anyhow::Error) -> bool {
    e.downcast_ref::<crate::memory_rpc::MemoryRpcError>()
        .is_some_and(crate::memory_rpc::MemoryRpcError::is_not_found)
}

/// The stderr line for a `memory_list` call that failed other than as an
/// absent palace (#9026).
///
/// Why: the line printed `{e}`, which renders only the outermost context —
/// "call memory_list on the trusty-memory daemon at …" — so a timeout, a
/// hardened-connect refusal of the socket file, and a dead socket all read
/// the same, and a socket the client refused before sending anything looked
/// like an unreachable daemon.
/// What: names the socket and the whole `anyhow` cause chain (`{e:#}`).
/// Test: `the_unreachable_warning_carries_the_whole_cause_chain_9026`.
fn memory_list_failed_warning(memory_socket: &std::path::Path, e: &anyhow::Error) -> String {
    format!(
        "catchup: memory_list on trusty-memory at {} failed: {e:#}",
        memory_socket.display()
    )
}

/// Fetch recent drawers from a trusty-memory palace, fail-open.
///
/// Why: palace drawers are one of three catch-up activity sources; failure to
/// reach the daemon must not abort the entire catch-up. Returning `Option`
/// (rather than always collapsing to an empty `Vec`) lets [`super::generate_catchup_context`]
/// render a distinct "unreachable" message instead of conflating it with a
/// genuinely empty result (issue #2030, item 5).
/// What: calls `memory_list` via
/// [`crate::memory_rpc::call_memory_tool_at`] against `memory_socket`
/// (already resolved by the caller). On success, parses + sorts via
/// [`parse_memory_list_result`] and — when `since` is `Some` — filters
/// client-side to drawers created after that timestamp. Returns
/// `Some(drawers)` on success (possibly empty), `Some(vec![])` without a
/// warning when the daemon reports the palace absent (#9026), or `None` on any
/// other transport/RPC/parse error (after logging a warning to stderr).
/// Test: `drawer_since_filter`, `drawer_empty_array`,
/// `live_drawer_fetch` (ignored; requires running daemon).
pub async fn fetch_recent_palace_drawers(
    memory_socket: &std::path::Path,
    palace_id: &str,
    limit: usize,
    since: Option<DateTime<Utc>>,
) -> Option<Vec<DrawerSummary>> {
    let params = json!({ "palace": palace_id, "limit": limit });
    let result =
        match crate::memory_rpc::call_memory_tool_at(memory_socket, "memory_list", params).await {
            Ok(v) => v,
            // #9026: the daemon answered that this project's palace does not
            // exist yet. That is "reached, nothing stored", not an outage.
            Err(e) if is_absent_palace(&e) => return Some(Vec::new()),
            Err(e) => {
                // #9026: the whole chain, not the outermost context.
                eprintln!("{}", memory_list_failed_warning(memory_socket, &e));
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

    /// Uses a socket path nothing binds rather than any resolved daemon path —
    /// the point of the test is only "does not panic", exercising the fail-open
    /// `None` path. Needs no daemon, so it runs by default.
    #[tokio::test]
    async fn live_drawer_fetch() {
        let drawers = fetch_recent_palace_drawers(
            std::path::Path::new("/nonexistent/trusty-memory.sock"),
            "test-palace",
            5,
            None,
        )
        .await;
        // Just verify it returns without panicking.
        let _ = drawers;
    }

    /// A one-connection stub daemon that answers `memory_list` with `code`.
    fn spawn_refusing_daemon(dir: &std::path::Path, code: i64) -> std::path::PathBuf {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let sock = dir.join("sockets").join("memory.sock");
        let listener = crate::uds::bind_hardened(&sock).expect("bind stub socket");
        let reply = format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": code, "message": "palace x"}})
        );
        tokio::spawn(async move {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let _ = conn.write_all(reply.as_bytes()).await;
            let _ = conn.flush().await;
        });
        sock
    }

    /// Why (#9026): the catch-up digest logged "could not reach trusty-memory"
    /// on every run for a project whose palace was never created, and rendered
    /// the section as unreachable. The daemon answered; the palace is absent.
    /// What: a not-found refusal is `Some(empty)`. Fail-Open Check: any other
    /// refusal (`-32603`) still reports the daemon unreachable (`None`).
    #[tokio::test]
    async fn an_absent_palace_is_reached_and_empty_not_unreachable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let absent = spawn_refusing_daemon(tmp.path(), crate::memory_rpc::CODE_NOT_FOUND);
        let drawers = fetch_recent_palace_drawers(&absent, "never-created", 5, None).await;
        assert!(
            drawers.as_ref().is_some_and(Vec::is_empty),
            "an absent palace is reached and empty: {drawers:?}"
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let failing = spawn_refusing_daemon(tmp.path(), -32603);
        let drawers = fetch_recent_palace_drawers(&failing, "broken", 5, None).await;
        assert!(
            drawers.is_none(),
            "an internal error is not an empty palace"
        );
    }

    /// #9026: the warning names the root cause, here the dial refusal of a
    /// socket file that does not exist, not only the outer "call memory_list"
    /// context; and it no longer claims the daemon could not be reached.
    #[tokio::test]
    async fn the_unreachable_warning_carries_the_whole_cause_chain_9026() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dead = tmp.path().join("absent.sock");
        let e = crate::memory_rpc::call_memory_tool_at_with_timeout(
            &dead,
            "memory_list",
            json!({ "palace": "p" }),
            std::time::Duration::from_secs(2),
        )
        .await
        .expect_err("nothing serves the socket");
        let root = e.root_cause().to_string();
        let line = memory_list_failed_warning(&dead, &e);
        assert!(
            line.contains(&root),
            "{line:?} lacks the root cause {root:?}"
        );
        assert!(line.contains("call memory_list"), "{line:?}");
        assert!(!line.contains("could not reach"), "{line:?}");
    }
}
