//! Tests for the `GET /health` handler and health probe helpers.
use super::super::health::{
    ensure_health_probe_palace, run_health_round_trip_inner, HealthProbeError,
};
use super::super::recall_routes::recall_entry_json;
use super::super::router;
use super::super::HEALTH_PROBE_PALACE;
use super::test_state;
use crate::service::{drawer_content_preview, DRAWER_PREVIEW_MAX_CHARS};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::RecallResult;
use uuid::Uuid;

#[test]
fn drawer_preview_collapses_whitespace_and_truncates() {
    // Short single-line content is returned verbatim.
    assert_eq!(drawer_content_preview("hello world"), "hello world");

    // Multiline / tab-laden content collapses to single-spaced text.
    assert_eq!(
        drawer_content_preview("first line\n\nsecond\tline   third"),
        "first line second line third"
    );

    // Leading / trailing whitespace is stripped.
    assert_eq!(drawer_content_preview("   padded   "), "padded");

    // Empty content yields an empty preview (fallback signal for clients).
    assert_eq!(drawer_content_preview(""), "");

    // Long content is truncated to DRAWER_PREVIEW_MAX_CHARS with an ellipsis.
    let long = "x".repeat(DRAWER_PREVIEW_MAX_CHARS + 50);
    let preview = drawer_content_preview(&long);
    assert_eq!(preview.chars().count(), DRAWER_PREVIEW_MAX_CHARS);
    assert!(preview.ends_with('…'));

    // Content right at the limit is not truncated.
    let exact = "y".repeat(DRAWER_PREVIEW_MAX_CHARS);
    assert_eq!(drawer_content_preview(&exact), exact);
}

/// `GET /health` returns HTTP 200 with `status: "ok"` after the
/// round-trip clears every stage against the auto-provisioned probe palace.
///
/// Why: confirms the JSON contract (`status`, `version`) for monitors that
/// poll `/health`. Marked `#[ignore]` because issue #185 routes the probe
/// through the dedicated palace and `recall_with_default_embedder` loads
/// ONNX — too heavy for the default CI matrix. Run with
/// `cargo test -p trusty-memory -- --include-ignored`.
/// What: Drives `/health` and asserts the basic JSON keys.
/// Test: this test.
#[tokio::test]
#[ignore = "loads the default ONNX embedder; run with --include-ignored"]
async fn health_endpoint_returns_ok() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
}

/// Issue #35 — `GET /health` carries the enriched resource block
/// (`rss_mb`, `disk_bytes`, `cpu_pct`, `uptime_secs`).
///
/// Why: external probes and the admin UI render these; the JSON contract
/// must remain stable. `rss_mb` is sampled live so it is asserted only
/// for a sane unit, not an exact value. Marked `#[ignore]` because
/// issue #185 makes every `/health` request run the full round-trip and
/// `recall_with_default_embedder` loads the ONNX embedder.
/// What: drives `/health` through the router and asserts every new field
/// deserialises with a plausible value.
/// Test: this test.
#[tokio::test]
#[ignore = "loads the default ONNX embedder; run with --include-ignored"]
async fn health_endpoint_includes_resource_fields() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    // rss_mb must be a sane unit (megabytes, not bytes).
    let rss_mb = v["rss_mb"].as_u64().expect("rss_mb is u64");
    assert!(rss_mb < 1024 * 1024, "rss_mb unit must be MB");
    // cpu_pct is a non-negative percentage (first sample may be 0.0).
    let cpu = v["cpu_pct"].as_f64().expect("cpu_pct is a number");
    assert!(cpu >= 0.0, "cpu_pct must be non-negative");
    // disk ticker has not run in this oneshot test → 0.
    assert_eq!(v["disk_bytes"].as_u64(), Some(0));
    // uptime_secs is present and a u64.
    assert!(v["uptime_secs"].is_u64(), "uptime_secs must be present");
}

/// Why: the fd-exhaustion gauge must appear in the `/health` response on
/// Unix platforms so operators can monitor fd consumption vs. the ceiling.
/// What: drives `/health` through the router and asserts that `open_fds`
/// and `fd_soft_limit` are present and are non-zero unsigned integers.
/// On non-Unix platforms the fields may be absent (the helpers return None
/// and are skipped in serialisation) — that is acceptable and tested here
/// by not asserting presence, only asserting that when present they are sane.
/// Test: this test.
#[tokio::test]
async fn health_endpoint_includes_fd_gauge() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();

    // On Unix, both fields must be present and sane.
    #[cfg(unix)]
    {
        let open_fds = v["open_fds"]
            .as_u64()
            .expect("open_fds must be present on Unix");
        assert!(
            open_fds > 0,
            "open_fds must be > 0 (at least stdin/stdout/stderr)"
        );

        let limit = v["fd_soft_limit"]
            .as_u64()
            .expect("fd_soft_limit must be present on Unix");
        assert!(limit > 0, "fd_soft_limit must be > 0");

        // Sanity: open_fds should be well below the ceiling on test machines.
        assert!(
            open_fds < limit,
            "open_fds ({open_fds}) must be below fd_soft_limit ({limit}) in tests"
        );
    }
}

/// Issue #71 + #185 — `GET /health` reports `status: "ok"` on a fresh
/// install by auto-provisioning the dedicated probe palace and running
/// the full remember/recall/forget cycle against it.
///
/// Why: Pre-#185 the handler short-circuited with "no palaces" on a fresh
/// install, so a broken data plane would not surface until a real user
/// created a palace. The dedicated `__health_probe__` palace removes that
/// blind spot: the probe runs from boot. Marked `#[ignore]` because the
/// round-trip now loads the ONNX embedder via `recall_with_default_embedder`,
/// which is too heavy for the default CI matrix — run with
/// `cargo test -p trusty-memory -- --include-ignored` for local verification.
/// What: Drives `/health` through the router with an empty `data_root`
/// and asserts `status == "ok"` (probe palace was auto-created and the
/// round-trip cleared every stage) and the `detail` key is absent.
/// Test: this test.
#[tokio::test]
#[ignore = "loads the default ONNX embedder; run with --include-ignored"]
async fn health_endpoint_round_trip_on_fresh_install_is_ok() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["status"], "ok");
    assert!(
        v.get("detail").is_none() || v["detail"].is_null(),
        "fresh-install health must not carry a degraded detail (got {v:?})"
    );
}

/// Issue #71 — `GET /health` exercises the full store/recall/forget
/// cycle against the first palace and reports `status: "ok"` on success.
///
/// Why: The whole point of issue #71 is to catch store/recall
/// regressions at probe time rather than via real client traffic. This
/// test creates a real palace, hits `/health`, and asserts the
/// round-trip path is happy. Marked `#[ignore]` because
/// `recall_with_default_embedder` pulls in the ONNX model and is too
/// heavy for the default CI matrix — run with
/// `cargo test -p trusty-memory -- --include-ignored` for local
/// verification.
/// What: Builds an `AppState` with a tempdir `data_root`, creates a
/// `health-probe-palace` via `registry.create_palace`, hits `/health`,
/// and asserts both the status and the absence of any `detail` field.
/// Test: this test.
#[tokio::test]
#[ignore = "loads the default ONNX embedder; run with --include-ignored"]
async fn health_endpoint_round_trip_with_palace_is_ok() {
    let state = test_state();
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("health-probe-palace"),
        name: "health-probe-palace".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("health-probe-palace"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 2048).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        v["status"], "ok",
        "round-trip should succeed against a fresh palace; got {v:?}"
    );
    assert!(
        v.get("detail").is_none() || v["detail"].is_null(),
        "successful round-trip must not carry a detail field (got {v:?})"
    );
}

/// Issue #185 — the `__health_probe__` palace is hidden from
/// `MemoryService::list_palaces`.
///
/// Why: The dedicated health-probe palace exists on disk and must keep
/// existing across restarts, but it is an internal implementation detail
/// of `/health` and must never confuse the user (in the admin UI, TUI,
/// chat-tool palace roster, etc.).
/// What: Provisions the probe palace via the same helper the handler uses,
/// confirms the directory exists on disk, then asks
/// `MemoryService::list_palaces` for the user-facing roster and asserts
/// no palace with the reserved id (or any `__`-prefixed id) is returned.
/// Test: this test.
#[tokio::test]
async fn health_probe_palace_is_invisible() {
    let state = test_state();
    ensure_health_probe_palace(&state).expect("ensure_health_probe_palace");

    // The probe palace was persisted under the data root.
    assert!(
        state.data_root.join(HEALTH_PROBE_PALACE).exists(),
        "probe palace directory should be persisted on disk"
    );

    let service = crate::service::MemoryService::new(state);
    let listed = service.list_palaces().await.expect("list_palaces");
    assert!(
        listed.iter().all(|p| !p.id.starts_with("__")),
        "no `__`-prefixed palace may appear in the user-facing list; got {:?}",
        listed.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    assert!(
        !listed.iter().any(|p| p.id == HEALTH_PROBE_PALACE),
        "the dedicated `__health_probe__` palace must be invisible; got {:?}",
        listed.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
}

/// Issue #185 — after a successful round-trip, the probe palace holds
/// zero drawers.
///
/// Why: The probe must clean up after itself on every success path. If
/// the forget step were ever skipped silently, the probe palace would
/// grow unbounded over time (the original symptom was ~1,420 leaked
/// drawers in `localLLM`). This test pins the post-condition without
/// requiring the heavy ONNX recall — it exercises
/// `run_health_round_trip_inner` with a recall stub that returns a
/// synthetic hit matching the probe drawer id.
/// What: Provisions the probe palace, opens its handle, runs the inner
/// round-trip with a stubbed recall that returns the probe drawer, and
/// asserts the handle's drawer count drops back to zero.
/// Test: this test.
#[tokio::test]
async fn health_probe_cleans_up_on_success() {
    use trusty_common::memory_core::Drawer;

    let state = test_state();
    ensure_health_probe_palace(&state).expect("ensure_health_probe_palace");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(HEALTH_PROBE_PALACE))
        .expect("open probe palace");

    let result = run_health_round_trip_inner(handle.clone(), move |h, _query| async move {
        // Synthesize a hit that points at the most recently stored drawer
        // so the round-trip treats this as a successful recall.
        let drawers = h.drawers.read();
        let last = drawers
            .last()
            .cloned()
            .unwrap_or_else(|| Drawer::new(Uuid::new_v4(), "stub"));
        drop(drawers);
        Ok(vec![RecallResult {
            drawer: last,
            score: 1.0,
            layer: 1,
        }])
    })
    .await;
    assert!(
        result.is_ok(),
        "successful round-trip should return Ok; got {result:?}"
    );

    let drawer_count = handle.drawers.read().len();
    assert_eq!(
        drawer_count, 0,
        "probe palace must have zero drawers after a successful round-trip (got {drawer_count})"
    );
}

/// Issue #185 — when recall returns an empty result, the probe drawer is
/// still deleted before the round-trip surfaces the failure.
///
/// Why: This is the bug fix's central correctness property. Before #185
/// the empty-result branch did `return Err(RecallMiss)` *before* calling
/// `handle.forget(drawer_id)`, leaking the drawer. The new code calls
/// forget unconditionally and then evaluates the recall outcome, so a
/// recall miss can never leave a drawer behind.
/// What: Drives `run_health_round_trip_inner` with a recall stub that
/// returns an empty `Vec`, asserts the function reports
/// `HealthProbeError::ProbeMissing`, and then asserts the probe palace
/// is empty.
/// Test: this test.
#[tokio::test]
async fn health_probe_cleans_up_on_recall_miss() {
    let state = test_state();
    ensure_health_probe_palace(&state).expect("ensure_health_probe_palace");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(HEALTH_PROBE_PALACE))
        .expect("open probe palace");

    let result = run_health_round_trip_inner(handle.clone(), |_h, _q| async move {
        // Empty result — pre-#185 this leaked the drawer.
        Ok(Vec::new())
    })
    .await;
    assert!(
        matches!(result, Err(HealthProbeError::ProbeMissing(_))),
        "recall miss must surface as ProbeMissing; got {result:?}"
    );

    let drawer_count = handle.drawers.read().len();
    assert_eq!(
        drawer_count, 0,
        "probe palace must be empty after a recall miss (got {drawer_count})"
    );
}

/// Issue #185 — when recall errors out, the probe drawer is still
/// deleted before the round-trip surfaces the failure.
///
/// Why: The second leak mode pre-#185: `recall` returning `Err(_)` made
/// the function `return Err(Recall(e))` before reaching `forget`. The
/// fix calls forget unconditionally; this test guards that ordering by
/// stubbing a recall that always errors and asserting the palace ends
/// empty.
/// What: Drives `run_health_round_trip_inner` with a recall stub that
/// returns `Err(Recall(...))`, asserts the function surfaces a Recall
/// error, and then asserts the probe palace is empty.
/// Test: this test.
#[tokio::test]
async fn health_probe_cleans_up_on_recall_error() {
    let state = test_state();
    ensure_health_probe_palace(&state).expect("ensure_health_probe_palace");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(HEALTH_PROBE_PALACE))
        .expect("open probe palace");

    let result = run_health_round_trip_inner(handle.clone(), |_h, _q| async move {
        Err(HealthProbeError::Recall("simulated failure".to_string()))
    })
    .await;
    assert!(
        matches!(result, Err(HealthProbeError::Recall(_))),
        "recall error must surface as Recall; got {result:?}"
    );

    let drawer_count = handle.drawers.read().len();
    assert_eq!(
        drawer_count, 0,
        "probe palace must be empty after a recall error (got {drawer_count})"
    );
}

/// Issue #69 — `recall_entry_json` hoists the drawer's fields to the top
/// level so `content` is directly reachable.
///
/// Why: The recall API previously wrapped the drawer under a `"drawer"`
/// key, so clients scanning the top level for `content`/`tags` found
/// nothing and recall always looked empty. This locks the flattened shape
/// in place so the regression cannot silently return.
/// What: Builds a `RecallResult`, runs it through `recall_entry_json`, and
/// asserts `content`, `tags`, and `importance` are at the top level, that
/// `score`/`layer` sit alongside them, and that the old `drawer` wrapper
/// key is gone.
/// Test: this test.
#[test]
fn recall_entry_json_hoists_drawer_fields() {
    use trusty_common::memory_core::Drawer;

    let room = Uuid::new_v4();
    let mut drawer = Drawer::new(room, "the answer is 42");
    drawer.tags = vec!["source:kuzu".to_string()];
    drawer.importance = 0.7;

    let entry = recall_entry_json(RecallResult {
        drawer,
        score: 0.699,
        layer: 1,
    });

    // Content must be reachable WITHOUT a `drawer` wrapper (issue #69).
    assert_eq!(
        entry.get("content").and_then(|v| v.as_str()),
        Some("the answer is 42"),
        "content must be at the top level, got {entry:?}"
    );
    assert!(
        entry.get("drawer").is_none(),
        "the legacy `drawer` wrapper must not be present, got {entry:?}"
    );
    // Other drawer fields are hoisted too.
    assert_eq!(
        entry["importance"].as_f64().map(|f| (f * 10.0).round()),
        Some(7.0)
    );
    assert_eq!(
        entry["tags"][0].as_str(),
        Some("source:kuzu"),
        "tags must be hoisted, got {entry:?}"
    );
    // Ranking metadata sits alongside the hoisted fields.
    assert_eq!(entry["layer"].as_u64(), Some(1));
    assert!(
        entry["score"]
            .as_f64()
            .is_some_and(|s| (s - 0.699).abs() < 1e-6),
        "score must be preserved, got {entry:?}"
    );
}
