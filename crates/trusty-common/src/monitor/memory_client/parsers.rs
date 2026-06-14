//! JSON-to-domain projection functions for the trusty-memory client.
//!
//! Why: isolating all `serde_json::Value` parsing logic here keeps `client.rs`
//! free of projection noise and makes every parser independently unit-testable
//! without a live daemon.
//! What: free functions that accept raw `serde_json::Value` payloads from the
//! memory daemon's REST endpoints and return typed domain values.
//! Test: each parser has a dedicated test in `tests.rs`.

use crate::monitor::dashboard::PalaceRow;

use super::types::{
    DRAWER_SNIPPET_FALLBACK_MAX, DrawerInfo, DreamStats, MemoryDetail, MemoryEvent,
    NO_CREATOR_LABEL, RecallHit,
};

use super::types::PalaceWire;

/// Project a `/api/v1/recall` JSON payload into [`RecallHit`]s.
///
/// Why: the recall endpoint returns a bare array of result objects;
/// centralising the projection keeps the client testable and resilient to
/// absent optional fields.
/// What: accepts a JSON array, and for each entry takes `palace_id`, the first
/// line of `content`, and `score`. Any other shape yields an empty list.
/// Test: `parse_recall_hits_projects_fields`.
pub fn parse_recall_hits(raw: &serde_json::Value) -> Vec<RecallHit> {
    let serde_json::Value::Array(items) = raw else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let palace_id = item
                .get("palace_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let snippet = item
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let score = item.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            RecallHit {
                palace_id,
                snippet,
                score,
            }
        })
        .collect()
}

/// Project a `/api/v1/dream/run` JSON payload into a [`DreamStats`].
///
/// Why: the dream endpoint returns an object with several aggregate counters;
/// the TUI surfaces three of them.
/// What: reads `merged`, `pruned`, and `compacted`, defaulting absent fields
/// to zero.
/// Test: `parse_dream_stats_reads_counts`.
pub fn parse_dream_stats(raw: &serde_json::Value) -> DreamStats {
    let u64_of = |key: &str| raw.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    DreamStats {
        merged: u64_of("merged"),
        pruned: u64_of("pruned"),
        compacted: u64_of("compacted"),
    }
}

/// Parse one `/sse` `data:` JSON object into a [`MemoryEvent`].
///
/// Why: the daemon serializes `DaemonEvent` as `{"type": "...", ...fields}`;
/// the TUI needs the four user-facing variants and ignores housekeeping
/// frames, so this folds the wire shape into [`MemoryEvent`].
/// What: dispatches on the `type` tag — `palace_created`, `drawer_added`,
/// `drawer_deleted`, `dream_completed`. Returns `None` for `connected`, `lag`,
/// `status_changed`, or any unrecognised tag.
/// Test: `parse_memory_event_maps_type_tag`.
pub fn parse_memory_event(value: &serde_json::Value) -> Option<MemoryEvent> {
    let tag = value.get("type").and_then(|v| v.as_str())?;
    let str_of = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let u64_of = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    match tag {
        "palace_created" => Some(MemoryEvent::PalaceCreated {
            name: str_of("name"),
        }),
        "drawer_added" => Some(MemoryEvent::DrawerAdded {
            palace_id: str_of("palace_id"),
            drawer_count: u64_of("drawer_count"),
            content_preview: str_of("content_preview"),
        }),
        "drawer_deleted" => Some(MemoryEvent::DrawerDeleted {
            palace_id: str_of("palace_id"),
            drawer_count: u64_of("drawer_count"),
        }),
        "dream_completed" => Some(MemoryEvent::DreamCompleted {
            merged: u64_of("merged"),
            pruned: u64_of("pruned"),
            compacted: u64_of("compacted"),
        }),
        _ => None,
    }
}

/// Project a palace-list JSON payload into [`PalaceRow`]s.
///
/// Why: the trusty-memory palace endpoint has shipped both a bare-array shape
/// and an object-wrapped shape across versions; centralising the parsing keeps
/// the client resilient to either and makes it unit-testable without a daemon.
/// What: accepts a JSON array of palace objects, or an object carrying a
/// `palaces` array, and returns the projected rows; any other shape yields an
/// empty list.
/// Test: `palace_list_accepts_array_and_object_shapes`.
pub fn parse_palaces(raw: &serde_json::Value) -> Vec<PalaceRow> {
    let array = match raw {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(obj) => match obj.get("palaces") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    array
        .into_iter()
        .filter_map(|v| serde_json::from_value::<PalaceWire>(v).ok())
        .map(|p| PalaceRow {
            id: p.id,
            name: p.name,
            vector_count: p.vector_count,
            drawer_count: p.drawer_count,
            last_write_at: p.last_write_at,
            description: p.description,
            kg_triple_count: p.kg_triple_count,
            node_count: p.node_count,
            edge_count: p.edge_count,
            community_count: p.community_count,
            is_compacting: p.is_compacting,
        })
        .collect()
}

/// Project a `…/drawers` JSON payload into [`MemoryDetail`]s (issue #215).
///
/// Why: the detail modal needs the full untruncated `content` field, which
/// the row-oriented [`parse_drawers`] projection deliberately omits. A
/// dedicated projection keeps both call sites honest about what they
/// consume.
/// What: accepts the same shapes as [`parse_drawers`] — a bare array, or an
/// object with a `drawers` field — and pulls `id`, `content`, `tags`, and
/// `created_at` for each entry. Absent or malformed fields fall through to
/// safe defaults so a single corrupt row does not drop the whole page.
/// Test: `parse_memory_details_projects_full_content`.
pub fn parse_memory_details(raw: &serde_json::Value) -> Vec<MemoryDetail> {
    let array: Vec<serde_json::Value> = match raw {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(obj) => match obj.get("drawers") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    array
        .into_iter()
        .map(|item| {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let content = item
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let tags: Vec<String> = item
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let created_at = item
                .get("created_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));
            MemoryDetail {
                id,
                content,
                tags,
                created_at,
            }
        })
        .collect()
}

/// Project a `…/drawers` JSON payload into [`DrawerInfo`]s.
///
/// Why: the endpoint may return either a bare JSON array (the
/// trusty-memory daemon's current shape) or — defensively — an object with a
/// `drawers` array. The projection tolerates both, and absent / malformed
/// fields fall through to safe defaults so a single corrupt row does not
/// drop the whole page.
/// What: accepts a JSON array (or object with `drawers`); for each entry
/// reads `id` (UUID string), `created_at` (RFC 3339), and `tags`, then
/// resolves the creator label via [`creator_label`]. The optional
/// `snippet` field is preferred; when absent the client falls back to
/// truncating `content` to [`DRAWER_SNIPPET_FALLBACK_MAX`] chars so older
/// daemons still surface a usable snippet (issue #202).
/// Test: `parse_drawers_projects_fields`.
pub fn parse_drawers(raw: &serde_json::Value) -> Vec<DrawerInfo> {
    let array: Vec<serde_json::Value> = match raw {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(obj) => match obj.get("drawers") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    array
        .into_iter()
        .map(|item| {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let created_at = item
                .get("created_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));
            let tags: Vec<String> = item
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let creator = creator_label(&tags);
            // Issue #202: prefer the daemon-supplied `snippet` field
            // (already truncated and whitespace-collapsed). Fall back
            // to a client-side truncation of `content` for daemons that
            // predate the field. `null` / empty / whitespace-only
            // values yield `None` so the renderer can omit the column.
            let snippet = item
                .get("snippet")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    item.get("content")
                        .and_then(|v| v.as_str())
                        .map(client_snippet)
                        .filter(|s| !s.is_empty())
                });
            DrawerInfo {
                id,
                created_at,
                creator,
                tags,
                snippet,
            }
        })
        .collect()
}

/// Client-side fallback snippet builder for daemons that predate the
/// `snippet` wire field (issue #202).
///
/// Why: keep the activity panel useful across daemon versions; the
/// projection should still surface a glanceable summary even when the
/// daemon returns only the raw `content`.
/// What: whitespace-collapses, trims, and truncates to
/// [`DRAWER_SNIPPET_FALLBACK_MAX`] with a trailing `…` when cut. Empty
/// / whitespace-only inputs yield the empty string so the caller can
/// drop the snippet via `filter(|s| !s.is_empty())`.
/// Test: covered by `parse_drawers_projects_fields` via the fallback path.
fn client_snippet(content: &str) -> String {
    let normalised: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= DRAWER_SNIPPET_FALLBACK_MAX {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(DRAWER_SNIPPET_FALLBACK_MAX.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

/// Pick the first recognised creator tag from a drawer's tag list.
///
/// Why: trusty-memory drawers carry their writer's identity in tag form —
/// `msg:from=<peer>` for cross-instance messages, `creator:client=<name>` /
/// `creator:source=<src>` for HTTP / MCP writers, and `tag:creator:…` for the
/// legacy CTO/MPM convention. The TUI surfaces whichever match comes first
/// so the operator can trace authorship at a glance.
/// What: scans `tags` in order looking for a prefix in
/// (`msg:from=`, `tag:creator:`, `creator:`). Returns the matching tag
/// verbatim or [`NO_CREATOR_LABEL`] when none match.
/// Test: `creator_label_picks_first_match`.
pub fn creator_label(tags: &[String]) -> String {
    for tag in tags {
        if tag.starts_with("msg:from=")
            || tag.starts_with("tag:creator:")
            || tag.starts_with("creator:")
        {
            return tag.clone();
        }
    }
    NO_CREATOR_LABEL.to_string()
}
