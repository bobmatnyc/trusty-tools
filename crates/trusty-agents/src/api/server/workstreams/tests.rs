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

#[test]
fn workstream_tag_renders_ws_prefix() {
    assert_eq!(workstream_tag("feat-x"), "ws:feat-x");
}

#[test]
fn workstream_summary_tag_renders_ws_summary_prefix() {
    assert_eq!(workstream_summary_tag("feat-x"), "ws-summary:feat-x");
}

#[tokio::test]
async fn create_tagged_drawer_at_no_project_root_errs() {
    // Why: the write path must fail loudly (not silently drop the turn)
    // when there is nowhere to write it — a tempdir with no project
    // marker resolves no palace id.
    let tmp = tempfile::tempdir().expect("tempdir");
    let err = create_tagged_drawer_at(tmp.path(), "http://127.0.0.1:1", "content", vec![])
        .await
        .expect_err("no project root must error, not silently succeed");
    assert!(err.to_string().contains("no project root"));
}

#[tokio::test]
async fn drawers_by_tag_at_no_project_root_is_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = drawers_by_tag_at(tmp.path(), "http://127.0.0.1:1", "ws:feat-x", 10).await;
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
    use axum::Router;
    use axum::extract::{Path as AxumPath, Query, State};
    use axum::routing::{get, post};
    use std::net::SocketAddr;
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

    async fn mock_create_palace(Json(_body): Json<serde_json::Value>) -> Json<serde_json::Value> {
        Json(serde_json::json!({"ok": true}))
    }

    #[derive(Deserialize)]
    struct MockCreateDrawerBody {
        content: String,
        #[serde(default)]
        tags: Vec<String>,
    }

    async fn mock_create_drawer(
        State(state): State<std::sync::Arc<MockState>>,
        AxumPath(_palace_id): AxumPath<String>,
        Json(body): Json<MockCreateDrawerBody>,
    ) -> Json<serde_json::Value> {
        let mut drawers = state.drawers.lock().unwrap();
        let created_at = ts(drawers.len() as i64);
        drawers.push(MockDrawer {
            content: body.content,
            tags: body.tags,
            created_at,
        });
        Json(serde_json::json!({"ok": true}))
    }

    #[derive(Deserialize)]
    struct MockListQuery {
        tag: Option<String>,
    }

    async fn mock_list_drawers(
        State(state): State<std::sync::Arc<MockState>>,
        AxumPath(_palace_id): AxumPath<String>,
        Query(q): Query<MockListQuery>,
    ) -> Json<Vec<serde_json::Value>> {
        let drawers = state.drawers.lock().unwrap();
        let mut rows: Vec<&MockDrawer> = drawers
            .iter()
            .filter(|d| match &q.tag {
                Some(t) => d.tags.iter().any(|x| x == t),
                None => true,
            })
            .collect();
        rows.sort_by_key(|d| std::cmp::Reverse(d.created_at));
        Json(
            rows.into_iter()
                .map(|d| {
                    serde_json::json!({
                        "content": d.content,
                        "tags": d.tags,
                        "created_at": d.created_at,
                    })
                })
                .collect(),
        )
    }

    async fn spawn(state: std::sync::Arc<MockState>) -> SocketAddr {
        let app = Router::new()
            .route("/api/v1/palaces", post(mock_create_palace))
            .route(
                "/api/v1/palaces/{id}/drawers",
                get(mock_list_drawers).post(mock_create_drawer),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        addr
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
        let addr = spawn(state).await;
        let base_url = format!("http://{addr}");
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        create_tagged_drawer_at(tmp.path(), &base_url, "turn one", vec!["ws:feat-x".into()])
            .await
            .expect("first write succeeds");
        create_tagged_drawer_at(tmp.path(), &base_url, "turn two", vec!["ws:feat-x".into()])
            .await
            .expect("second write succeeds");
        create_tagged_drawer_at(
            tmp.path(),
            &base_url,
            "other turn",
            vec!["ws:feat-y".into()],
        )
        .await
        .expect("third write succeeds");

        let out = drawers_by_tag_at(tmp.path(), &base_url, "ws:feat-x", 10).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].content, "turn two", "newest-first ordering");
        assert_eq!(out[1].content, "turn one");
    }
}
