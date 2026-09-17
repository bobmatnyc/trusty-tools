use super::*;
use tower::ServiceExt;

#[tokio::test]
async fn experiment_guard_rejects_unsafe_requests_and_allows_measurements() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let runner = RunnerState {
        config: parse_experiment_config(None, None).unwrap(),
        calls: Arc::new(AtomicU64::new(0)),
        source_revision: "abcdef0".into(),
        data_dir: root.join("data"),
        corpus_root: root.clone(),
    };
    let router = Router::new()
        .fallback(|| async { StatusCode::OK })
        .layer(middleware::from_fn_with_state(runner, fixture_guard));
    let create = serde_json::json!({"id":"experiment", "root_path":root,
        "skip_vector":true, "skip_kg":false});
    let cases = [
        ("POST", "/indexes", create.clone(), StatusCode::OK),
        (
            "POST",
            "/indexes",
            serde_json::json!({"id":"experiment", "root_path":root,
            "skip_vector":false,"skip_kg":false}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/indexes",
            serde_json::json!({"id":"other", "root_path":root,
            "skip_vector":true,"skip_kg":false}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/indexes",
            serde_json::json!({"id":"experiment", "root_path":"/",
            "skip_vector":true,"skip_kg":false}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/indexes/experiment/search",
            serde_json::json!({"stage":"graph"}),
            StatusCode::OK,
        ),
        (
            "POST",
            "/indexes/experiment/search",
            serde_json::json!({"stage":"semantic"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/indexes/experiment/reindex",
            serde_json::json!({"force":true}),
            StatusCode::OK,
        ),
        (
            "POST",
            "/indexes/experiment/reindex",
            serde_json::json!({"root_path":"/"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/admin/stop",
            serde_json::json!({}),
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/experiment/evidence",
            serde_json::json!({}),
            StatusCode::OK,
        ),
    ];
    for (method, uri, body, expected) in cases {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{method} {uri}: {body}");
    }
    for (field, value) in [
        ("include_paths", serde_json::json!(["/outside"])),
        ("include_paths", serde_json::json!(["../outside"])),
        ("follow_links", serde_json::json!(true)),
        ("allow_sensitive_path", serde_json::json!(true)),
    ] {
        for (uri, mut body) in [
            ("/indexes", create.clone()),
            (
                "/indexes/experiment/reindex",
                serde_json::json!({"force":true}),
            ),
        ] {
            body[field] = value.clone();
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}: {body}");
        }
    }
}

#[tokio::test]
async fn experiment_rejecting_embedder_records_both_forbidden_entrypoints() {
    let calls = Arc::new(AtomicU64::new(0));
    let embedder = RejectingEmbedder {
        calls: calls.clone(),
    };
    assert!(embedder.embed("source").await.is_err());
    assert!(embedder.embed_batch(&["source"]).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(embedder.dimension() > 0);
}
