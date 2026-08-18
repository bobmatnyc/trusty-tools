//! `GET /api/workstreams` + `GET /api/workstreams/:name/history` — resumable
//! workstreams sourced from trusty-memory's `ws:<name>` tag convention
//! (#3819, epic #3052), plus the shared read/write primitives the
//! filterable-context classification path (DOC-54 §9.6,
//! `ctrl::pm_task::dispatch::classification`) reuses to persist and read
//! back workstream-tagged chat turns.
//!
//! Why: Bob's directive replaces the sidebar's static `PROJECTS` (today: one
//! entry, `CTRL`) with `WORKSTREAMS` — workstreams the user can resume,
//! backed by TAGGED MEMORY HISTORY, the convention `tm` already uses (PR
//! #3731, `docs/specs/DOC-53-workstream-claim-drawer-convention.md`). DOC-53
//! §4.1 stamps every drawer written by a workstream-identified session with
//! a bare `ws:<name>` tag into the project's own trusty-memory palace,
//! cwd-resolved via the READ-ONLY `project_slug_at_readonly` for every GET
//! handler here (never lazily writes the project's pin file on a poll).
//! DOC-54 §9.6 reuses the SAME tag convention for classified chat turns —
//! a persona turn classified as `<label>` is persisted as a drawer tagged
//! `ws:<label>`, so it appears in this same sidebar listing for free. A
//! sibling namespace (`ws-summary:<label>`, [`WORKSTREAM_SUMMARY_TAG_PREFIX`])
//! holds the cached per-workstream summary DOC-54 §9.6.2 describes;
//! [`drawers_by_tag_at`] is the one generic exact-tag read primitive both
//! the turn-history and summary-cache lookups share. [`create_tagged_drawer_at`]
//! is the write-path counterpart the classification path uses to persist
//! both — unlike every GET handler it resolves via the WRITING
//! `project_slug_at` (may lazily create the pin file), since it only runs
//! on a genuine chat turn, never a poll.
//! What: [`list_workstreams_at`] fetches the palace's drawers (no `tag`
//! filter — trusty-memory's tag filter is an EXACT match, so a `ws:` PREFIX
//! scan happens client-side), then [`group_by_workstream`] (pure,
//! unit-tested) buckets them by `ws:<name>`, most-recent-first.
//! [`workstream_history_at`] narrows to one workstream's exact tag for the
//! resume flow's context-injection payload.
//! Fails gracefully (empty list, not an error) when the daemon is
//! unreachable or the project has no palace yet — a down trusty-memory
//! daemon must never break the GUI (safe-defaults, `BASE-ENGINEER`). The
//! write path is the one exception: a failed write returns `Err` so the
//! caller can log it rather than silently losing a classified turn.
//! Test: `group_by_workstream_*`, `list_workstreams_at_*` /
//! `workstream_history_at_*`, `create_tagged_drawer_at_*` /
//! `drawers_by_tag_at_*` — see `tests.rs`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
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
pub(crate) const WORKSTREAM_TAG_PREFIX: &str = "ws:";
/// DOC-53 §3.1's fixed claim-drawer marker tag.
const CLAIM_TAG: &str = "ws-claim";
/// DOC-53 §3.1's per-claim area tag prefix.
const AREA_TAG_PREFIX: &str = "area:";
/// DOC-54 §9.6.2's cached per-workstream summary tag prefix — sibling
/// namespace to [`WORKSTREAM_TAG_PREFIX`], so a summary drawer never gets
/// mistaken for a raw turn by [`group_by_workstream`] (a `ws-summary:`
/// prefix does not match `ws:`'s `strip_prefix`, so summary drawers are
/// invisible to the sidebar listing by construction — they are cache
/// entries, not user-visible activity).
pub(crate) const WORKSTREAM_SUMMARY_TAG_PREFIX: &str = "ws-summary:";

/// Render the `ws:<name>` tag for a workstream's per-turn drawers.
pub(crate) fn workstream_tag(name: &str) -> String {
    format!("{WORKSTREAM_TAG_PREFIX}{name}")
}

/// Render the `ws-summary:<name>` tag for a workstream's cached summary
/// drawer (DOC-54 §9.6.2).
pub(crate) fn workstream_summary_tag(name: &str) -> String {
    format!("{WORKSTREAM_SUMMARY_TAG_PREFIX}{name}")
}

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
pub(crate) struct WorkstreamSummary {
    pub(crate) name: String,
    pub(crate) last_activity: DateTime<Utc>,
    /// The most recent drawer's content for this workstream, truncated to a
    /// glanceable length — mirrors trusty-memory's own `snippet` convention
    /// (`service::core::drawer_snippet`) rather than reinventing truncation.
    pub(crate) summary: String,
    /// True when at least one of this workstream's drawers also carries the
    /// `ws-claim` marker tag (DOC-53 §3.1) — i.e. an explicit "I'm working
    /// here" claim exists, not just incidental attributed activity.
    pub(crate) has_open_claim: bool,
    pub(crate) areas: Vec<String>,
    pub(crate) item_count: usize,
}

/// One item in a workstream's resume history — the `GET
/// /api/workstreams/:name/history` response shape. Also the shape
/// [`drawers_by_tag_at`] returns for the classification path's turn-history
/// and summary-cache reads (DOC-54 §9.6).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct HistoryItem {
    pub(crate) content: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) tags: Vec<String>,
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

/// Resolve the current project's trusty-memory palace id from `cwd`.
///
/// Why: this used `project_slug_at_readonly`, which is pin-then-BASENAME and
/// consults no git identity — so an unpinned repo listed workstreams from
/// `<dirname>` while the daemon wrote them to `<owner>-<repo>` and the endpoint
/// showed an empty list (#5811). The shared entry point answers the same way as
/// every other caller, and is still side-effect-free, so a polled GET never
/// writes a pin file.
/// What: returns `None` when no level yields an id or when a pin exists but
/// cannot be trusted — an empty workstream list is the safe answer for a poll.
pub(crate) fn palace_id_for(cwd: &Path) -> Option<String> {
    trusty_common::palace_resolve::resolve_palace(cwd)
        .inspect_err(|e| tracing::debug!("no palace for {}: {e}", cwd.display()))
        .ok()
        .map(|r| r.id)
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
/// `list_workstreams_at_unreachable_daemon_is_empty_not_error`.
pub(crate) async fn list_workstreams_at(cwd: &Path, base_url: &str) -> Vec<WorkstreamSummary> {
    let Some(palace_id) = palace_id_for(cwd) else {
        return Vec::new();
    };
    let rows = fetch_drawers(base_url, &palace_id, WORKSTREAM_SCAN_LIMIT).await;
    group_by_workstream(&rows)
}

/// Core per-workstream history logic — see [`list_workstreams_at`]'s doc for
/// the testable-core rationale.
pub(crate) async fn workstream_history_at(
    cwd: &Path,
    base_url: &str,
    name: &str,
    limit: usize,
) -> Vec<HistoryItem> {
    drawers_by_tag_at(cwd, base_url, &workstream_tag(name), limit).await
}

/// Just the distinct workstream label set, most-recently-active first — the
/// closed vocabulary the in-band classification block (DOC-54 §9.6.1,
/// `ctrl::pm_task::dispatch::classification::classification_block`) presents
/// to the model.
/// Test: `list_workstream_labels_at_against_mock_daemon`.
pub(crate) async fn list_workstream_labels_at(cwd: &Path, base_url: &str) -> Vec<String> {
    list_workstreams_at(cwd, base_url)
        .await
        .into_iter()
        .map(|w| w.name)
        .collect()
}

/// Fetch drawers matching an EXACT tag, newest first, mapped to
/// [`HistoryItem`] — the one generic read primitive the classification path
/// shares between turn-history lookups (`tag = "ws:<label>"`) and
/// summary-cache lookups (`tag = "ws-summary:<label>"`).
/// What: Resolves the palace id from `cwd` (read-only — never writes a pin
/// file); returns `[]` when no project root is found or the daemon is
/// unreachable, mirroring [`list_workstreams_at`]'s safe-default posture.
/// Test: `drawers_by_tag_at_no_project_root_is_empty`,
/// `create_tagged_drawer_at_and_drawers_by_tag_at_round_trip`.
pub(crate) async fn drawers_by_tag_at(
    cwd: &Path,
    base_url: &str,
    tag: &str,
    limit: usize,
) -> Vec<HistoryItem> {
    let Some(palace_id) = palace_id_for(cwd) else {
        return Vec::new();
    };
    fetch_drawers_by_tag(base_url, &palace_id, tag, limit)
        .await
        .into_iter()
        .map(|row| HistoryItem {
            content: row.content,
            created_at: row.created_at,
            tags: row.tags,
        })
        .collect()
}

/// Build a short-timeout `reqwest::Client` matching the rest of this
/// module's read paths — shared by the write path below so palace-create +
/// drawer-create use the same connect/call timeouts as every read.
fn build_http_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(HEALTH_TIMEOUT)
        .timeout(CALL_TIMEOUT)
        .build()
        .ok()
}

/// Write a `content` drawer carrying `tags` into the current project's
/// palace — the write-path counterpart of [`drawers_by_tag_at`], used by the
/// classification path (DOC-54 §9.6) to persist a classified turn
/// (`tags = ["ws:<label>"]`) or a refreshed per-workstream summary
/// (`tags = ["ws-summary:<label>"]`).
///
/// Why: this is a WRITE path, so resolving it differently from the readers is
/// how drawers end up somewhere nobody looks. It used the writing
/// `project_slug_at` (pin-then-basename, no git identity, and a lazy pin-file
/// write as a side effect); it now uses the same entry point as every other
/// caller (#5811). Returns `Err` on failure instead of silently dropping the
/// turn — callers log it and carry on.
/// What: idempotently ensures the palace exists (`force: true`, matching
/// `TrustyMemoryClient::ensure_palace`), then posts the drawer with
/// `force: true` (bypasses trusty-memory's signal/noise gate for
/// short/structured content, same rationale as `CreateDrawerReq::force`).
/// Test: `create_tagged_drawer_at_and_drawers_by_tag_at_round_trip`,
/// `create_tagged_drawer_at_without_a_project_root_still_resolves_a_palace`,
/// `create_tagged_drawer_at_malformed_pin_errs`.
pub(crate) async fn create_tagged_drawer_at(
    cwd: &Path,
    base_url: &str,
    content: &str,
    tags: Vec<String>,
) -> Result<()> {
    let palace_id = trusty_common::palace_resolve::resolve_palace(cwd)
        .with_context(|| format!("resolve palace for {}", cwd.display()))?
        .id;
    let client = build_http_client().context("building trusty-memory HTTP client")?;

    let create_url = format!("{}/api/v1/palaces", base_url.trim_end_matches('/'));
    client
        .post(&create_url)
        .json(&serde_json::json!({
            "name": palace_id,
            "description": "Auto-created by trusty-agents workstream classification",
            "force": true,
        }))
        .send()
        .await
        .with_context(|| format!("POST {create_url}"))?;

    let drawer_url = format!(
        "{}/api/v1/palaces/{palace_id}/drawers",
        base_url.trim_end_matches('/')
    );
    let resp = client
        .post(&drawer_url)
        .json(&serde_json::json!({
            "content": content,
            "tags": tags,
            "force": true,
        }))
        .send()
        .await
        .with_context(|| format!("POST {drawer_url}"))?;
    if !resp.status().is_success() {
        bail!(
            "trusty-memory create_drawer failed: HTTP {} ({})",
            resp.status(),
            drawer_url
        );
    }
    Ok(())
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
mod tests;
