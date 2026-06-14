//! Unit tests for the trusty-memory client module.
//!
//! Why: live endpoints are covered by the trusty-memory daemon suite;
//! these tests cover URL helpers and all JSON-projection functions
//! without requiring a running daemon.
//! What: unit tests for `normalize_url`, `resolve_memory_url`,
//! `MemoryClient` construction, and all `parse_*` / `creator_label`
//! functions.
//! Test: this file is the test coverage.

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use super::super::client::MemoryClient;
    use super::super::parsers::{
        creator_label, parse_drawers, parse_dream_stats, parse_memory_details, parse_memory_event,
        parse_palaces, parse_recall_hits,
    };
    use super::super::types::{DEFAULT_MEMORY_URL, normalize_url, resolve_memory_url};
    use super::super::types::{
        DRAWER_SNIPPET_FALLBACK_MAX, DreamStats, MemoryEvent, NO_CREATOR_LABEL,
    };

    #[test]
    fn default_memory_url_is_local() {
        assert!(DEFAULT_MEMORY_URL.starts_with("http://127.0.0.1"));
    }

    #[test]
    fn normalize_url_adds_scheme() {
        assert_eq!(normalize_url("127.0.0.1:7070"), "http://127.0.0.1:7070");
        assert_eq!(
            normalize_url("http://127.0.0.1:7070"),
            "http://127.0.0.1:7070"
        );
    }

    #[test]
    fn memory_client_stores_base_url() {
        let client = MemoryClient::new("http://127.0.0.1:7070");
        assert_eq!(client.base_url(), "http://127.0.0.1:7070");
    }

    #[test]
    fn memory_client_repoints() {
        let mut client = MemoryClient::new("http://127.0.0.1:7070");
        client.set_base_url("http://127.0.0.1:8080");
        assert_eq!(client.base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn resolve_memory_url_returns_http_url() {
        let url = resolve_memory_url();
        assert!(url.starts_with("http://") || url.starts_with("https://"));
    }

    #[test]
    fn palace_list_accepts_array_and_object_shapes() {
        // Bare-array shape.
        let arr = serde_json::json!([
            {"id": "p1", "name": "default", "vector_count": 8400},
            {"id": "p2", "name": "work", "vectors": 0},
        ]);
        let rows = parse_palaces(&arr);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "p1");
        assert_eq!(rows[0].vector_count, 8400);
        // The `vectors` alias is honoured.
        assert_eq!(rows[1].name, "work");

        // Object-wrapped shape.
        let obj = serde_json::json!({
            "palaces": [{"id": "p3", "name": "notes", "total_vectors": 12}],
        });
        let rows = parse_palaces(&obj);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].vector_count, 12);

        // An unexpected shape yields no rows rather than panicking.
        assert!(parse_palaces(&serde_json::json!("nonsense")).is_empty());
    }

    #[test]
    fn parse_recall_hits_projects_fields() {
        // The recall endpoint returns a bare array; each hit projects
        // palace_id, a one-line snippet, and the score.
        let raw = serde_json::json!([
            {
                "palace_id": "default",
                "content": "JWT middleware added to auth flow\nmore detail",
                "score": 0.83,
            },
            {
                "palace_id": "work",
                "content": "  single line  ",
                "score": 0.5,
            },
        ]);
        let hits = parse_recall_hits(&raw);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].palace_id, "default");
        assert_eq!(hits[0].snippet, "JWT middleware added to auth flow");
        assert!((hits[0].score - 0.83).abs() < 1e-6);
        assert_eq!(hits[1].snippet, "single line");
        // A non-array payload yields no hits.
        assert!(parse_recall_hits(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn parse_dream_stats_reads_counts() {
        let raw = serde_json::json!({
            "merged": 3, "pruned": 1, "compacted": 0,
            "closets_updated": 5, "duration_ms": 42,
        });
        assert_eq!(
            parse_dream_stats(&raw),
            DreamStats {
                merged: 3,
                pruned: 1,
                compacted: 0,
            }
        );
        // Absent fields default to zero.
        assert_eq!(
            parse_dream_stats(&serde_json::json!({})),
            DreamStats::default()
        );
    }

    #[test]
    fn parse_memory_event_maps_type_tag() {
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "palace_created", "id": "p1", "name": "notes",
            })),
            Some(MemoryEvent::PalaceCreated {
                name: "notes".into(),
            })
        );
        // drawer_added with a content preview round-trips the preview.
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "drawer_added",
                "palace_id": "default",
                "drawer_count": 14,
                "content_preview": "How the migration system handles…",
            })),
            Some(MemoryEvent::DrawerAdded {
                palace_id: "default".into(),
                drawer_count: 14,
                content_preview: "How the migration system handles…".into(),
            })
        );
        // Older daemons omit `content_preview`; the field defaults to empty.
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "drawer_added", "palace_id": "default", "drawer_count": 14,
            })),
            Some(MemoryEvent::DrawerAdded {
                palace_id: "default".into(),
                drawer_count: 14,
                content_preview: String::new(),
            })
        );
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "dream_completed", "merged": 3, "pruned": 1, "compacted": 0,
            })),
            Some(MemoryEvent::DreamCompleted {
                merged: 3,
                pruned: 1,
                compacted: 0,
            })
        );
        // Housekeeping and unmodelled frames are dropped.
        assert!(parse_memory_event(&serde_json::json!({"type": "connected"})).is_none());
        assert!(parse_memory_event(&serde_json::json!({"type": "lag", "skipped": 2})).is_none());
        assert!(parse_memory_event(&serde_json::json!({"no": "type"})).is_none());
    }

    #[test]
    fn parse_drawers_projects_fields() {
        // Bare array shape — the daemon's current response. Row 0
        // carries an explicit `snippet`; row 1 only has `content` (the
        // fallback path); row 2 carries neither.
        let raw = serde_json::json!([
            {
                "id": "11111111-1111-1111-1111-111111111111",
                "created_at": "2026-05-20T12:34:56Z",
                "tags": ["msg:from=cto", "user-tag"],
                "content": "ignored when snippet is present",
                "snippet": "JWT middleware added",
            },
            {
                "id": "22222222-2222-2222-2222-222222222222",
                "created_at": "2026-05-19T08:00:00Z",
                "tags": ["creator:client=mpm", "creator:source=http"],
                "content": "Plain content for the legacy fallback path",
            },
            {
                "id": "33333333-3333-3333-3333-333333333333",
                "created_at": "bad-timestamp",
                "tags": [],
            },
        ]);
        let drawers = parse_drawers(&raw);
        assert_eq!(drawers.len(), 3);
        assert_eq!(drawers[0].id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(drawers[0].creator, "msg:from=cto");
        assert_eq!(drawers[0].tags.len(), 2);
        assert!(drawers[0].created_at.is_some());
        // Issue #202: explicit snippet wins over content.
        assert_eq!(drawers[0].snippet.as_deref(), Some("JWT middleware added"));

        assert_eq!(drawers[1].creator, "creator:client=mpm");
        // Issue #202: fall back to truncating `content` when snippet is absent.
        assert_eq!(
            drawers[1].snippet.as_deref(),
            Some("Plain content for the legacy fallback path"),
        );

        // Malformed timestamp drops to None; missing creator tag → em-dash;
        // no snippet and no content → snippet is None.
        assert!(drawers[2].created_at.is_none());
        assert_eq!(drawers[2].creator, NO_CREATOR_LABEL);
        assert!(drawers[2].snippet.is_none());

        // Object-wrapped shape.
        let obj = serde_json::json!({
            "drawers": [{"id": "abc", "tags": []}],
        });
        let drawers = parse_drawers(&obj);
        assert_eq!(drawers.len(), 1);
        assert_eq!(drawers[0].id, "abc");

        // Unexpected shape yields an empty list.
        assert!(parse_drawers(&serde_json::json!("nope")).is_empty());

        // An explicit `null` snippet (daemon returned `Value::Null`) also
        // yields `None` — neither the snippet nor the absent content
        // fields fill it in.
        let null_snippet = serde_json::json!([{
            "id": "44444444-4444-4444-4444-444444444444",
            "snippet": serde_json::Value::Null,
            "tags": [],
        }]);
        let drawers = parse_drawers(&null_snippet);
        assert!(drawers[0].snippet.is_none());

        // Long content gets truncated by the client fallback.
        let long_content = "x".repeat(200);
        let long = serde_json::json!([{
            "id": "55555555-5555-5555-5555-555555555555",
            "content": long_content,
            "tags": [],
        }]);
        let drawers = parse_drawers(&long);
        let snippet = drawers[0].snippet.as_deref().expect("fallback snippet");
        assert_eq!(snippet.chars().count(), DRAWER_SNIPPET_FALLBACK_MAX);
        assert!(
            snippet.ends_with('…'),
            "long fallback snippet must be truncated with ellipsis",
        );
    }

    /// Why (issue #215): the detail modal must see the full `content`
    /// field on every drawer; the row-oriented `parse_drawers` projection
    /// deliberately omits it, so `parse_memory_details` is the channel.
    /// What: feeds a bare array and an object-wrapped array of drawer
    /// payloads through the projection and asserts each row keeps its
    /// full body, tag list, and timestamp.
    /// Test: itself.
    #[test]
    fn parse_memory_details_projects_full_content() {
        let raw = serde_json::json!([
            {
                "id": "11111111-1111-1111-1111-111111111111",
                "created_at": "2026-05-20T12:34:56Z",
                "tags": ["msg:from=cto"],
                "content": "Full memory body the modal renders verbatim.",
            },
            {
                "id": "22222222-2222-2222-2222-222222222222",
                "created_at": "bad-timestamp",
                "tags": [],
                "content": "",
            },
        ]);
        let details = parse_memory_details(&raw);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(
            details[0].content,
            "Full memory body the modal renders verbatim."
        );
        assert_eq!(details[0].tags, vec!["msg:from=cto".to_string()]);
        assert!(details[0].created_at.is_some());

        // Empty content / bad timestamp degrade to safe defaults instead of
        // dropping the row.
        assert!(details[1].created_at.is_none());
        assert!(details[1].content.is_empty());

        // Object-wrapped shape.
        let obj = serde_json::json!({
            "drawers": [{"id": "abc", "content": "wrapped", "tags": []}],
        });
        let details = parse_memory_details(&obj);
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].content, "wrapped");

        // Unexpected shape yields an empty list.
        assert!(parse_memory_details(&serde_json::json!("nope")).is_empty());
    }

    #[test]
    fn creator_label_picks_first_match() {
        // First matching tag wins, in the tag list's order.
        let label = creator_label(&[
            "user-tag".into(),
            "msg:from=cto".into(),
            "creator:client=mpm".into(),
        ]);
        assert_eq!(label, "msg:from=cto");

        // `tag:creator:` legacy prefix is recognised.
        let label = creator_label(&["tag:creator:client=mpm".into()]);
        assert_eq!(label, "tag:creator:client=mpm");

        // `creator:` alone (HTTP attribution) is recognised.
        let label = creator_label(&["creator:source=http".into()]);
        assert_eq!(label, "creator:source=http");

        // No recognised tags → em-dash placeholder.
        assert_eq!(
            creator_label(&["user-tag".into(), "kind:note".into()]),
            NO_CREATOR_LABEL,
        );
        assert_eq!(creator_label(&[]), NO_CREATOR_LABEL);
    }
}
