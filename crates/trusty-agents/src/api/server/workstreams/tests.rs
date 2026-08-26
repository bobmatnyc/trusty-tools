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

/// A socket path nothing can be serving.
///
/// Why (#6286): the retired rigs pointed at `http://127.0.0.1:1` — a reserved
/// port — so a dial failed immediately rather than timing out. A path under a
/// directory that cannot exist is the socket equivalent.
fn unreachable_socket() -> &'static std::path::Path {
    std::path::Path::new("/nonexistent/trusty-memory/trusty-memory.sock")
}

#[tokio::test]
async fn list_workstreams_at_no_project_root_is_empty() {
    // A tempdir with no git/project marker resolves no palace id, so the
    // function returns empty without ever making a network call.
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = list_workstreams_at(tmp.path(), unreachable_socket()).await;
    assert_eq!(out, Vec::new());
}

#[tokio::test]
async fn list_workstreams_at_unreachable_daemon_is_empty_not_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
    // Port 1 is a reserved, never-listening port — the health probe
    // fails fast and the function degrades to an empty list.
    let out = list_workstreams_at(tmp.path(), unreachable_socket()).await;
    assert_eq!(out, Vec::new());
}

#[test]
fn workstream_tag_renders_ws_prefix() {
    assert_eq!(workstream_tag("feat-x"), "ws:feat-x");
}

#[test]
fn workstream_summary_tag_renders_ws_summary_prefix() {
    assert_eq!(workstream_summary_tag("feat-x"), "ws-summary:feat-x");
}

/// Why (#5811): a directory with no project marker and no git remote is a
/// LEGITIMATE palace owner, not a failure — trusty-agents mints a palace per
/// assistant, and those have no repo at all. This used to assert the opposite,
/// because the endpoint resolved through `project_slug_at_readonly`, which is
/// pin-then-basename and returns `None` outside a project. The shared resolver
/// answers via level 4 (`parent/dir` of the main worktree root), so a
/// projectless caller resolves and the write proceeds to the daemon.
/// What: asserts the call still fails LOUDLY (the absent socket is
/// unreachable) but NOT for want of a project root.
/// Test: itself.
#[tokio::test]
async fn create_tagged_drawer_at_without_a_project_root_still_resolves_a_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let err = create_tagged_drawer_at(tmp.path(), unreachable_socket(), "content", vec![])
        .await
        .expect_err("an unreachable daemon must error, not silently succeed");
    let msg = err.to_string();
    assert!(
        !msg.contains("resolve palace for"),
        "a projectless caller must still resolve a palace, got: {msg}"
    );
}

/// Why (#5811): the one input that legitimately blocks the write is a committed
/// pin that cannot be trusted. Deriving past it would send the drawer to a
/// palace nobody chose, so the write must stop before any HTTP call.
/// What: `.trusty-tools/` is itself a project marker, so the tempdir IS the root
/// the resolver stops at, and the pin body is not valid pin YAML.
/// Test: itself.
#[tokio::test]
async fn create_tagged_drawer_at_malformed_pin_errs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pin_dir = tmp.path().join(".trusty-tools");
    std::fs::create_dir_all(&pin_dir).expect("create .trusty-tools");
    std::fs::write(
        pin_dir.join("trusty-memory.yaml"),
        "palace: [unclosed\n\t bad: :",
    )
    .expect("write malformed pin");

    let err = create_tagged_drawer_at(tmp.path(), unreachable_socket(), "content", vec![])
        .await
        .expect_err("an untrustworthy pin must stop the write");
    assert!(
        err.to_string().contains("resolve palace for"),
        "expected a resolution failure, got: {err}"
    );
}

#[tokio::test]
async fn drawers_by_tag_at_no_project_root_is_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = drawers_by_tag_at(tmp.path(), unreachable_socket(), "ws:feat-x", 10).await;
    assert_eq!(out, Vec::new());
}

// -----------------------------------------------------------------
// Mock HTTP server covering the write path (`POST /api/v1/palaces`,
// `POST /api/v1/palaces/{id}/drawers`, `GET /api/v1/palaces/{id}/drawers`)
// — mirrors `memory::trusty_client::tests`' mock server rather than
// reinventing it, narrowed to only the routes this module's write path
// exercises.
// -----------------------------------------------------------------
mod mock_daemon {
    use super::*;
    use crate::uds_mock::{self, MockMemoryDaemon};
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    #[derive(Clone)]
    struct MockDrawer {
        content: String,
        tags: Vec<String>,
        created_at: DateTime<Utc>,
    }

    #[derive(Default)]
    struct MockState {
        drawers: StdMutex<Vec<MockDrawer>>,
    }

    /// Serve the three methods this module's read and write paths call.
    ///
    /// `memory.health` and `palace_create` answer trivially; the drawer pair is
    /// the stateful part, and `memory.drawers_list` sorts newest-first the way
    /// the daemon does for `sort=created_desc`.
    async fn spawn(state: Arc<MockState>) -> MockMemoryDaemon {
        uds_mock::spawn(move |method: &str, params: serde_json::Value| {
            let state = Arc::clone(&state);
            let method = method.to_string();
            Box::pin(async move {
                match method.as_str() {
                    "memory.health" => Ok(serde_json::json!({"status": "ok"})),
                    "palace_create" => Ok(serde_json::json!({"ok": true})),
                    "memory.drawer_create" => {
                        let mut drawers = state.drawers.lock().unwrap();
                        let created_at = ts(drawers.len() as i64);
                        drawers.push(MockDrawer {
                            content: params["content"].as_str().unwrap_or_default().to_string(),
                            tags: params["tags"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str().map(str::to_string))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            created_at,
                        });
                        Ok(serde_json::json!({"id": "d1"}))
                    }
                    "memory.drawers_list" => {
                        let wanted = params["tag"].as_str().map(str::to_string);
                        let drawers = state.drawers.lock().unwrap();
                        let mut rows: Vec<&MockDrawer> = drawers
                            .iter()
                            .filter(|d| match &wanted {
                                Some(t) => d.tags.iter().any(|x| x == t),
                                None => true,
                            })
                            .collect();
                        rows.sort_by_key(|d| std::cmp::Reverse(d.created_at));
                        Ok(serde_json::json!(
                            rows.into_iter()
                                .map(|d| serde_json::json!({
                                    "content": d.content,
                                    "tags": d.tags,
                                    "created_at": d.created_at,
                                }))
                                .collect::<Vec<_>>()
                        ))
                    }
                    other => Err(uds_mock::RpcError::method_not_found(other, &[])),
                }
            })
        })
        .await
    }

    /// Why: proves `create_tagged_drawer_at` + `drawers_by_tag_at` compose
    /// into a working write/read round trip against the real route
    /// shapes (mirrors `memory::trusty_client::tests`'
    /// `insert_get_delete_round_trip_against_mock_daemon`), and that a
    /// drawer tagged for one workstream is invisible under a different
    /// tag.
    /// What: writes two drawers under `ws:feat-x`, one under
    /// `ws:feat-y`; asserts `drawers_by_tag_at` returns exactly the
    /// matching two, newest first.
    /// Test: this test.
    #[tokio::test]
    async fn create_tagged_drawer_at_and_drawers_by_tag_at_round_trip() {
        let state = std::sync::Arc::new(MockState::default());
        let daemon = spawn(state).await;
        let socket = daemon.socket().to_path_buf();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        create_tagged_drawer_at(tmp.path(), &socket, "turn one", vec!["ws:feat-x".into()])
            .await
            .expect("first write succeeds");
        create_tagged_drawer_at(tmp.path(), &socket, "turn two", vec!["ws:feat-x".into()])
            .await
            .expect("second write succeeds");
        create_tagged_drawer_at(
            tmp.path(),
            &socket,
            "other turn",
            vec!["ws:feat-y".into()],
        )
        .await
        .expect("third write succeeds");

        let out = drawers_by_tag_at(tmp.path(), &socket, "ws:feat-x", 10).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].content, "turn two", "newest-first ordering");
        assert_eq!(out[1].content, "turn one");
    }

    /// Why: `list_workstream_labels_at` is a thin bare-name projection over
    /// `list_workstreams_at` (itself only unit-tested against an empty/
    /// unreachable daemon above) — the classification block (DOC-54
    /// §9.6.1, `ctrl::pm_task::dispatch::classification`) needs the CLOSED
    /// label vocabulary as plain strings, so this is worth its own
    /// happy-path guard against a real mock daemon rather than trusting the
    /// composition untested.
    /// What: seeds one `WS-CLAIM`-tagged drawer under `ws:feat-x` and one
    /// under `ws:feat-y`; asserts the label list contains exactly both bare
    /// names (order not asserted — `group_by_workstream`'s recency sort is
    /// covered separately).
    /// Test: this test.
    #[tokio::test]
    async fn list_workstream_labels_at_against_mock_daemon() {
        let state = std::sync::Arc::new(MockState::default());
        let daemon = spawn(state).await;
        let socket = daemon.socket().to_path_buf();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        create_tagged_drawer_at(
            tmp.path(),
            &socket,
            "WS-CLAIM feat-x: does a thing",
            vec!["ws-claim".into(), "ws:feat-x".into()],
        )
        .await
        .expect("first write succeeds");
        create_tagged_drawer_at(
            tmp.path(),
            &socket,
            "WS-CLAIM feat-y: does another thing",
            vec!["ws-claim".into(), "ws:feat-y".into()],
        )
        .await
        .expect("second write succeeds");

        let labels = list_workstream_labels_at(tmp.path(), &socket).await;
        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&"feat-x".to_string()));
        assert!(labels.contains(&"feat-y".to_string()));
    }
}
