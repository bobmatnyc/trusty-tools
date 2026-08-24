//! Tests for the `GET /health` handler and health probe helpers.
use super::super::health::{
    ensure_health_probe_palace, run_health_round_trip_inner, seed_probe_sentinel_if_absent,
    HealthProbeError, PROBE_SENTINEL_CONTENT,
};
use super::super::router;
use super::super::HEALTH_PROBE_PALACE;
use super::test_state;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::{PalaceHandle, RecallResult};
use trusty_common::memory_core::store::kg::KnowledgeGraph;
use trusty_common::memory_core::store::vector::UsearchStore;
use uuid::Uuid;

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

/// Issue #1101 — `GET /health` without `?probe=true` returns `status: "ok"`
/// immediately (no ONNX round-trip). Runs in the default (non-ignored) matrix.
///
/// Why: LBs poll `/health` every 1 s; the ONNX round-trip was too expensive
/// for this cadence. The fix makes the probe opt-in via `?probe=true`.
/// What: Drives `/health` without params; asserts 200, `status=ok`, `version`
/// present, and no `detail` field — all without the embedder.
/// Test: this test.
#[tokio::test]
async fn health_endpoint_cheap_by_default() {
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
    assert_eq!(v["status"], "ok", "cheap health must report ok; got {v:?}");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        v.get("detail").is_none() || v["detail"].is_null(),
        "cheap health must not carry detail; got {v:?}"
    );
}

// ---- Issue #4001: /health must observe the worker pool ----

/// Why (issue #4001): doctor cannot report what `/health` never tells it. An
/// idle daemon must publish a worker block so an out-of-process probe can see
/// that it positively observed zero outstanding work — as opposed to a
/// pre-#4001 daemon, where the absence of the block means "unknown".
/// What: asserts the cheap path carries `worker.in_flight`/`worker.wedged`,
/// and omits `oldest_age_secs` when idle.
/// Test: this test.
#[tokio::test]
async fn health_reports_idle_worker_pool() {
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
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["worker"]["in_flight"], 0, "idle pool; got {v:?}");
    assert_eq!(v["worker"]["wedged"], false, "idle pool; got {v:?}");
    assert!(
        v["worker"].get("oldest_age_secs").is_none(),
        "an idle pool has no age to report; got {v:?}"
    );
}

/// Why (issue #4001): THE regression lock. During the #3992 incident the HTTP
/// listener answered normally while six threads sat parked and a
/// `memory_remember` had been hung ~1800 s, and `/health` reported `"ok"` —
/// which is what let both doctors report HEALTHY. Here the listener is equally
/// live; the only difference is that an operation has been outstanding past
/// the wedge threshold. `/health` must say so.
/// What: registers a tracked operation and drives this daemon's wedge
/// threshold to zero so the test is deterministic and instant (no sleeping for
/// minutes). The threshold is per-`AppState`, not an env var, so this test
/// cannot race the sibling test below.
/// Test: this test.
#[tokio::test]
async fn health_reports_wedged_worker_pool() {
    let mut state = test_state();
    state.wedge_threshold = std::time::Duration::ZERO;
    // Hold an in-flight operation open across the request, exactly as a
    // wedged `open_palace_handle` would.
    let _stuck = state.worker_liveness.track();
    // Let at least one millisecond elapse so the age strictly exceeds zero.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let app = router().with_state(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(
        v["worker"]["wedged"], true,
        "a stuck operation past the threshold must read as wedged; got {v:?}"
    );
    assert_eq!(
        v["status"], "wedged",
        "top-level status must NOT be ok while workers are stuck; got {v:?}"
    );
    assert_ne!(
        v["status"], "ok",
        "this is the #3992 false positive; got {v:?}"
    );
    assert_eq!(v["worker"]["in_flight"], 1, "got {v:?}");
    assert!(
        v["detail"].as_str().unwrap_or_default().contains("wedged")
            || v["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("not making progress"),
        "detail must explain the wedge; got {v:?}"
    );
}

/// Why (issue #4001): the wedge signal must clear on its own. If a completed
/// operation left the gauge tripped, the fix would trade a false positive for
/// a sticky one and operators would learn to ignore it.
/// What: tracks and drops an operation, then asserts `/health` is back to ok.
/// Test: this test.
#[tokio::test]
async fn health_wedge_signal_clears_when_work_completes() {
    let mut state = test_state();
    state.wedge_threshold = std::time::Duration::ZERO;
    {
        let _stuck = state.worker_liveness.track();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    } // operation completes here

    let app = router().with_state(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(v["worker"]["wedged"], false, "got {v:?}");
    assert_eq!(v["status"], "ok", "got {v:?}");
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
/// zero ephemeral drawers (only the permanent sentinel, if already seeded).
///
/// Why: The probe must clean up the ephemeral round-trip drawer on every
/// success path. If the forget step were ever skipped silently, the probe
/// palace would grow unbounded over time (the original symptom was ~1,420
/// leaked drawers in `localLLM`). This test pins the post-condition
/// without requiring the heavy ONNX recall — it exercises
/// `run_health_round_trip_inner` with a recall stub that returns a
/// synthetic hit matching the probe drawer id.
/// What: Provisions the probe palace (no sentinel yet — fresh palace),
/// opens its handle, runs the inner round-trip with a stubbed recall that
/// returns the last drawer, and asserts the handle's drawer count drops
/// back to zero after cleanup.
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
/// fix calls forget unconditionally; this test guards that ordering.
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

/// Issue #1142 — `seed_probe_sentinel_if_absent` self-heals the sentinel
/// drawer when the probe palace exists but is empty (post-migration wipe).
///
/// Why: After a redb v2→v3 migration the palace directory and `palace.json`
/// survive but the internal vector/drawer stores are reset to empty. Before
/// this fix the probe would open the empty palace, store a probe drawer,
/// recall via the ANN index (which also reset), find nothing, and report
/// `ProbeMissing` on every deep probe — making `/health?probe=true`
/// permanently degraded. The fix calls `seed_probe_sentinel_if_absent` from
/// `run_health_round_trip`, which seeds a sentinel on the first probe and
/// returns `Ok(())` immediately so the caller never sees a false failure.
/// What: Creates the probe palace with an empty drawer store (simulates the
/// post-migration state), calls `seed_probe_sentinel_if_absent` directly,
/// and asserts (a) it returns `Ok(true)` (was absent, now seeded), (b) the
/// palace holds exactly one drawer with the expected sentinel content, and
/// (c) a second call returns `Ok(false)` (already present — idempotent).
/// Test: this test (issue #1142 regression guard).
#[tokio::test]
async fn health_probe_self_heals_after_migration_wipe() {
    let state = test_state();
    // Simulate post-migration state: palace exists but drawers are empty.
    ensure_health_probe_palace(&state).expect("create probe palace");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(HEALTH_PROBE_PALACE))
        .expect("open probe palace");
    assert_eq!(
        handle.drawers.read().len(),
        0,
        "palace must be empty before self-heal"
    );

    // First call: sentinel is absent → seed it and return Ok(true).
    let seeded = seed_probe_sentinel_if_absent(&handle)
        .await
        .expect("seed_probe_sentinel_if_absent");
    assert!(
        seeded,
        "first call must report that the sentinel was seeded"
    );

    {
        let drawers = handle.drawers.read();
        assert_eq!(
            drawers.len(),
            1,
            "sentinel must be seeded when palace is empty (issue #1142)"
        );
        assert_eq!(
            drawers[0].content(),
            PROBE_SENTINEL_CONTENT,
            "seeded drawer must carry the well-known sentinel content"
        );
    }

    // Second call: sentinel already present → return Ok(false) (idempotent).
    let seeded_again = seed_probe_sentinel_if_absent(&handle)
        .await
        .expect("seed_probe_sentinel_if_absent idempotent");
    assert!(
        !seeded_again,
        "second call must report sentinel already present"
    );
    let drawer_count = handle.drawers.read().len();
    assert_eq!(
        drawer_count, 1,
        "seed_probe_sentinel_if_absent must be idempotent (got {drawer_count})"
    );
}

// ---- Issue #4911: /health must show a palace it refused to open ----

/// Why (issue #4911): once a read-only open REFUSES an incompatible store
/// instead of recreating it, the palace's bytes survive but nothing tells an
/// operator the palace is there — it is missing from `palace_list` and missing
/// from the handle cache, which reads exactly like deletion. Recording the skip
/// in the registry is only half the fix; without a reader it is bookkeeping no
/// human can reach. `/health` is that reader.
/// What: records an unopenable palace on the state's registry (the same call
/// `AppState::load_palaces_from_disk` makes when an open fails), drives the
/// cheap `/health` path, and asserts the id and the reason both surface.
/// Test: this test.
#[tokio::test]
async fn health_reports_unopenable_palaces() {
    let state = test_state();
    state.registry.record_unopenable(
        PalaceId::new("stale-format"),
        "open vector store for stale-format: incompatible on-disk format".to_string(),
    );
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
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();

    let listed = v["unopenable_palaces"]
        .as_array()
        .unwrap_or_else(|| panic!("/health must list unopenable palaces; got {v:?}"));
    assert_eq!(listed.len(), 1, "exactly one palace was refused; got {v:?}");
    assert_eq!(listed[0]["id"], "stale-format");
    assert!(
        listed[0]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("incompatible on-disk format")),
        "the reason must reach the operator, not just the id; got {v:?}"
    );
}

/// Why (issue #4911): monitors poll `/health` every second, so the healthy
/// payload must not grow a field that is empty in every normal response.
/// What: drives `/health` on a daemon with no refused palace and asserts the
/// key is absent entirely.
/// Test: this test.
#[tokio::test]
async fn health_omits_unopenable_palaces_when_none() {
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
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        v.get("unopenable_palaces").is_none(),
        "a healthy daemon's payload must be unchanged; got {v:?}"
    );
}

// ---- Issue #6217: /health must show a palace serving a partial corpus ----

/// Build an in-memory palace handle for `id`, flagged degraded or not.
///
/// Why: the degrade is produced deep inside `PalaceHandle::open_with_intent`,
/// which needs a corrupt redb `DRAWERS` table to reach. These tests exercise the
/// reader, not the producer — #6201 already covers the producer — so they set
/// the flag the open path would have set and assert what `/health` does with it.
/// What: a handle over throwaway vector and KG stores in a leaked tempdir (the
/// same `mem::forget` lifetime trick `test_state` uses), with
/// `drawer_load_degraded` set as asked.
/// Test: used by the four `health_*drawer_degraded*` tests below.
fn degraded_handle(id: &str, degraded: bool) -> std::sync::Arc<PalaceHandle> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let vs = UsearchStore::new(dir.join("idx.usearch"), 384).expect("vector store");
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).expect("kg");
    let mut handle = PalaceHandle::new(PalaceId::new(id), String::new(), vs, kg);
    handle.drawer_load_degraded = degraded;
    std::sync::Arc::new(handle)
}

/// Drive the cheap (non-probe) `/health` path and return the parsed body.
async fn health_body(state: crate::AppState) -> Value {
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
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Why (issue #6217): #6201 made a partial drawer load observable on the handle
/// and wired it to nothing, so a corpus with holes reached an operator only as a
/// startup `warn!` line. `/health` is the reader that closes that gap.
/// What: registers a degraded handle, drives the cheap `/health` path, and
/// asserts the palace is named. Also pins the deliberate decision that a partial
/// corpus does NOT flip `status`: it is durable state a restart cannot repair,
/// and `status: "degraded"` already means the round-trip probe just failed.
/// Test: this test.
#[tokio::test]
async fn health_reports_drawer_degraded_palace() {
    let state = test_state();
    state
        .registry
        .register_arc(degraded_handle("corrupt", true));

    let v = health_body(state).await;

    let listed = v["drawer_degraded_palaces"]
        .as_array()
        .unwrap_or_else(|| panic!("/health must name the degraded palace; got {v:?}"));
    assert_eq!(listed.len(), 1, "exactly one palace is degraded; got {v:?}");
    assert_eq!(listed[0], "corrupt");
    assert_eq!(
        v["status"], "ok",
        "a partial corpus is reported, not escalated to a failing status; got {v:?}"
    );
}

/// Why (issue #6217): a flag that is always set reports nothing. This is the
/// test that catches an always-true wiring, and the one that keeps a healthy
/// daemon's payload from growing a field present in every response.
/// What: registers a handle with `drawer_load_degraded == false` and asserts the
/// key is absent entirely.
/// Test: this test.
#[tokio::test]
async fn health_omits_drawer_degraded_when_all_healthy() {
    let state = test_state();
    state
        .registry
        .register_arc(degraded_handle("intact", false));

    let v = health_body(state).await;

    assert!(
        v.get("drawer_degraded_palaces").is_none(),
        "a fully-loaded palace must not be reported as degraded; got {v:?}"
    );
}

/// Why (issue #6217): "the corpus is degraded" and "one of three palaces is
/// degraded" call for different operator responses, so the payload must
/// distinguish them rather than collapsing to a single boolean.
/// What: registers three handles, one degraded, and asserts only that one is
/// named while the two intact palaces are absent.
/// Test: this test.
#[tokio::test]
async fn health_drawer_degraded_names_only_the_degraded_palace() {
    let state = test_state();
    state
        .registry
        .register_arc(degraded_handle("intact-a", false));
    state.registry.register_arc(degraded_handle("holey", true));
    state
        .registry
        .register_arc(degraded_handle("intact-b", false));

    let v = health_body(state).await;

    let listed = v["drawer_degraded_palaces"]
        .as_array()
        .unwrap_or_else(|| panic!("/health must name the degraded palace; got {v:?}"));
    assert_eq!(
        listed.len(),
        1,
        "only one of three palaces is degraded; got {v:?}"
    );
    assert_eq!(listed[0], "holey");
}

/// Why (issue #6217): monitors poll `/health` every second, so this check must
/// read the handle cache and never open a palace. Opening one to ask whether its
/// drawers loaded would put disk I/O — and the redb open lock — on the liveness
/// path, which is a worse failure than under-reporting an idle-evicted palace.
/// What: fills a capacity-2 registry with two resident handles so a third palace,
/// which exists on disk, is evicted. Asserts `/health` names only the resident
/// degraded palace and leaves both residents in the cache. Opening the on-disk
/// palace would have to evict a resident to make room, so two surviving residents
/// and an absent third is the proof that no open happened — an assertion the
/// weaker "cache did not grow" form misses, because a full open sweep also ends
/// at size 2.
/// Test: this test.
#[tokio::test]
async fn health_drawer_degraded_check_opens_no_palace() {
    use trusty_common::memory_core::{Palace, PalaceRegistry};

    let mut state = test_state();
    let registry = PalaceRegistry::with_max_open(2);
    let on_disk = PalaceId::new("on-disk-only");
    registry
        .create_palace(
            &state.data_root,
            Palace {
                id: on_disk.clone(),
                name: "on-disk-only".to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: state.data_root.join("on-disk-only"),
            },
        )
        .unwrap_or_else(|e| panic!("create_palace(on-disk-only) failed: {e:#}"));
    // Two residents fill the capacity-2 cache and push the on-disk palace out.
    registry.register_arc(degraded_handle("resident-intact", false));
    registry.register_arc(degraded_handle("resident-degraded", true));
    assert!(
        registry.peek(&on_disk).is_none(),
        "precondition: the on-disk palace must be evicted before /health runs"
    );
    state.registry = std::sync::Arc::new(registry);
    let registry = std::sync::Arc::clone(&state.registry);

    let v = health_body(state).await;

    let listed = v["drawer_degraded_palaces"]
        .as_array()
        .unwrap_or_else(|| panic!("/health must name the resident degraded palace; got {v:?}"));
    assert_eq!(listed.len(), 1, "only the resident is degraded; got {v:?}");
    assert_eq!(listed[0], "resident-degraded");
    assert!(
        registry.peek(&PalaceId::new("resident-intact")).is_some(),
        "/health must not evict a resident handle to inspect a palace on disk; got {v:?}"
    );
    assert!(
        registry.peek(&PalaceId::new("resident-degraded")).is_some(),
        "/health must not evict a resident handle to inspect a palace on disk; got {v:?}"
    );
    assert!(
        registry.peek(&on_disk).is_none(),
        "/health must not open a palace that is not already resident; got {v:?}"
    );
    assert_eq!(registry.len(), 2, "/health must not grow the cache");
}
