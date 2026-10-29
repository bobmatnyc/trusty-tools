//! #9881: a recall that serves a drawer records an access on it.
//!
//! Why: `Drawer::access_count` feeds the access boost that recall rank, dream
//! prune and fading read, but no production path ever raised it, so a drawer
//! recalled every day looked exactly as unused as one nobody had read.
//! What: drives the real MCP recall handlers through `dispatch_tool` against a
//! tempdir-rooted `AppState`, then drops the state and reads the drawer back
//! from a cold one, so only what reached disk can pass.
//! Test: this IS the test module.

use super::*;
use crate::engine::decay::DecayConfig;
use serde_json::Value;
use std::collections::HashMap;

/// Remember `text` in `palace` and return the new drawer's id.
async fn remember(state: &AppState, palace: &str, text: &str) -> String {
    let out = dispatch_tool(
        state,
        "memory_remember",
        json!({"palace": palace, "text": text, "room": "General"}),
    )
    .await
    .expect("memory_remember");
    out["drawer_id"].as_str().expect("drawer_id").to_string()
}

/// Ids of the hits a recall response served from a retrieval lane (layer >= 2).
fn lane_hits(out: &Value) -> Vec<(String, String)> {
    out["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter(|r| r["layer"].as_u64().expect("layer") >= 2)
        .map(|r| {
            let palace = r["palace_id"].as_str().unwrap_or_default().to_string();
            (
                palace,
                r["drawer_id"].as_str().expect("drawer_id").to_string(),
            )
        })
        .collect()
}

/// The persisted `(access_count, last_accessed_at.is_some())` of `drawer_id`,
/// read from a cold `AppState` over `root`.
fn persisted_access(root: &std::path::Path, palace: &str, drawer_id: &str) -> (u32, bool) {
    let cold = AppState::new(root.to_path_buf());
    cold.set_ready();
    let handle = crate::tools::helpers::open_palace_handle(&cold, palace).expect("reopen palace");
    let drawers = handle.drawers.read();
    let drawer = drawers
        .iter()
        .find(|d| d.id.to_string() == drawer_id)
        .expect("drawer survives the reopen");
    (drawer.access_count, drawer.last_accessed_at.is_some())
}

/// Why (#9881): the defect itself. Every recall left `access_count` at 0, so
/// the boost that keeps a used drawer out of dream prune never applied.
/// What: remembers one drawer, serves it through `memory_recall` twice and
/// `memory_recall_deep` once, counts how often it came back as a lane hit,
/// then drops the state and reads the drawer from disk. The persisted count
/// must equal the lane-hit count and `last_accessed_at` must be set.
/// Test: this function.
#[tokio::test]
async fn a_served_lane_hit_persists_its_access_count_across_reopen() {
    let (state, tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "acc"}))
        .await
        .expect("palace_create");
    let text = "The access counter rises each time recall serves this drawer";
    let id = remember(&state, "acc", text).await;

    let mut served = 0u32;
    for tool in ["memory_recall", "memory_recall", "memory_recall_deep"] {
        let out = dispatch_tool(
            &state,
            tool,
            json!({"palace": "acc", "query": text, "top_k": 3}),
        )
        .await
        .expect("recall");
        served += lane_hits(&out).iter().filter(|(_, d)| *d == id).count() as u32;
    }
    assert!(
        served >= 2,
        "the fixture must serve the drawer as a lane hit"
    );
    drop(state);

    let (count, stamped) = persisted_access(tmp.path(), "acc", &id);
    assert_eq!(count, served, "each served lane hit is one recorded access");
    assert!(stamped, "a served drawer carries its last access time");
}

/// Why (#9881, Q4): a cross-palace recall serves drawers from many palaces;
/// each access belongs to the palace that owns the drawer.
/// What: two palaces with one drawer each, one `memory_recall_all`. Every lane
/// hit it returns must be persisted, with count 1, in its own palace.
/// Test: this function.
#[tokio::test]
async fn recall_all_records_each_hit_in_its_own_palace() {
    let (state, tmp) = test_state();
    let text = "Cross palace recall records the access in the owning palace";
    let mut owned = HashMap::new();
    for palace in ["acc-a", "acc-b"] {
        dispatch_tool(&state, "palace_create", json!({"name": palace}))
            .await
            .expect("palace_create");
        owned.insert(remember(&state, palace, text).await, palace);
    }
    let out = dispatch_tool(&state, "memory_recall_all", json!({"q": text, "top_k": 10}))
        .await
        .expect("memory_recall_all");
    let hits = lane_hits(&out);
    for (palace, drawer) in &hits {
        assert_eq!(owned.get(drawer), Some(&palace.as_str()), "{out:#}");
    }
    assert_eq!(hits.len(), 2, "both palaces serve their drawer: {out:#}");
    drop(state);

    for (palace, drawer) in &hits {
        let (count, stamped) = persisted_access(tmp.path(), palace, drawer);
        assert_eq!((count, stamped), (1, true), "{palace}/{drawer}");
    }
}

/// Why: the access write is advisory; a failed one must never fail a recall.
/// What: the palace's next access write fails. Two recalls still succeed; the
/// failed flush keeps the accesses pending, and the next flush writes both.
/// Test: this function.
#[tokio::test]
async fn a_failed_access_write_never_fails_the_recall() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "acc-fail"}))
        .await
        .expect("palace_create");
    let text = "A failed access write leaves the recall answer untouched";
    let id = remember(&state, "acc-fail", text).await;
    let handle = state
        .registry
        .peek(&PalaceId::new("acc-fail"))
        .expect("resident handle");
    // No background flush may race the explicit ones below.
    handle
        .accesses
        .set_debounce(std::time::Duration::from_secs(600));
    handle.accesses.fail_next_write();
    let args = json!({"palace": "acc-fail", "query": text, "top_k": 3});
    let first = dispatch_tool(&state, "memory_recall", args.clone())
        .await
        .expect("recall survives a pending failure");
    assert!(lane_hits(&first).iter().any(|(_, d)| *d == id), "{first:#}");
    assert!(handle.flush_pending_accesses().await.is_err(), "seam fails");
    dispatch_tool(&state, "memory_recall", args)
        .await
        .expect("recall after a failed write");
    assert_eq!(handle.flush_pending_accesses().await.expect("retry"), 1);
    let on_disk = handle.kg.load_drawers().expect("load");
    let row = on_disk
        .iter()
        .find(|d| d.id.to_string() == id)
        .expect("row");
    assert_eq!(row.access_count, 2);
}

/// Poll a cold state until `drawer_id` shows an access in `palace`; the write
/// is detached, so the test waits for it with a bound.
async fn wait_for_persisted_access(
    root: &std::path::Path,
    palace: &str,
    drawer_id: &str,
) -> (u32, bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let seen = persisted_access(root, palace, drawer_id);
        if seen.0 > 0 || tokio::time::Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Why (#9881, M2): `scope: "all"` releases each batch's palaces before the
/// cut, so every served hit's palace is closed when the access is recorded.
/// The reopen must still land the count and leave the registry as it was.
/// What: one palace, released from the registry; `memory_recall_all` with
/// `scope: "all"` serves its drawer. The count reaches disk and the palace is
/// not resident afterwards.
/// Test: this function.
#[tokio::test]
async fn recall_all_scope_all_records_into_a_released_palace() {
    let (state, tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "acc-rel"}))
        .await
        .expect("palace_create");
    let text = "Scope all records the access in a palace the batch already released";
    let id = remember(&state, "acc-rel", text).await;
    state.registry.remove(&PalaceId::new("acc-rel"));
    let baseline = state.registry.list().len();

    let out = dispatch_tool(
        &state,
        "memory_recall_all",
        json!({"q": text, "top_k": 5, "scope": "all"}),
    )
    .await
    .expect("memory_recall_all scope all");
    assert!(lane_hits(&out).iter().any(|(_, d)| *d == id), "{out:#}");

    let (count, stamped) = wait_for_persisted_access(tmp.path(), "acc-rel", &id).await;
    assert_eq!((count, stamped), (1, true), "{out:#}");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while state.registry.list().len() != baseline && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(state.registry.list().len(), baseline, "back to baseline");
    assert!(state.registry.peek(&PalaceId::new("acc-rel")).is_none());
}

/// Why (#9881, M2): the reopen is synchronous redb work and its release
/// commits; run inline it parks the async worker and holds the recall
/// response behind a slow or contended open.
/// What: a closed palace whose open queue the test holds. Recording a served
/// ruling for it must return at once; once the queue is released the detached
/// reopen lands the count.
/// Test: this function.
#[tokio::test]
async fn a_released_palace_is_recorded_off_the_response_path() {
    let (state, tmp) = test_state();
    for name in ["acc-own", "acc-other"] {
        dispatch_tool(&state, "palace_create", json!({"name": name}))
            .await
            .expect("palace_create");
    }
    let id = remember(
        &state,
        "acc-own",
        "A served ruling is recorded in its owning palace without making the recall wait",
    )
    .await;
    let drawer = Uuid::parse_str(&id).expect("uuid");
    state.registry.remove(&PalaceId::new("acc-own"));
    let other = crate::tools::helpers::open_palace_handle(&state, "acc-other").expect("other");

    let gate = state.registry.open_lock_for_test(&PalaceId::new("acc-own"));
    let held = gate.lock();
    let started = std::time::Instant::now();
    let served = crate::tools::recall_access::ServedHits {
        lane: Vec::new(),
        rulings: vec![("acc-own".to_string(), drawer)],
    };
    served.record(&state, &other);
    let waited = started.elapsed();
    assert!(
        waited < std::time::Duration::from_secs(5),
        "the recall waited {waited:?} on a reopen"
    );
    drop(held);

    let (count, _) = wait_for_persisted_access(tmp.path(), "acc-own", &id).await;
    assert_eq!(count, 1, "the detached reopen lands the count");
}

/// Why (#9881): recall records accesses, but no response showed the count or
/// the boost it earns, so a caller could not tell a used drawer from an unused
/// one.
/// What: serves one drawer through `memory_recall` three times, then reads it
/// back through `memory_recall`, `memory_recall_all` and `memory_list`. Each
/// view must carry `access_count > 0`, `accumulated_boost > 0` and a
/// `last_accessed_at`. The in-memory count is bumped synchronously, so no
/// flush is needed.
/// Test: this function.
#[tokio::test]
async fn recall_and_list_responses_expose_access_count_and_boost() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "acc-show"}))
        .await
        .expect("palace_create");
    let text = "Recall responses show how often this drawer has been served";
    let id = remember(&state, "acc-show", text).await;
    let args = json!({"palace": "acc-show", "query": text, "top_k": 3});
    for _ in 0..3 {
        dispatch_tool(&state, "memory_recall", args.clone())
            .await
            .expect("recall");
    }

    let recall = dispatch_tool(&state, "memory_recall", args)
        .await
        .expect("recall");
    let all = dispatch_tool(&state, "memory_recall_all", json!({"q": text, "top_k": 5}))
        .await
        .expect("recall_all");
    let list = dispatch_tool(&state, "memory_list", json!({"palace": "acc-show"}))
        .await
        .expect("memory_list");
    for (name, out, key) in [
        ("memory_recall", &recall, "results"),
        ("memory_recall_all", &all, "results"),
        ("memory_list", &list, "drawers"),
    ] {
        let row = out[key]
            .as_array()
            .expect("rows")
            .iter()
            .find(|r| r["drawer_id"] == id.as_str())
            .unwrap_or_else(|| panic!("{name} omits the drawer: {out:#}"));
        assert!(
            row["access_count"].as_u64().unwrap_or(0) > 0,
            "{name} access_count: {row:#}"
        );
        assert!(
            row["accumulated_boost"].as_f64().unwrap_or(0.0) > 0.0,
            "{name} accumulated_boost: {row:#}"
        );
        assert!(
            row["last_accessed_at"].as_str().is_some(),
            "{name} last_accessed_at: {row:#}"
        );
    }
}

/// Why (#9881): the view test above only proves `> 0`. A recall row is
/// projected before its own access is recorded, and the boost must track the
/// count exactly; a loose check would pass a stale or double-counted value.
/// What: a fresh drawer is recalled twice, then listed. Recall 1 reads count 0
/// with a null `last_accessed_at` (its own access lands after the snapshot);
/// recall 2 reads 1 (recall 1's access); `memory_list` reads 2 (recall 2's
/// access, bumped in memory before the call returns). Every row's
/// `accumulated_boost` equals `count * DecayConfig::default().access_boost`;
/// two accesses stay under the boost cap, so the cap never clips.
/// Test: this function.
#[tokio::test]
async fn access_count_and_boost_follow_the_exact_recall_sequence() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "acc-seq"}))
        .await
        .expect("palace_create");
    let text = "The first recall of a fresh drawer reports zero prior accesses";
    let id = remember(&state, "acc-seq", text).await;
    let args = json!({"palace": "acc-seq", "query": text, "top_k": 3});
    let first = dispatch_tool(&state, "memory_recall", args.clone())
        .await
        .expect("first recall");
    let second = dispatch_tool(&state, "memory_recall", args)
        .await
        .expect("second recall");
    let list = dispatch_tool(&state, "memory_list", json!({"palace": "acc-seq"}))
        .await
        .expect("memory_list");

    let per_access = DecayConfig::default().access_boost;
    for (name, out, key, count, stamped) in [
        ("first recall", &first, "results", 0u64, false),
        ("second recall", &second, "results", 1, true),
        ("memory_list", &list, "drawers", 2, true),
    ] {
        let row = out[key]
            .as_array()
            .expect("rows")
            .iter()
            .find(|r| r["drawer_id"] == id.as_str())
            .unwrap_or_else(|| panic!("{name} omits the drawer: {out:#}"));
        assert_eq!(row["access_count"].as_u64(), Some(count), "{name}: {row:#}");
        assert_eq!(
            row["last_accessed_at"].as_str().is_some(),
            stamped,
            "{name}: {row:#}"
        );
        let boost = row["accumulated_boost"].as_f64().expect("boost") as f32;
        assert!(
            (boost - count as f32 * per_access).abs() < 1e-5,
            "{name} boost {boost} != {count} x {per_access}"
        );
    }
}
