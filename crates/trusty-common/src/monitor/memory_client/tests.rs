//! Unit tests for the trusty-memory client module.
//!
//! Why: live behaviour is covered by the trusty-memory daemon suite; these
//! tests cover socket resolution, the activity-poll cursor, and every
//! JSON-projection function without requiring a running daemon.
//! What: unit tests for `resolve_memory_socket`, `MemoryClient` construction,
//! `project_events`, `project_palaces`, and all `parse_*` / `creator_label`
//! functions.
//! Test: this file is the test coverage.

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use super::super::client::MemoryClient;
    use super::super::client::{project_events, project_palaces};
    use super::super::parsers::{
        creator_label, parse_drawers, parse_dream_stats, parse_memory_details, parse_memory_event,
        parse_palace_detail, parse_recall_hits,
    };
    use super::super::types::resolve_memory_socket;
    use super::super::types::{
        DRAWER_SNIPPET_FALLBACK_MAX, DreamStats, MemoryEvent, NO_CREATOR_LABEL,
    };

    /// Why (#6286): the client dials a derived socket rather than a base URL,
    /// and the poller re-resolves it every tick, so both the stored path and
    /// the setter have to hold what they were given.
    /// Test: this test.
    #[test]
    fn memory_client_stores_its_socket() {
        let client = MemoryClient::new("/tmp/a.sock");
        assert_eq!(client.socket(), std::path::Path::new("/tmp/a.sock"));
    }

    /// Why: a `TRUSTY_MEMORY_SOCKET` or data-dir change between ticks moves the
    /// path under a running dashboard.
    /// Test: this test.
    #[test]
    fn memory_client_repoints() {
        let mut client = MemoryClient::new("/tmp/a.sock");
        client.set_socket("/tmp/b.sock");
        assert_eq!(client.socket(), std::path::Path::new("/tmp/b.sock"));
    }

    /// Why (#6286): the monitor and the daemon must derive the SAME path, and
    /// nothing publishes it — so a resolution that answered a bare filename or
    /// an empty path would fail only at dial time.
    /// What: asserts the resolved path is absolute and names the daemon.
    /// Test: this test.
    #[test]
    fn resolve_memory_socket_names_the_daemon_socket() {
        let socket = resolve_memory_socket().expect("resolve the socket path");
        assert!(socket.is_absolute(), "got {}", socket.display());
        assert!(
            socket.to_string_lossy().contains("trusty-memory"),
            "got {}",
            socket.display()
        );
    }

    /// Why (#6286): the activity poll replaced the `/sse` subscription, and the
    /// cursor is the only part that can be wrong silently — a cursor that fails
    /// to advance replays every event on every tick, and one that advances past
    /// unread rows drops them. Ordering matters too: the daemon answers
    /// newest-first and the activity log renders oldest-first.
    /// What: feeds a two-row page against a cursor below both, then re-feeds it
    /// against the returned cursor.
    /// Test: this test.
    #[test]
    fn recent_events_keeps_only_rows_past_the_cursor() {
        let page = serde_json::json!({
            "entries": [
                {"id": 9, "payload": {"type": "palace_created", "name": "newer"}},
                {"id": 8, "payload": {"type": "palace_created", "name": "older"}},
            ],
            "total": 2, "limit": 100, "offset": 0,
        });

        let (cursor, events) = project_events(&page, 7);
        assert_eq!(cursor, 9);
        assert_eq!(
            events,
            vec![
                MemoryEvent::PalaceCreated {
                    name: "older".to_string()
                },
                MemoryEvent::PalaceCreated {
                    name: "newer".to_string()
                },
            ],
            "oldest first, both rows past the cursor"
        );

        let (cursor, events) = project_events(&page, cursor);
        assert_eq!(cursor, 9, "an unchanged page must not move the cursor");
        assert!(events.is_empty(), "nothing past the cursor replays");
    }

    /// Why (#6286): the palace panel is assembled from `memory.palaces_list`
    /// now, and the readable case has to project the nested `palace` object
    /// rather than the row itself — a projection reading the row would find no
    /// counts and drop it, which is the failure this method replaced.
    /// What: feeds a one-row roster and asserts the counts land.
    /// Test: this test.
    #[test]
    fn palaces_project_a_readable_row() {
        let roster = serde_json::json!({
            "palaces": [{
                "id": "cto",
                "error": null,
                "palace": {
                    "id": "cto",
                    "name": "cto",
                    "drawer_count": 12,
                    "vector_count": 12,
                    "kg_triple_count": 3,
                    "cached": true,
                },
            }],
        });
        let rows = project_palaces(&roster);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "cto");
        assert_eq!(rows[0].drawer_count, 12);
        assert!(
            !rows[0].counts_unknown,
            "a row the daemon read is a measurement"
        );
    }

    /// Why (#6286 review, finding 7): the fan-out this replaced dropped a
    /// palace whose fetch failed at `debug!`, so the panel could report a
    /// palace COUNT above fewer rows than that with nothing saying which were
    /// missing. A failed row must be RENDERED — carrying `counts_unknown` so no
    /// zero is read as a measurement, and the daemon's reason so an operator
    /// can act on it.
    /// What: feeds a roster whose second row carries `error` instead of
    /// `palace`, and asserts both rows survive.
    /// Test: this test.
    #[test]
    fn palaces_project_a_failed_row_rather_than_dropping_it() {
        let roster = serde_json::json!({
            "palaces": [
                {
                    "id": "healthy",
                    "error": null,
                    "palace": { "id": "healthy", "name": "healthy", "drawer_count": 4 },
                },
                { "id": "wedged", "error": "open KG for wedged: permission denied" },
            ],
        });
        let rows = project_palaces(&roster);
        assert_eq!(
            rows.len(),
            2,
            "a palace the daemon could not read must still be a row: {rows:?}"
        );
        let wedged = rows
            .iter()
            .find(|r| r.id == "wedged")
            .expect("the failed row");
        assert!(wedged.counts_unknown, "its zeros mean unknown, never empty");
        assert_eq!(
            wedged.description.as_deref(),
            Some("open KG for wedged: permission denied"),
            "the daemon's reason has to reach the panel"
        );
    }

    /// Why: a roster shape this client does not know is the one remaining drop,
    /// and it must not take the rest of the roster with it.
    /// What: feeds a row carrying neither counts nor a reason alongside a good
    /// one, and asserts only the unknown row is skipped.
    /// Test: this test.
    #[test]
    fn palaces_skip_only_the_row_they_cannot_read() {
        let roster = serde_json::json!({
            "palaces": [
                { "id": "mystery" },
                { "id": "ok", "error": null, "palace": { "id": "ok", "name": "ok" } },
            ],
        });
        let rows = project_palaces(&roster);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "ok");

        assert!(
            project_palaces(&serde_json::json!({})).is_empty(),
            "an answer with no palaces array is empty, not a panic"
        );
    }

    /// Why (issue #4682): `cached: false` rows carry placeholder zeros — 2,180
    /// of 2,183 rows on a live daemon — and projecting those as measurements is
    /// what made the header read `0 drawers` above a "Drawers (1)" list. #6286
    /// moved the assertion from the retired bulk-list projection onto the
    /// single-palace one, which is the only shape the socket answers.
    /// What: asserts an uncached row is flagged `counts_unknown` and every
    /// accessor returns `None`, while a `cached: true` row keeps real counts.
    /// Test: this test.
    #[test]
    fn parse_palace_detail_marks_uncached_rows_unknown() {
        let cold = parse_palace_detail(&serde_json::json!({
            "id": "cold", "name": "cold", "vector_count": 0, "drawer_count": 0,
            "kg_triple_count": 0, "node_count": 0, "edge_count": 0, "cached": false
        }))
        .expect("projects");
        assert!(cold.counts_unknown, "cached:false marks the counts unknown");
        assert_eq!(cold.vectors(), None, "a placeholder 0 must not read as 0");
        assert_eq!(cold.drawers(), None);
        assert_eq!(cold.kg_triples(), None);
        assert_eq!(cold.nodes(), None);
        assert_eq!(cold.edges(), None);

        let warm = parse_palace_detail(&serde_json::json!({
            "id": "warm", "name": "warm", "vector_count": 912, "drawer_count": 38,
            "kg_triple_count": 122, "node_count": 9, "edge_count": 8, "cached": true
        }))
        .expect("projects");
        assert!(!warm.counts_unknown);
        assert_eq!(warm.vectors(), Some(912));
        assert_eq!(warm.drawers(), Some(38));
        assert_eq!(warm.kg_triples(), Some(122));
    }

    /// Why (issue #4682): `cached` only exists on daemons carrying #4640. An
    /// older daemon opened every palace, so its counts are authoritative —
    /// defaulting the absent flag to `false` would make a current client print
    /// `—` for every palace against it.
    /// What: asserts a payload with no `cached` key keeps its counts, and that
    /// a genuinely empty *cached* palace still reads as a known `0` rather
    /// than unknown.
    /// Test: this test.
    #[test]
    fn parse_palace_detail_trusts_counts_when_cached_flag_absent() {
        let legacy = parse_palace_detail(
            &serde_json::json!({"id": "p1", "name": "p1", "vector_count": 8400}),
        )
        .expect("projects");
        assert!(!legacy.counts_unknown, "absent flag != not loaded");
        assert_eq!(legacy.vectors(), Some(8400));

        let empty_but_loaded = parse_palace_detail(
            &serde_json::json!({"id": "p2", "name": "p2", "vector_count": 0, "cached": true}),
        )
        .expect("projects");
        assert_eq!(
            empty_but_loaded.vectors(),
            Some(0),
            "an empty loaded palace is a known zero, not unknown"
        );
    }

    /// Why (issue #4682): the CLI's single-id path must read the route that
    /// opens the palace; this pins the projection it depends on.
    /// What: asserts a single palace object projects to a row with live counts.
    /// Test: this test.
    #[test]
    fn parse_palace_detail_reads_live_counts() {
        let raw = serde_json::json!({
            "id": "t-tmpugxp9v", "name": "t-tmpugxp9v",
            "drawer_count": 1, "vector_count": 1, "kg_triple_count": 8,
            "node_count": 9, "edge_count": 8, "cached": true,
        });
        let row = parse_palace_detail(&raw).expect("single palace object projects");
        assert_eq!(row.id, "t-tmpugxp9v");
        assert_eq!(row.vectors(), Some(1));
        assert_eq!(row.drawers(), Some(1));
        assert_eq!(row.kg_triples(), Some(8));
    }

    /// Why (issue #4682): a non-object 2xx body must surface as an error, not
    /// as a row of silent zeros the CLI would print as fact.
    /// What: asserts arrays, strings, and null all yield `None`.
    /// Test: this test.
    #[test]
    fn parse_palace_detail_rejects_non_object() {
        assert!(parse_palace_detail(&serde_json::json!([])).is_none());
        assert!(parse_palace_detail(&serde_json::json!("nonsense")).is_none());
        assert!(parse_palace_detail(&serde_json::Value::Null).is_none());
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
