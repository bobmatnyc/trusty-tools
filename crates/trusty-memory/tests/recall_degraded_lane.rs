//! Issue #1970's degraded-recall guarantee: a recall issued while the embedder
//! is genuinely cold returns normally instead of blocking or erroring.
//!
//! Why these moved out of the lib-test binary (#4836): the recall handlers now
//! consult `shared_embedder_initialized()` before degrading, and that cell is
//! process-wide. In the lib-test binary `dispatch_remember_then_recall`
//! initialises it with the real singleton, so whether these tests exercised the
//! degraded lane depended on test execution order — they still passed, but only
//! because they asserted "does not error", which both lanes satisfy. A test that
//! silently stops testing its subject is worse than no test.
//!
//! What: their own integration binary, which never seeds or initialises an
//! embedder — `palace_create` and recall are the only operations, and neither
//! touches the embedder cell. Each test asserts the cold-embedder precondition
//! explicitly, so if that ever stops holding the test fails loudly rather than
//! quietly passing on the ready path.
//! Test: this IS the test module.

use serde_json::json;
use tempfile::TempDir;
use trusty_memory::engine::retrieval::shared_embedder_initialized;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::{AppState, DaemonReadiness};

/// A `Warming` state with a provably cold embedder.
///
/// Why: this is the precondition every test here depends on. Asserting it
/// (rather than assuming it) is what stops these tests from decaying back into
/// order-dependent no-ops — the exact failure mode that sent them to their own
/// binary.
/// What: fresh `AppState`, never `set_ready()`; asserts both the latch reads
/// `Warming` and the shared embedder cell is uninitialised.
/// Test: used by every test below.
fn cold_warming_state(tmp: &TempDir) -> AppState {
    let state = AppState::new(tmp.path().to_path_buf());
    assert!(
        !shared_embedder_initialized(),
        "precondition: the shared embedder must be cold, or these tests silently \
         exercise the ready path instead of the degraded one (#4836)"
    );
    assert_eq!(
        state.readiness(),
        DaemonReadiness::Warming,
        "precondition: a fresh AppState must start Warming"
    );
    state
}

/// Every result came from a non-vector lane.
///
/// Why: "did not error" is satisfied by both lanes, so it cannot distinguish
/// degraded recall from full recall. Layer is the observable that can: L2 and L3
/// exist only when the vector search ran.
/// What: asserts no result carries `layer` 2 or 3.
fn assert_no_vector_lane(result: &serde_json::Value, tool: &str) {
    let results = result["results"].as_array().expect("results array");
    for r in results {
        let layer = r["layer"].as_u64().unwrap_or_default();
        assert!(
            layer != 2 && layer != 3,
            "{tool}: returned a layer-{layer} hit while the embedder was cold — \
             the vector lane must not run on the degraded path"
        );
    }
}

/// `memory_recall` degrades rather than blocking or erroring (issue #1970).
#[tokio::test]
async fn recall_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = cold_warming_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": "warmtest-recall", "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");

    let result = dispatch_tool(
        &state,
        "memory_recall",
        json!({"palace": "warmtest-recall", "query": "test query"}),
    )
    .await
    .expect("memory_recall must not error while Warming (issue #1970)");

    assert!(result["results"].is_array());
    assert_no_vector_lane(&result, "memory_recall");
}

/// `memory_recall_deep` mirrors `memory_recall`'s degraded posture (issue #1970).
#[tokio::test]
async fn recall_deep_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = cold_warming_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": "warmtest-recall-deep", "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");

    let result = dispatch_tool(
        &state,
        "memory_recall_deep",
        json!({"palace": "warmtest-recall-deep", "query": "test query"}),
    )
    .await
    .expect("memory_recall_deep must not error while Warming (issue #1970)");

    assert!(result["results"].is_array());
    assert_no_vector_lane(&result, "memory_recall_deep");
}

/// `memory_recall_all` fans the degraded lane across every palace.
///
/// Regression guard for the gap originally fixed in #914 Part A, re-targeted at
/// graceful degradation instead of a hard error.
#[tokio::test]
async fn recall_all_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = cold_warming_state(&tmp);

    let result = dispatch_tool(
        &state,
        "memory_recall_all",
        json!({"q": "test query issued while warming up"}),
    )
    .await
    .expect("memory_recall_all must not error while Warming (issue #1970)");

    assert!(result["results"].is_array());
    assert_no_vector_lane(&result, "memory_recall_all");
}

/// Why (#8246 review): the embedder-warming path must demote stale snapshots
/// like the ready path does, or a recall issued during warm-up still puts an
/// old status line above the current ruling.
/// What: a cold, `Warming` state with the in-process BM25 lane, so the
/// lexical lane is the only query-scored lane. The snapshot repeats the query
/// terms and so out-scores the ruling on BM25; backdated 30 days it must rank
/// below it on both single-palace recall tools.
#[tokio::test]
async fn the_warming_path_demotes_stale_snapshots() {
    use trusty_memory::bm25_lane::Bm25Lane;
    use trusty_memory::engine::palace::{Drawer, PalaceId};

    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::new(tmp.path().join("bm25"));
    let state = cold_warming_state(&tmp).with_bm25_lane(lane.clone());
    let palace = "warmtest-rank";
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": palace, "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");

    let add = |text: &str, tag: &str, age_days: i64| {
        let mut d = Drawer::new(uuid::Uuid::new_v4(), text);
        d.tags = vec![tag.to_string()];
        d.created_at = chrono::Utc::now() - chrono::Duration::days(age_days);
        let id = d.id;
        handle.add_drawer(d);
        (id, text.to_string())
    };
    let snapshot = add("rust builder cap snapshot rust builder cap", "status", 30);
    let ruling = add("rust builder cap ruling", "bob-ruling", 1);
    for (id, text) in [&snapshot, &ruling] {
        lane.index(palace, &id.to_string(), text)
            .await
            .expect("index");
    }

    for tool in ["memory_recall", "memory_recall_deep"] {
        let result = dispatch_tool(
            &state,
            tool,
            json!({"palace": palace, "query": "rust builder cap", "top_k": 5}),
        )
        .await
        .expect("recall while warming");
        assert_no_vector_lane(&result, tool);
        let ids: Vec<&str> = result["results"]
            .as_array()
            .expect("results")
            .iter()
            .filter_map(|r| r["drawer_id"].as_str())
            .collect();
        let rank = |id: uuid::Uuid| ids.iter().position(|d| *d == id.to_string());
        let (s, r) = (rank(snapshot.0), rank(ruling.0));
        assert!(
            s.is_some() && r.is_some(),
            "{tool}: both recalled: {result:#}"
        );
        assert!(
            r < s,
            "{tool}: the ruling must outrank the stale snapshot: {result:#}"
        );
    }
}

/// Why (#9390): an expired Tier C slot is a stale fact, and no recall may
/// return it. The warming path hydrates BM25 hits straight from the drawer
/// table, which the L0/L1 and L2/L3 expiry filters never see.
/// What: a cold, `Warming` state with the in-process BM25 lane and two slots
/// matching the query, one expired an hour ago and one expiring in a day. On
/// all three recall tools the expired id is absent and the live id present;
/// `memory_list` still lists the expired drawer (excluded, not deleted).
#[tokio::test]
async fn an_expired_slot_is_not_returned_by_the_warming_path() {
    use trusty_memory::bm25_lane::Bm25Lane;
    use trusty_memory::engine::palace::{Drawer, PalaceId};

    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::new(tmp.path().join("bm25"));
    let state = cold_warming_state(&tmp).with_bm25_lane(lane.clone());
    let palace = "warmtest-expiry";
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": palace, "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");

    let now = chrono::Utc::now();
    let slot = |key: &str, text: &str, expires_at: chrono::DateTime<chrono::Utc>| {
        let mut d = Drawer::new(uuid::Uuid::new_v4(), text);
        d.fact_key = Some(key.to_string());
        d.expires_at = Some(expires_at);
        let id = d.id;
        handle.add_drawer(d);
        (id, text.to_string())
    };
    let expired = slot(
        "pr:1/state",
        "release head sha for pr one is abc123",
        now - chrono::Duration::hours(1),
    );
    let live = slot(
        "pr:2/state",
        "release head sha for pr two is def456",
        now + chrono::Duration::days(1),
    );
    for (id, text) in [&expired, &live] {
        lane.index(palace, &id.to_string(), text)
            .await
            .expect("index");
    }

    for tool in ["memory_recall", "memory_recall_deep", "memory_recall_all"] {
        let args = if tool == "memory_recall_all" {
            json!({"q": "release head sha", "top_k": 10})
        } else {
            json!({"palace": palace, "query": "release head sha", "top_k": 10})
        };
        let result = dispatch_tool(&state, tool, args)
            .await
            .expect("recall while warming");
        assert_no_vector_lane(&result, tool);
        let ids: Vec<&str> = result["results"]
            .as_array()
            .expect("results")
            .iter()
            .filter_map(|r| r["drawer_id"].as_str())
            .collect();
        assert!(
            !ids.contains(&expired.0.to_string().as_str()),
            "{tool}: an expired slot must not be recalled: {result:#}"
        );
        assert!(
            ids.contains(&live.0.to_string().as_str()),
            "{tool}: an unexpired slot must still be recalled: {result:#}"
        );
    }

    // Excluded, not deleted: the listing still carries the expired slot.
    let listed = dispatch_tool(&state, "memory_list", json!({"palace": palace}))
        .await
        .expect("memory_list");
    let row = listed["drawers"]
        .as_array()
        .expect("drawers")
        .iter()
        .find(|d| d["drawer_id"].as_str() == Some(expired.0.to_string().as_str()))
        .unwrap_or_else(|| panic!("expired slot still listed: {listed:#}"));
    assert_eq!(row["fact_key"], json!("pr:1/state"), "{listed:#}");
}

/// Why (#9390, PR A critic MEDIUM): the warming path asks BM25 for a fixed
/// window and drops expired hits after the fetch. Expired Tier C slots are
/// never deleted (ADR-0028 D6), so a pile of them that out-scores the live slot
/// fills that window and the live slot is never hydrated.
/// What: a cold, `Warming` state with the in-process BM25 lane, one live slot
/// and 40 expired slots on other keys whose shorter text out-scores it on BM25
/// (the window is 20 at `top_k` 10). All three recall tools must return the
/// live slot and no expired one.
#[tokio::test]
async fn recall_returns_the_live_slot_past_twenty_expired_ones_while_warming() {
    use trusty_memory::bm25_lane::Bm25Lane;
    use trusty_memory::engine::palace::{Drawer, PalaceId};

    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::new(tmp.path().join("bm25"));
    let state = cold_warming_state(&tmp).with_bm25_lane(lane.clone());
    let palace = "warmtest-refill";
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": palace, "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");

    let now = chrono::Utc::now();
    let mut slots = Vec::new();
    let mut slot = |key: String, text: String, expires_at: chrono::DateTime<chrono::Utc>| {
        let mut d = Drawer::new(uuid::Uuid::new_v4(), &text);
        d.fact_key = Some(key);
        d.expires_at = Some(expires_at);
        let id = d.id;
        handle.add_drawer(d);
        slots.push((id, text));
        id
    };
    let live = slot(
        "pr:999/state".to_string(),
        "release head sha for the live resume slot".to_string(),
        now + chrono::Duration::days(1),
    );
    let expired: Vec<uuid::Uuid> = (0..40)
        .map(|n| {
            slot(
                format!("pr:{n}/state"),
                format!("release head sha {n}"),
                now - chrono::Duration::hours(1),
            )
        })
        .collect();
    for (id, text) in &slots {
        lane.index(palace, &id.to_string(), text)
            .await
            .expect("index");
    }

    for tool in ["memory_recall", "memory_recall_deep", "memory_recall_all"] {
        let args = if tool == "memory_recall_all" {
            json!({"q": "release head sha", "top_k": 10})
        } else {
            json!({"palace": palace, "query": "release head sha", "top_k": 10})
        };
        let result = dispatch_tool(&state, tool, args)
            .await
            .expect("recall while warming");
        assert_no_vector_lane(&result, tool);
        let ids: Vec<String> = result["results"]
            .as_array()
            .expect("results")
            .iter()
            .filter_map(|r| r["drawer_id"].as_str().map(str::to_string))
            .collect();
        assert!(
            ids.contains(&live.to_string()),
            "{tool}: the live slot must be recalled past 40 expired ones: {result:#}"
        );
        assert!(
            !expired.iter().any(|id| ids.contains(&id.to_string())),
            "{tool}: an expired slot was recalled: {result:#}"
        );
    }
}

/// Why (#9719 critic): the warming path kept a drawer's L1 copy when the BM25
/// lane also hit it, so the hit reported layer 1 with its raw importance and
/// escaped the `min_score` floor, the opposite of the ready path.
/// What: a cold, `Warming` state with the in-process BM25 lane and one drawer
/// that is both in L1 and a BM25 hit. On both single-palace recall tools the
/// drawer appears once, at the lexical lane's layer (4), not layer 1.
#[tokio::test]
async fn a_warming_lane_hit_replaces_its_l1_copy() {
    use trusty_memory::bm25_lane::Bm25Lane;
    use trusty_memory::engine::palace::{Drawer, PalaceId};

    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::new(tmp.path().join("bm25"));
    let state = cold_warming_state(&tmp).with_bm25_lane(lane.clone());
    let palace = "warmtest-l1-dedup";
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": palace, "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");
    let text = "the ingest retry budget is three";
    let mut d = Drawer::new(uuid::Uuid::new_v4(), text);
    d.importance = 0.9;
    let id = d.id.to_string();
    handle.add_drawer(d);
    handle.refresh_l1();
    lane.index(palace, &id, text).await.expect("index");

    for tool in ["memory_recall", "memory_recall_deep"] {
        let result = dispatch_tool(
            &state,
            tool,
            json!({"palace": palace, "query": "ingest retry budget", "top_k": 5}),
        )
        .await
        .expect("recall while warming");
        let rows: Vec<&serde_json::Value> = result["results"]
            .as_array()
            .expect("results")
            .iter()
            .filter(|r| r["drawer_id"].as_str() == Some(id.as_str()))
            .collect();
        assert_eq!(rows.len(), 1, "{tool}: one row per drawer: {result:#}");
        assert_eq!(
            rows[0]["layer"],
            json!(4),
            "{tool}: the lane hit must replace its L1 copy: {result:#}"
        );
    }
}
