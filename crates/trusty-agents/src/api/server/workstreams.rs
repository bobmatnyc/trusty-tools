//! `GET /api/workstreams` + `GET /api/workstreams/:name/history` — resumable
//! workstreams sourced from trusty-memory's `ws:<name>` tag convention
//! (#3819, epic #3052).
//!
//! Why: Bob's directive replaces the sidebar's static `PROJECTS` (today: one
//! entry, `CTRL`) with `WORKSTREAMS` — workstreams the user can resume,
//! backed by TAGGED MEMORY HISTORY, the same convention the `tm` harness
//! already uses (PR #3731 / commit `f29f7332`,
//! `docs/specs/DOC-53-workstream-claim-drawer-convention.md`). DOC-53 §4.1
//! stamps every drawer written by a workstream-identified session with a bare
//! `ws:<name>` tag (alongside hand-written `WS-CLAIM` drawers, which carry the
//! same tag by convention, §3.1) into "the project's own palace" — the same
//! trusty-memory palace PM sessions write to, cwd-resolved via
//! `trusty_memory::project_root::project_slug_at_readonly` (the read-only,
//! no-side-effect variant — this is a GET handler, it must never lazily
//! write a `.trusty-tools/trusty-memory.yaml` pin file as a side effect of
//! being polled).
//! What: [`list_workstreams_at`] fetches the palace's drawers (a single
//! request, no `tag` filter — trusty-memory's `ListDrawersQuery.tag` is an
//! EXACT match, so filtering by any tag *starting with* `ws:` has to happen
//! client-side; see the module-level design note in the PR body for why this
//! is the simplest robust source of truth for "list workstreams + their
//! recent history" without inventing a new trusty-memory query capability),
//! then [`group_by_workstream`] (pure, unit-tested) buckets them by their
//! `ws:<name>` tag and keeps the most-recent-first summary per name.
//! [`workstream_history_at`] does the same fetch narrowed to one workstream's
//! `ws:<name>` tag (an exact match — trusty-memory supports this natively)
//! for the "resume" flow's context-injection payload.
//! Fails gracefully, not with an error, when the daemon is unreachable or the
//! project has no palace yet (empty list) — this is a sidebar convenience
//! panel, not a critical path; a down trusty-memory daemon must not break the
//! rest of the GUI (safe-defaults convention, `BASE-ENGINEER`).
//! Test: `group_by_workstream_*` (pure grouping logic), `list_workstreams_at_*`
//! / `workstream_history_at_*` (mock-daemon round trip, mirroring
//! `memory::trusty_client::tests`' `TcpListener` + `axum::serve` pattern).

use std::path::Path;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path as AxumPath, Query, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::state::AppState;
use crate::memory::trusty_client::default_trusty_url;

/// Health-probe timeout — short so a down daemon never noticeably stalls the
/// sidebar. Mirrors `memory::trusty_client::HEALTH_TIMEOUT`.
const HEALTH_TIMEOUT: Duration = Duration::from_millis(500);
/// Per-call timeout for the drawer-listing request itself.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
/// Upper bound on how many of the palace's most-recent drawers we scan for
/// `ws:` tags in one request. Bounds worst-case response size/latency; a
/// palace with more workstream-relevant activity than this in its most
/// recent window will only surface the newest `WORKSTREAM_SCAN_LIMIT`
/// drawers' worth — an accepted approximation for a sidebar convenience
/// panel, not a completeness guarantee.
const WORKSTREAM_SCAN_LIMIT: usize = 500;
/// How many history items `workstream_history_at` returns for one workstream
/// by default — the resume flow's context-injection payload should stay
/// compact for a chat-context banner, not dump an unbounded log.
const DEFAULT_HISTORY_LIMIT: usize = 20;

/// DOC-53 §4.1/§4.2's bare, visible tag prefix — `WORKSTREAM_TAG_PREFIX` in
/// `trusty_memory::attribution`. Re-declared here (a plain `&str`, not a
/// dependency on trusty-memory's internal module) because that constant is
/// not part of trusty-memory's public re-exports this crate already depends
/// on; keeping the string in both places is cheap and the convention is a
/// stable, documented wire contract (DOC-53), not an implementation detail
/// likely to drift silently.
const WORKSTREAM_TAG_PREFIX: &str = "ws:";
/// DOC-53 §3.1's fixed claim-drawer marker tag.
const CLAIM_TAG: &str = "ws-claim";
/// DOC-53 §3.1's per-claim area tag prefix.
const AREA_TAG_PREFIX: &str = "area:";

/// One row as returned by trusty-memory's `GET /api/v1/palaces/{id}/drawers`
/// — a subset of `trusty_common::memory_core::palace::Drawer`'s fields (only
/// what grouping/summary needs).
#[derive(Debug, Clone, Deserialize)]
struct DrawerRow {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    created_at: DateTime<Utc>,
}

/// One workstream summary row — the `GET /api/workstreams` response shape.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct WorkstreamSummary {
    pub(super) name: String,
    pub(super) last_activity: DateTime<Utc>,
    /// The most recent drawer's content for this workstream, truncated to a
    /// glanceable length — mirrors trusty-memory's own `snippet` convention
    /// (`service::core::drawer_snippet`) rather than reinventing truncation.
    pub(super) summary: String,
    /// True when at least one of this workstream's drawers also carries the
    /// `ws-claim` marker tag (DOC-53 §3.1) — i.e. an explicit "I'm working
    /// here" claim exists, not just incidental attributed activity.
    pub(super) has_open_claim: bool,
    pub(super) areas: Vec<String>,
    pub(super) item_count: usize,
}

/// One item in a workstream's resume history — the `GET
/// /api/workstreams/:name/history` response shape.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct HistoryItem {
    pub(super) content: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) tags: Vec<String>,
}

/// Bound content to a glanceable length for the summary card — full content
/// remains available via `history`. Whitespace-collapsed so a multi-line
/// claim-drawer body renders as one tidy line.
fn snippet(content: &str, max_chars: usize) -> String {
    let collapsed: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

/// Extract the `ws:<name>` value from a drawer's tags, if present. When a
/// drawer somehow carries more than one (should not happen per DOC-53 §3.1's
/// dedup rule, but a hand-authored drawer could), the first wins —
/// deterministic given `tags`' on-disk order, not meaningfully ambiguous.
fn workstream_name_of(tags: &[String]) -> Option<&str> {
    tags.iter()
        .find_map(|t| t.strip_prefix(WORKSTREAM_TAG_PREFIX))
        .filter(|s| !s.is_empty())
}

/// Pure grouping core: buckets `rows` by their `ws:<name>` tag, sorted
/// most-recently-active workstream first. `rows` is assumed already sorted
/// newest-first (trusty-memory's `sort=created_desc`) — the first row seen
/// per name becomes that workstream's summary.
///
/// Test: `group_by_workstream_empty`, `group_by_workstream_single_claim`,
/// `group_by_workstream_groups_multiple_drawers_same_name`,
/// `group_by_workstream_ignores_untagged_drawers`,
/// `group_by_workstream_sorts_by_last_activity_desc`,
/// `group_by_workstream_collects_areas`.
fn group_by_workstream(rows: &[DrawerRow]) -> Vec<WorkstreamSummary> {
    let mut by_name: std::collections::HashMap<&str, WorkstreamSummary> =
        std::collections::HashMap::new();
    let mut order: Vec<&str> = Vec::new();

    for row in rows {
        let Some(name) = workstream_name_of(&row.tags) else {
            continue;
        };
        let is_claim = row.tags.iter().any(|t| t == CLAIM_TAG);
        let areas: Vec<String> = row
            .tags
            .iter()
            .filter_map(|t| t.strip_prefix(AREA_TAG_PREFIX))
            .map(str::to_string)
            .collect();

        match by_name.get_mut(name) {
            Some(existing) => {
                existing.item_count += 1;
                existing.has_open_claim = existing.has_open_claim || is_claim;
                if row.created_at > existing.last_activity {
                    existing.last_activity = row.created_at;
                    existing.summary = snippet(&row.content, 140);
                }
                for area in areas {
                    if !existing.areas.contains(&area) {
                        existing.areas.push(area);
                    }
                }
            }
            None => {
                order.push(name);
                by_name.insert(
                    name,
                    WorkstreamSummary {
                        name: name.to_string(),
                        last_activity: row.created_at,
                        summary: snippet(&row.content, 140),
                        has_open_claim: is_claim,
                        areas,
                        item_count: 1,
                    },
                );
            }
        }
    }

    let mut out: Vec<WorkstreamSummary> = order
        .into_iter()
        .filter_map(|name| by_name.remove(name))
        .collect();
    out.sort_by_key(|w| std::cmp::Reverse(w.last_activity));
    out
}

/// Fetch a palace's drawers from a trusty-memory daemon at `base_url`.
/// Returns an empty vec (never an error) when the daemon is unreachable, the
/// palace doesn't exist yet (404), or the response fails to parse — a down
/// or not-yet-provisioned trusty-memory daemon must never break the sidebar.
async fn fetch_drawers(base_url: &str, palace_id: &str, limit: usize) -> Vec<DrawerRow> {
    let client = match reqwest::Client::builder()
        .connect_timeout(HEALTH_TIMEOUT)
        .timeout(CALL_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let health_url = format!("{}/health", base_url.trim_end_matches('/'));
    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {}
        _ => return Vec::new(),
    }

    let url = format!(
        "{}/api/v1/palaces/{palace_id}/drawers",
        base_url.trim_end_matches('/')
    );
    let resp = match client
        .get(&url)
        .query(&[("sort", "created_desc"), ("limit", &limit.to_string())])
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if !resp.status().is_success() {
        // 404 (palace not yet created) and any other non-2xx are both "no
        // workstream data yet" from this endpoint's point of view.
        return Vec::new();
    }
    resp.json::<Vec<DrawerRow>>().await.unwrap_or_default()
}

/// Fetch a palace's drawers narrowed to an EXACT `tag` match — trusty-memory
/// supports this natively (`ListDrawersQuery.tag`), unlike the `ws:` PREFIX
/// scan `fetch_drawers` does for the summary listing.
async fn fetch_drawers_by_tag(
    base_url: &str,
    palace_id: &str,
    tag: &str,
    limit: usize,
) -> Vec<DrawerRow> {
    let client = match reqwest::Client::builder()
        .connect_timeout(HEALTH_TIMEOUT)
        .timeout(CALL_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let url = format!(
        "{}/api/v1/palaces/{palace_id}/drawers",
        base_url.trim_end_matches('/')
    );
    let resp = match client
        .get(&url)
        .query(&[
            ("tag", tag),
            ("sort", "created_desc"),
            ("limit", &limit.to_string()),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    resp.json::<Vec<DrawerRow>>().await.unwrap_or_default()
}

/// Resolve the current project's trusty-memory palace id from `cwd` — the
/// read-only variant (`project_slug_at_readonly`) so a poll of this endpoint
/// never has the side effect of lazily writing a pin file (that side effect
/// is reserved for interactive `trusty-memory` commands, not a GUI GET
/// handler polled on an interval).
fn palace_id_for(cwd: &Path) -> Option<String> {
    trusty_memory::project_root::project_slug_at_readonly(cwd)
}

/// Core listing logic against an explicit cwd + trusty-memory base URL.
///
/// Why: Extracted so tests can point at a mock daemon / tempdir project root
/// without depending on the process cwd or a live daemon (mirrors
/// `patch_agent_at`'s testable-core split).
/// What: Resolves the palace id from `cwd`; returns `[]` immediately when no
/// project root is found (e.g. running outside any git-marked project).
/// Otherwise fetches up to `WORKSTREAM_SCAN_LIMIT` of the palace's most
/// recent drawers and groups them.
/// Test: `list_workstreams_at_no_project_root_is_empty`,
/// `list_workstreams_at_against_mock_daemon`.
pub(super) async fn list_workstreams_at(cwd: &Path, base_url: &str) -> Vec<WorkstreamSummary> {
    let Some(palace_id) = palace_id_for(cwd) else {
        return Vec::new();
    };
    let rows = fetch_drawers(base_url, &palace_id, WORKSTREAM_SCAN_LIMIT).await;
    group_by_workstream(&rows)
}

/// Core per-workstream history logic — see [`list_workstreams_at`]'s doc for
/// the testable-core rationale.
pub(super) async fn workstream_history_at(
    cwd: &Path,
    base_url: &str,
    name: &str,
    limit: usize,
) -> Vec<HistoryItem> {
    let Some(palace_id) = palace_id_for(cwd) else {
        return Vec::new();
    };
    let tag = format!("{WORKSTREAM_TAG_PREFIX}{name}");
    fetch_drawers_by_tag(base_url, &palace_id, &tag, limit)
        .await
        .into_iter()
        .map(|row| HistoryItem {
            content: row.content,
            created_at: row.created_at,
            tags: row.tags,
        })
        .collect()
}

/// `GET /api/workstreams` — HTTP entry point.
pub(super) async fn list_workstreams_route(
    State(_state): State<AppState>,
) -> Json<Vec<WorkstreamSummary>> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    Json(list_workstreams_at(&cwd, &default_trusty_url()).await)
}

#[derive(Debug, Deserialize)]
pub(super) struct HistoryQuery {
    limit: Option<usize>,
}

/// `GET /api/workstreams/:name/history?limit=<n>` — HTTP entry point.
pub(super) async fn workstream_history_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HistoryQuery>,
) -> Json<Vec<HistoryItem>> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let limit = q.limit.unwrap_or(DEFAULT_HISTORY_LIMIT);
    Json(workstream_history_at(&cwd, &default_trusty_url(), &name, limit).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(content: &str, tags: &[&str], created_at: DateTime<Utc>) -> DrawerRow {
        DrawerRow {
            content: content.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            created_at,
        }
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid timestamp")
    }

    #[test]
    fn group_by_workstream_empty() {
        assert_eq!(group_by_workstream(&[]), Vec::new());
    }

    #[test]
    fn group_by_workstream_ignores_untagged_drawers() {
        let rows = vec![row("plain note", &["misc"], ts(0))];
        assert_eq!(group_by_workstream(&rows), Vec::new());
    }

    #[test]
    fn group_by_workstream_single_claim() {
        let rows = vec![row(
            "WS-CLAIM feat-x: does a thing",
            &["ws-claim", "ws:feat-x", "area:health-endpoint"],
            ts(0),
        )];
        let out = group_by_workstream(&rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "feat-x");
        assert!(out[0].has_open_claim);
        assert_eq!(out[0].areas, vec!["health-endpoint".to_string()]);
        assert_eq!(out[0].item_count, 1);
    }

    #[test]
    fn group_by_workstream_groups_multiple_drawers_same_name() {
        let rows = vec![
            row("newest", &["ws:feat-x"], ts(20)),
            row("older", &["ws:feat-x"], ts(10)),
        ];
        let out = group_by_workstream(&rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item_count, 2);
        assert_eq!(out[0].summary, "newest");
        assert_eq!(out[0].last_activity, ts(20));
    }

    #[test]
    fn group_by_workstream_sorts_by_last_activity_desc() {
        let rows = vec![
            row("a", &["ws:older-stream"], ts(5)),
            row("b", &["ws:newer-stream"], ts(50)),
        ];
        let out = group_by_workstream(&rows);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "newer-stream");
        assert_eq!(out[1].name, "older-stream");
    }

    #[test]
    fn group_by_workstream_collects_areas() {
        let rows = vec![
            row("a", &["ws-claim", "ws:feat-x", "area:one"], ts(0)),
            row("b", &["ws:feat-x", "area:two"], ts(1)),
        ];
        let out = group_by_workstream(&rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].areas.len(), 2);
        assert!(out[0].areas.contains(&"one".to_string()));
        assert!(out[0].areas.contains(&"two".to_string()));
    }

    #[test]
    fn snippet_short_content_passes_through() {
        assert_eq!(snippet("hello world", 140), "hello world");
    }

    #[test]
    fn snippet_truncates_long_content() {
        let long = "x".repeat(200);
        let out = snippet(&long, 10);
        assert_eq!(out.chars().count(), 11); // 10 chars + the ellipsis char
        assert!(out.ends_with('…'));
    }

    #[tokio::test]
    async fn list_workstreams_at_no_project_root_is_empty() {
        // A tempdir with no git/project marker resolves no palace id, so the
        // function returns empty without ever making a network call.
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = list_workstreams_at(tmp.path(), "http://127.0.0.1:1").await;
        assert_eq!(out, Vec::new());
    }

    #[tokio::test]
    async fn list_workstreams_at_unreachable_daemon_is_empty_not_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        // Port 1 is a reserved, never-listening port — the health probe
        // fails fast and the function degrades to an empty list.
        let out = list_workstreams_at(tmp.path(), "http://127.0.0.1:1").await;
        assert_eq!(out, Vec::new());
    }
}
