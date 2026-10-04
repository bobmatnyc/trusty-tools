//! End-to-end tests for the user-scope rulings leg (#9143): a ruling stored in
//! a configured rulings palace reaches a project palace's recall, and every
//! way that leg can fail leaves the project recall answering.
//!
//! Why: the leg is optional and crosses palaces, so its failure modes — an
//! absent, unreadable or hung palace, an odd env value, a scoped or
//! multi-tenant recall — must each be visible and harmless. With the leg unset
//! recall must be exactly what it was before #9143.
//! What: each test builds an `AppState` on a `TempDir` (see `recall_support`),
//! with neutral palace names, and recalls through `memory_recall`.
//! Test: this IS the test module.

mod recall_support;

use std::time::{Duration, Instant};

use recall_support::{create_palaces, rank_of, recall, recall_envelope, remember, state_with};
use serde_json::{json, Value};
use trusty_common::memory_core::palace::PalaceId;
use trusty_memory::tools::recall_rulings::parse_rulings_palaces;
use trusty_memory::AppState;
use uuid::Uuid;

const QUERY: &str = "issue titles name the symptom";

/// A project palace with two notes and a rulings palace with one ruling and
/// one non-ruling on the query's subject. Returns (ruling id, note id).
async fn seed(state: &AppState) -> (Uuid, Uuid) {
    for text in [
        "Quokkas are photogenic marsupials on Rottnest Island",
        "Basalt columns form when thick lava cools slowly",
    ] {
        remember(state, "project-a", text, &[], None).await;
    }
    let ruling = remember(
        state,
        "rulings-a",
        "issue titles name the symptom, never the proposed fix",
        &["bob-ruling", "standing-rule"],
        None,
    )
    .await;
    let note = remember(
        state,
        "rulings-a",
        "issue titles drafted on 2026-09-02 were long",
        &["note"],
        None,
    )
    .await;
    (ruling, note)
}

/// The project palace's own hits are all present.
fn assert_primary_survives(envelope: &Value) {
    let results = envelope["results"].as_array().expect("results");
    let has = |needle: &str| {
        results
            .iter()
            .any(|r| r["content"].as_str().is_some_and(|c| c.contains(needle)))
    };
    assert!(
        has("Quokkas") && has("Basalt"),
        "the project hits must survive a rulings failure: {envelope:#}"
    );
}

/// The `rulings_degraded` entries as (palace, reason).
fn degraded(envelope: &Value) -> Vec<(String, String)> {
    envelope["rulings_degraded"]
        .as_array()
        .unwrap_or_else(|| panic!("rulings_degraded must be set: {envelope:#}"))
        .iter()
        .map(|d| {
            (
                d["palace"].as_str().expect("palace").to_string(),
                d["reason"].as_str().expect("reason").to_string(),
            )
        })
        .collect()
}

fn plain(top_k: u64) -> Value {
    json!({ "palace": "project-a", "query": QUERY, "top_k": top_k })
}

/// Why (#9143 criteria 1-3): a ruling in the configured palace reaches a
/// project recall at L1; a non-ruling never crosses; an unconfigured palace
/// never contributes.
#[tokio::test]
async fn a_ruling_in_the_user_scope_palace_is_recalled_from_a_project_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a", "rulings-a", "other-a"], &["rulings-a"]).await;
    let (ruling, note) = seed(&state).await;
    let other = remember(&state, "other-a", "issue titles carry a key", &[], None).await;

    let envelope = recall_envelope(&state, plain(5)).await;
    let results = envelope["results"].as_array().expect("results").clone();
    let r = rank_of(&results, ruling).expect("the user-scope ruling is recalled");
    assert!(r < 3, "the ruling ranks in the top 3: {results:#?}");
    assert_eq!(
        results[r]["layer"],
        json!(1),
        "user-scope rulings join at L1"
    );
    assert_eq!(rank_of(&results, note), None, "a non-ruling never crosses");
    assert_eq!(
        rank_of(&results, other),
        None,
        "an unconfigured palace never contributes"
    );
    assert!(envelope.get("rulings_degraded").is_none(), "{envelope:#}");
}

/// Why (owner constraint, standalone): with the variable unset, recall is what
/// it was before #9143 — no extra palace opened, no new field, same results.
/// What: recall once before any rulings palace exists, then create one holding
/// an on-topic ruling and recall from a fresh, unconfigured `AppState` on the
/// same root. Same hits and scores; no `rulings_degraded`; the rulings palace
/// never opened.
#[tokio::test]
async fn an_unset_rulings_leg_changes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    remember(
        &state,
        "project-a",
        "Quokkas are photogenic marsupials",
        &[],
        None,
    )
    .await;
    remember(&state, "project-a", "Basalt columns form slowly", &[], None).await;
    let before = recall_envelope(&state, plain(5)).await;

    create_palaces(&state, &tmp, &["rulings-a"]).await;
    let ruling = remember(&state, "rulings-a", QUERY, &["bob-ruling"], None).await;
    let fresh = AppState::new(tmp.path().to_path_buf()).with_rulings_palaces(Vec::new());
    fresh.set_ready();
    let after = recall_envelope(&fresh, plain(5)).await;

    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&after), keys(&before), "no new envelope field");
    assert!(after.get("rulings_degraded").is_none(), "{after:#}");
    let hits = |v: &Value| -> Vec<(String, String)> {
        v["results"]
            .as_array()
            .expect("results")
            .iter()
            .map(|r| (r["drawer_id"].to_string(), r["score"].to_string()))
            .collect()
    };
    assert_eq!(hits(&before), hits(&after), "identical results");
    let results = after["results"].as_array().expect("results");
    assert_eq!(rank_of(results, ruling), None);
    assert!(
        fresh.registry.peek(&PalaceId::new("rulings-a")).is_none(),
        "an unset leg opens no rulings palace"
    );
}

/// Why (#9143 review, HIGH): a typo in the palace list returned no rulings
/// silently. It is now reported, the project hits survive, and the failed
/// palace is not retried on the next recall.
/// What: configure a palace that does not exist; recall reports it absent.
/// Create it with an on-topic ruling; within the retry window the next recall
/// still reports it (cached) and does not search it.
#[tokio::test]
async fn an_absent_rulings_palace_degrades_and_is_not_retried_at_once() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a", "rulings-a"], &["rulings-ghost"]).await;
    seed(&state).await;

    let first = recall_envelope(&state, plain(5)).await;
    assert_primary_survives(&first);
    let d = degraded(&first);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].0, "rulings-ghost");
    assert!(d[0].1.starts_with("absent"), "{d:?}");

    create_palaces(&state, &tmp, &["rulings-ghost"]).await;
    let late = remember(&state, "rulings-ghost", QUERY, &["bob-ruling"], None).await;
    let second = recall_envelope(&state, plain(5)).await;
    assert_primary_survives(&second);
    let d = degraded(&second);
    assert!(
        d[0].1.contains("cached"),
        "not retried within the window: {d:?}"
    );
    let results = second["results"].as_array().expect("results");
    assert_eq!(
        rank_of(results, late),
        None,
        "the cached failure is not searched"
    );
}

/// Why (#9143 review): an open failure other than absence is reported too.
/// What: evict a rulings palace from the registry and make its `palace.json`
/// unreadable, so the reopen fails with a permission error.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_rulings_palace_degrades_and_primary_hits_survive() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a", "rulings-a"], &["rulings-a"]).await;
    let (ruling, _) = seed(&state).await;
    state.registry.remove(&PalaceId::new("rulings-a"));
    let meta = state.data_root.join("rulings-a").join("palace.json");
    std::fs::set_permissions(&meta, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let _restore = Restore(meta.clone());
    assert!(
        std::fs::read(&meta).is_err(),
        "cannot exercise the open error: palace.json is still readable at mode 000"
    );

    let envelope = recall_envelope(&state, plain(5)).await;
    assert_primary_survives(&envelope);
    let d = degraded(&envelope);
    assert!(d[0].1.starts_with("open failed"), "{d:?}");
    let results = envelope["results"].as_array().expect("results");
    assert_eq!(rank_of(results, ruling), None);
}

/// Restores a file's mode on drop, so a failed assertion leaves a deletable tree.
struct Restore(std::path::PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o600));
    }
}

/// Why (#9143 review): blank entries, whitespace and repeats in the env value
/// must neither error nor search one palace twice.
/// What: the odd value names one absent palace twice among blanks: one
/// degraded entry, project hits intact. An all-blank value disables the leg:
/// no degraded field at all.
#[tokio::test]
async fn odd_env_values_degrade_once_and_blank_values_disable_the_leg() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a", "rulings-a"], &[]).await;
    seed(&state).await;

    let odd = state
        .clone()
        .with_rulings_palaces(parse_rulings_palaces(" , rulings-ghost ,rulings-ghost,, "));
    let envelope = recall_envelope(&odd, plain(5)).await;
    assert_primary_survives(&envelope);
    assert_eq!(degraded(&envelope).len(), 1, "{envelope:#}");

    let blank = state
        .clone()
        .with_rulings_palaces(parse_rulings_palaces("  , ,"));
    let envelope = recall_envelope(&blank, plain(5)).await;
    assert_primary_survives(&envelope);
    assert!(envelope.get("rulings_degraded").is_none(), "{envelope:#}");
}

/// Why (#9143 review): a hung rulings palace must not hang the recall.
/// What: a plain thread holds the rulings palace's drawer-table write lock,
/// so its search blocks. The recall runs as its own task on a multi-thread
/// runtime and must return within the bound, reporting the palace as timed
/// out, with the project hits intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_rulings_palace_returns_within_the_bound_as_degraded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bound = Duration::from_millis(300);
    let state = state_with(&tmp, &["project-a", "rulings-a"], &["rulings-a"])
        .await
        .with_rulings_timeout(bound);
    seed(&state).await;
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new("rulings-a"))
        .expect("open rulings palace");
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _hold = handle.drawers.write();
        locked_tx.send(()).expect("signal locked");
        let _ = release_rx.recv(); // released on send or on drop of the sender
    });
    locked_rx.recv().expect("the holder took the lock");

    let started = Instant::now();
    let task_state = state.clone();
    let task = tokio::spawn(async move { recall_envelope(&task_state, plain(5)).await });
    let joined = tokio::time::timeout(Duration::from_secs(10), task).await;
    let elapsed = started.elapsed();
    drop(release_tx);
    holder.join().expect("lock holder");

    let envelope = joined
        .expect("the recall must return while the rulings palace hangs")
        .expect("recall task");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert_primary_survives(&envelope);
    let d = degraded(&envelope);
    assert!(d[0].1.starts_with("timed out"), "{d:?}");
}

/// Why (#9143 review, scope leak): a room-scoped recall asked for one slice
/// of one palace; no other palace may contribute or even be tried.
/// What: an on-topic ruling and an absent palace are configured. A room-scoped
/// recall returns neither the ruling nor a `rulings_degraded` field.
#[tokio::test]
async fn a_room_scoped_recall_skips_the_rulings_leg() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        &tmp,
        &["project-a", "rulings-a"],
        &["rulings-a", "rulings-ghost"],
    )
    .await;
    let (ruling, _) = seed(&state).await;
    let envelope = recall_envelope(
        &state,
        json!({ "palace": "project-a", "query": QUERY, "top_k": 5, "room": "general" }),
    )
    .await;
    let results = envelope["results"].as_array().expect("results");
    assert_eq!(rank_of(results, ruling), None, "{envelope:#}");
    assert!(envelope.get("rulings_degraded").is_none(), "{envelope:#}");
}

/// Why (#9143 review): a multi-tenant daemon has no per-caller authorization
/// for the rulings palaces, so the leg fails closed there.
#[tokio::test]
async fn multi_tenant_mode_skips_the_rulings_leg() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        &tmp,
        &["project-a", "rulings-a"],
        &["rulings-a", "rulings-ghost"],
    )
    .await;
    let (ruling, _) = seed(&state).await;
    let mut tenant = state.clone();
    tenant.multi_tenant_mode = true;
    let envelope = recall_envelope(&tenant, plain(5)).await;
    let results = envelope["results"].as_array().expect("results");
    assert_eq!(rank_of(results, ruling), None, "{envelope:#}");
    assert!(envelope.get("rulings_degraded").is_none(), "{envelope:#}");
    let single = recall(&state, "project-a", QUERY, 5).await;
    assert!(
        rank_of(&single, ruling).is_some(),
        "control: single-tenant sees it"
    );
}

/// Why (#9143 review): one ruling stored in two rulings palaces appears once.
#[tokio::test]
async fn the_same_ruling_in_two_rulings_palaces_is_recalled_once() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        &tmp,
        &["project-a", "rulings-a", "rulings-b"],
        &["rulings-a", "rulings-b"],
    )
    .await;
    seed(&state).await;
    let text = "issue titles name the symptom, never the proposed fix";
    remember(&state, "rulings-b", text, &["bob-ruling"], None).await;
    let results = recall(&state, "project-a", QUERY, 6).await;
    let copies = results
        .iter()
        .filter(|r| r["content"].as_str() == Some(text))
        .count();
    assert_eq!(copies, 1, "{results:#?}");
}

/// Why (#9143 review, score scale): project hits carry an RRF bonus from the
/// project palace's BM25 lane; a ruling compared on the bare vector score was
/// on a different scale. Each rulings palace's hits now get the same fusion.
/// What: the same ruling recalled through a state with the BM25 lane armed
/// and indexed, and through a clone without the lane: the armed score is
/// higher by an RRF bonus (`1 / (60 + rank + 1)`, at least `1 / 70`).
#[tokio::test]
async fn rulings_get_the_same_bm25_fusion_as_project_hits() {
    use trusty_memory::bm25_lane::Bm25Lane;
    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::new(tmp.path().join("bm25"));
    // A clone shares the registry (same handles, closets and drawers), so the
    // lane is the only difference between the two recalls below.
    let bare_state = state_with(&tmp, &["project-a", "rulings-a"], &["rulings-a"]).await;
    let state = bare_state.clone().with_bm25_lane(lane.clone());
    let (ruling, _) = seed(&state).await;
    let text = "issue titles name the symptom, never the proposed fix";
    lane.index("rulings-a", &ruling.to_string(), text)
        .await
        .expect("index");

    let score_in = |envelope: &Value| {
        let results = envelope["results"].as_array().expect("results");
        let r = rank_of(results, ruling).expect("ruling recalled");
        results[r]["score"].as_f64().expect("score")
    };
    let fused = score_in(&recall_envelope(&state, plain(5)).await);
    let bare = score_in(&recall_envelope(&bare_state, plain(5)).await);
    assert!(
        fused - bare > 1.0 / 70.0,
        "the ruling must carry its palace's RRF bonus: fused {fused}, bare {bare}"
    );
}
