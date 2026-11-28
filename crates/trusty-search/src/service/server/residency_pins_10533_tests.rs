//! #10533: tm's active-project set drives the residency sweep.
//!
//! Why: the acceptance criteria of #10533 — an active-set member is never
//! parked by the cap, a non-member leaves only after the grace period with no
//! query, a stale or absent set changes nothing, and a `Running` reindex still
//! blocks a park. Every clock is injected: `run_residency_sweep_tick_at`
//! takes `now` and `now_unix`, and `ResidencyPins` takes `Instant`s.
//!
//! Isolation: `TRUSTY_DATA_DIR` points at a private tempdir and every
//! env-touching test is `#[serial_test::serial]`, as in `residency_sweep_tests`.
//!
//! Test: `cargo test -p trusty-search -- residency_pins_10533`

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexRegistry};
use crate::service::persistence::save_index_registry_at;
use crate::service::reindex::ReindexProgress;
use crate::service::server::residency_pins::{registered_in, ResidencyPins};
use crate::service::server::residency_pull::{
    apply_pull, prewake_targets, pull_interval, ACTIVE_SET_PULL_TIMEOUT,
};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::RwLock as TokioRwLock;
use trusty_common::machine_tier::MemoryTier;
use trusty_common::residency::{ActiveProject, ActiveProjectSet};

const STALE: Duration = Duration::from_secs(600);
const GRACE: Duration = Duration::from_secs(120);
/// The injected wall clock every tick in this file runs at.
const NOW_UNIX: u64 = 1_000_000;

fn bare_handle(id: &str) -> IndexHandle {
    let index_id = IndexId::new(id.to_string());
    let root = PathBuf::from(format!("/tmp/residency-pins-10533-{id}"));
    let indexer = Arc::new(TokioRwLock::new(CodeIndexer::new(id, &root)));
    IndexHandle::bare(index_id, indexer, root)
}

/// Every id counts as registered: the pure `ResidencyPins` tests.
fn any(_: &str) -> bool {
    true
}

fn entry(id: &str, last_queried: Option<u64>) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: PathBuf::from(format!("/tmp/residency-pins-10533-{id}")),
        last_queried_unix: last_queried,
        ..Default::default()
    }
}

/// A set naming `ids` as one active project's index ids. Built by mutation:
/// `ActiveProjectSet` is `#[non_exhaustive]`.
fn set_of(ids: &[&str]) -> ActiveProjectSet {
    let mut project = ActiveProject::default();
    project.index_ids = ids.iter().map(|s| s.to_string()).collect();
    let mut set = ActiveProjectSet::default();
    set.projects = vec![project];
    set
}

fn isolate_and_seed_toml(entries: &[PersistedIndex]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", tmp.path()) };
    let path = crate::service::persistence::indexes_toml_path().expect("indexes_toml_path");
    save_index_registry_at(&path, entries).expect("seed indexes.toml");
    tmp
}

fn clear_env() {
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_MAX_RESIDENT_INDEXES");
    }
}

/// A Degraded-tier state (cap 2) with explicit pin windows and `entries`
/// registered resident.
fn resident_state(entries: &[PersistedIndex]) -> SearchAppState {
    let mut state = SearchAppState::new(IndexRegistry::new());
    state.machine_tier = MemoryTier::Degraded;
    state.residency_pins = Arc::new(ResidencyPins::new(STALE, GRACE));
    for e in entries {
        state.registry.register(bare_handle(&e.id));
    }
    state
}

fn resident(state: &SearchAppState, id: &str) -> bool {
    state.registry.get(&IndexId::new(id.to_string())).is_some()
}

/// Set `id`'s indexer idle clock to read `idle` (#10533).
async fn idle_for(state: &SearchAppState, id: &str, idle: Duration) {
    let handle = state
        .registry
        .get(&IndexId::new(id.to_string()))
        .expect("resident handle");
    handle
        .indexer
        .write()
        .await
        .backdate_activity_for_test(idle);
}

/// Observe `set` with `state`'s own registered-id predicate.
fn observe(state: &SearchAppState, set: ActiveProjectSet, now: Instant) {
    state
        .residency_pins
        .observe(set, now, &registered_in(state));
}

/// AC1, pure: the cap never returns a pinned entry, even the coldest, and the
/// pinned residents use up their share of the cap.
#[test]
fn ids_to_park_never_chooses_an_active_set_member() {
    let entries = vec![
        entry("member", Some(1)),
        entry("warm", Some(200)),
        entry("hot", Some(300)),
    ];
    let pinned: HashSet<String> = ["member".to_string()].into_iter().collect();
    let parked: Vec<String> = crate::service::lazy_loader::ids_to_park(entries, 2, &pinned)
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(parked, vec!["warm".to_string()]);
}

/// AC1, through the tick: with a trusted set naming the coldest index, the
/// cap parks a non-member instead. On the base the coldest index parked.
#[tokio::test]
#[serial_test::serial]
async fn sweep_keeps_an_active_set_member_resident_past_the_cap() {
    clear_env();
    let t0 = Instant::now();
    // All three were queried 10 s ago, so the grace stage parks nothing.
    let entries = vec![
        entry("member", Some(NOW_UNIX - 10)),
        entry("warm", Some(NOW_UNIX - 9)),
        entry("hot", Some(NOW_UNIX - 8)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);
    let state = resident_state(&entries);
    observe(&state, set_of(&["member"]), t0);

    run_residency_sweep_tick_at(&Arc::new(state.clone()), t0, NOW_UNIX, |_| {}).await;

    assert!(resident(&state, "member"), "an active-set member must stay");
    assert!(
        resident(&state, "hot"),
        "the hottest non-member keeps the slot"
    );
    assert!(!resident(&state, "warm"), "the cap parks a non-member");
    clear_env();
}

/// AC2: under the cap, a non-member parks only once it has been absent AND
/// unqueried for the grace period. A recently queried non-member stays.
#[tokio::test]
#[serial_test::serial]
async fn non_member_parks_only_after_the_grace_period_with_no_query() {
    clear_env();
    let t0 = Instant::now();
    let entries = vec![
        entry("member", Some(1)),
        entry("idle", Some(NOW_UNIX - 3_600)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);
    let state = Arc::new(resident_state(&entries));
    observe(&state, set_of(&["member"]), t0);
    idle_for(&state, "idle", Duration::from_secs(3_600)).await;

    let just_before = t0 + GRACE - Duration::from_secs(1);
    run_residency_sweep_tick_at(&state, just_before, NOW_UNIX, |_| {}).await;
    assert!(resident(&state, "idle"), "inside the grace period it stays");

    // A query inside the grace window keeps a non-member resident.
    state
        .last_queried_write_cache
        .insert(IndexId::new("idle".to_string()), NOW_UNIX - 30);
    run_residency_sweep_tick_at(&state, t0 + GRACE, NOW_UNIX, |_| {}).await;
    assert!(resident(&state, "idle"), "a query 30 s ago keeps it");

    state
        .last_queried_write_cache
        .remove(&IndexId::new("idle".to_string()));
    run_residency_sweep_tick_at(&state, t0 + GRACE, NOW_UNIX, |_| {}).await;
    assert!(
        !resident(&state, "idle"),
        "absent and unqueried past grace parks"
    );
    assert!(resident(&state, "member"), "the member never parks");
    clear_env();
}

/// AC3: a set that went stale, and a set never fetched, both leave the tick
/// exactly as before #10533 — no grace park under the cap, and the cap ranks
/// by recency alone, so a former member is the one parked.
#[tokio::test]
#[serial_test::serial]
async fn stale_or_absent_set_sweep_behaves_as_today() {
    for observed in [true, false] {
        clear_env();
        let t0 = Instant::now();
        let under_cap = vec![
            entry("member", Some(NOW_UNIX - 3_600)),
            entry("idle", Some(NOW_UNIX - 3_600)),
        ];
        let _tmp = isolate_and_seed_toml(&under_cap);
        let state = Arc::new(resident_state(&under_cap));
        if observed {
            observe(&state, set_of(&["member"]), t0);
        }
        let later = t0 + STALE + Duration::from_secs(1);
        run_residency_sweep_tick_at(&state, later, NOW_UNIX, |_| {}).await;
        assert!(
            resident(&state, "idle"),
            "no grace park on an untrusted set"
        );

        state.registry.register(bare_handle("hot"));
        let over_cap = vec![
            entry("member", Some(100)),
            entry("idle", Some(200)),
            entry("hot", Some(300)),
        ];
        let _tmp = isolate_and_seed_toml(&over_cap);
        run_residency_sweep_tick_at(&state, later, NOW_UNIX, |_| {}).await;
        assert!(
            !resident(&state, "member"),
            "recency alone picks the coldest"
        );
        assert!(resident(&state, "idle") && resident(&state, "hot"));
    }
    clear_env();
}

/// AC4: a non-member past its grace period still stays while its reindex runs.
#[tokio::test]
#[serial_test::serial]
async fn running_reindex_blocks_an_active_set_grace_park() {
    clear_env();
    let t0 = Instant::now();
    let entries = vec![entry("member", Some(1)), entry("busy", Some(1))];
    let _tmp = isolate_and_seed_toml(&entries);
    let state = Arc::new(resident_state(&entries));
    observe(&state, set_of(&["member"]), t0);
    let running = Arc::new(ReindexProgress::new());
    running.status.store(ReindexStatus::Running);
    state
        .reindex_progress
        .insert(IndexId::new("busy".to_string()), running);

    run_residency_sweep_tick_at(&state, t0 + GRACE * 2, NOW_UNIX, |_| {}).await;

    assert!(
        resident(&state, "busy"),
        "a Running reindex blocks the park"
    );
    clear_env();
}

/// The empty set is "no evictions" (design section 3), like stale and absent.
#[test]
fn members_is_none_for_a_stale_absent_or_empty_set() {
    let pins = ResidencyPins::new(STALE, GRACE);
    let t0 = Instant::now();
    assert_eq!(pins.members(t0, &any), None, "never fetched");

    pins.observe(ActiveProjectSet::default(), t0, &any);
    assert_eq!(pins.members(t0, &any), None, "a set with no index ids");
    assert_eq!(pins.absent_for("x", t0, &any), None);

    pins.observe(set_of(&["a"]), t0, &any);
    assert!(pins.members(t0, &any).is_some_and(|m| m.contains("a")));
    assert_eq!(pins.members(t0 + STALE, &any), None, "stale");
}

/// After a stale gap the grace clock restarts at the first trusted pull, so a
/// recovered producer cannot cause an immediate mass park.
#[test]
fn absence_restarts_after_a_stale_gap() {
    let pins = ResidencyPins::new(STALE, GRACE);
    let t0 = Instant::now();
    pins.observe(set_of(&["a"]), t0, &any);
    assert_eq!(pins.absent_for("b", t0 + GRACE, &any), Some(GRACE));

    let recovered = t0 + STALE * 2;
    pins.observe(set_of(&["a"]), recovered, &any);
    assert_eq!(pins.absent_for("b", recovered, &any), Some(Duration::ZERO));
    assert_eq!(
        pins.absent_for("a", recovered, &any),
        None,
        "a member is never absent"
    );
}

/// The pull ticker body: a good pull pins, a failed one keeps the pin while
/// the set is fresh.
#[tokio::test]
async fn apply_pull_records_a_set_and_keeps_it_across_a_failure() {
    let mut state = SearchAppState::new(IndexRegistry::new());
    state.residency_pins = Arc::new(ResidencyPins::new(STALE, GRACE));
    // #10533: only a registered id is trusted.
    state.registry.register(bare_handle("a"));
    let state = Arc::new(state);
    let t0 = Instant::now();

    apply_pull(&state, Ok(set_of(&["a"])), t0).await;
    assert!(state
        .residency_pins
        .members(t0, &registered_in(&state))
        .is_some_and(|m| m.contains("a")));

    let t1 = t0 + Duration::from_secs(30);
    apply_pull(&state, Err("dial failed".to_string()), t1).await;
    assert!(state
        .residency_pins
        .members(t1, &registered_in(&state))
        .is_some_and(|m| m.contains("a")));
}

/// Only a member that is registered, cold-parked and not failed is woken.
#[test]
fn prewake_targets_names_only_cold_registered_members() {
    let state = SearchAppState::new(IndexRegistry::new());
    state.registry.register(bare_handle("hot"));
    let _guards = state
        .cold_store
        .register_cold_entries(vec![entry("cold", None), entry("broken", None)]);
    state
        .cold_store
        .mark_failed(&IndexId::new("broken".to_string()));
    let members: HashSet<String> = ["hot", "cold", "broken", "unknown"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let targets: Vec<String> = prewake_targets(&state, &members, None)
        .into_iter()
        .map(|id| id.0)
        .collect();
    assert_eq!(targets, vec!["cold".to_string()]);
}

// ---- code-critic round 1 (#10533) ----

/// Finding 1a: a search 61 s before the sweep advances only the indexer clock
/// (the stamp is written at most once per 60 s). Inside grace it must stay;
/// the same index idle for an hour parks, so the check can fail.
#[tokio::test]
#[serial_test::serial]
async fn a_search_without_a_stamp_write_keeps_a_non_member_within_grace() {
    clear_env();
    let t0 = Instant::now();
    let entries = vec![
        entry("member", Some(1)),
        entry("idle", Some(NOW_UNIX - 3_600)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);
    let state = Arc::new(resident_state(&entries));
    observe(&state, set_of(&["member"]), t0);

    idle_for(&state, "idle", Duration::from_secs(61)).await;
    run_residency_sweep_tick_at(&state, t0 + GRACE, NOW_UNIX, |_| {}).await;
    assert!(resident(&state, "idle"), "a search 61 s ago keeps it");

    idle_for(&state, "idle", Duration::from_secs(3_600)).await;
    run_residency_sweep_tick_at(&state, t0 + GRACE, NOW_UNIX, |_| {}).await;
    assert!(!resident(&state, "idle"), "an hour idle parks");
    clear_env();
}

/// Finding 1b: an index used only through grep or list_chunks stays resident
/// within grace. Neither path writes the query stamp.
#[tokio::test]
#[serial_test::serial]
async fn grep_and_list_chunks_keep_a_non_member_resident_within_grace() {
    for use_grep in [true, false] {
        clear_env();
        let t0 = Instant::now();
        let entries = vec![
            entry("member", Some(1)),
            entry("reader", Some(NOW_UNIX - 3_600)),
        ];
        let _tmp = isolate_and_seed_toml(&entries);
        let state = Arc::new(resident_state(&entries));
        observe(&state, set_of(&["member"]), t0);
        idle_for(&state, "reader", Duration::from_secs(3_600)).await;

        // The result does not matter here; the read is the activity.
        if use_grep {
            let req: crate::service::grep::GrepRequest =
                serde_json::from_value(serde_json::json!({ "pattern": "fn" })).expect("grep req");
            let _ = crate::service::server::files::grep_report(&state, "reader", req).await;
        } else {
            let params: crate::service::server::files::ChunksParams =
                serde_json::from_value(serde_json::json!({})).expect("chunk params");
            let _ =
                crate::service::server::files::index_chunks_report(&state, "reader", &params).await;
        }
        run_residency_sweep_tick_at(&state, t0 + GRACE, NOW_UNIX, |_| {}).await;
        assert!(
            resident(&state, "reader"),
            "use_grep={use_grep}: a read inside grace keeps it"
        );
    }
    clear_env();
}

/// Finding 2: a set naming only ids this daemon does not know is untrusted,
/// so nothing parks under the cap.
#[tokio::test]
#[serial_test::serial]
async fn a_set_naming_only_unknown_ids_parks_nothing_under_the_cap() {
    clear_env();
    let t0 = Instant::now();
    let entries = vec![
        entry("a", Some(NOW_UNIX - 3_600)),
        entry("b", Some(NOW_UNIX - 3_600)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);
    let state = Arc::new(resident_state(&entries));
    idle_for(&state, "a", Duration::from_secs(3_600)).await;
    idle_for(&state, "b", Duration::from_secs(3_600)).await;
    observe(&state, set_of(&["ghost-1", "ghost-2"]), t0);

    assert_eq!(
        state.residency_pins.members(t0, &registered_in(&state)),
        None
    );
    run_residency_sweep_tick_at(&state, t0 + GRACE * 2, NOW_UNIX, |_| {}).await;
    assert!(resident(&state, "a") && resident(&state, "b"));
    clear_env();
}

/// Finding 3: an empty set inside the stale window was never trusted, so the
/// first non-empty set restarts the absence clock.
#[test]
fn absence_restarts_after_an_empty_set() {
    let pins = ResidencyPins::new(Duration::from_secs(7_200), GRACE);
    let t0 = Instant::now();
    pins.observe(ActiveProjectSet::default(), t0, &any);
    let later = t0 + Duration::from_secs(3_600);
    pins.observe(set_of(&["a"]), later, &any);
    assert_eq!(pins.absent_for("b", later, &any), Some(Duration::ZERO));
}

/// Design choice 1: active-set members take their cap slots, so only
/// `cap - members` non-members stay.
#[test]
fn active_set_members_count_against_the_cap() {
    let entries = vec![
        entry("m1", Some(1)),
        entry("m2", Some(2)),
        entry("n1", Some(300)),
        entry("n2", Some(200)),
    ];
    let pinned: HashSet<String> = ["m1", "m2"].iter().map(|s| s.to_string()).collect();
    let parked: Vec<String> = crate::service::lazy_loader::ids_to_park(entries, 3, &pinned)
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(parked, vec!["n2".to_string()]);
}

/// Ruling (b): members filling the cap leave the most recently active
/// non-member resident, on this sweep and the next.
#[tokio::test]
#[serial_test::serial]
async fn cap_keeps_the_most_recent_non_member_when_members_fill_the_cap() {
    clear_env();
    let t0 = Instant::now();
    let entries = vec![
        entry("m1", Some(NOW_UNIX - 10)),
        entry("m2", Some(NOW_UNIX - 10)),
        entry("n1", Some(NOW_UNIX - 8)),
        entry("n2", Some(NOW_UNIX - 9)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);
    let state = Arc::new(resident_state(&entries));
    observe(&state, set_of(&["m1", "m2"]), t0);

    for tick in 0..2 {
        run_residency_sweep_tick_at(&state, t0, NOW_UNIX, |_| {}).await;
        assert!(
            resident(&state, "m1") && resident(&state, "m2"),
            "tick {tick}"
        );
        assert!(
            resident(&state, "n1"),
            "tick {tick}: the newest non-member stays"
        );
        assert!(!resident(&state, "n2"), "tick {tick}: the older one parks");
    }
    clear_env();
}

/// Design choice 2: `TRUSTY_MAX_RESIDENT_INDEXES=off` parks nothing, not the
/// cap and not a grace park.
#[tokio::test]
#[serial_test::serial]
async fn max_resident_off_parks_nothing_not_even_a_grace_park() {
    clear_env();
    let t0 = Instant::now();
    let entries = vec![
        entry("member", Some(1)),
        entry("idle", Some(1)),
        entry("cold", Some(1)),
    ];
    let _tmp = isolate_and_seed_toml(&entries);
    unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "off") };
    let state = Arc::new(resident_state(&entries));
    observe(&state, set_of(&["member"]), t0);
    idle_for(&state, "idle", Duration::from_secs(3_600)).await;
    idle_for(&state, "cold", Duration::from_secs(3_600)).await;

    run_residency_sweep_tick_at(&state, t0 + GRACE * 2, NOW_UNIX, |_| {}).await;
    for id in ["member", "idle", "cold"] {
        assert!(resident(&state, id), "{id} stays with the cap off");
    }
    clear_env();
}

/// Design choice 3: "no query" includes the last reindex, read from the
/// persisted `last_indexed_unix`.
#[tokio::test]
#[serial_test::serial]
async fn a_recent_reindex_counts_as_activity_for_the_grace_period() {
    clear_env();
    let t0 = Instant::now();
    let reindexed = PersistedIndex {
        last_indexed_unix: Some(NOW_UNIX - 30),
        ..entry("reindexed", None)
    };
    let entries = vec![entry("member", Some(1)), reindexed];
    let _tmp = isolate_and_seed_toml(&entries);
    let state = Arc::new(resident_state(&entries));
    observe(&state, set_of(&["member"]), t0);
    idle_for(&state, "reindexed", Duration::from_secs(3_600)).await;

    run_residency_sweep_tick_at(&state, t0 + GRACE, NOW_UNIX, |_| {}).await;
    assert!(resident(&state, "reindexed"), "a reindex 30 s ago keeps it");
    clear_env();
}

/// Design choice 4: a pull interval of `0`, or `TRUSTY_RESIDENCY=off`,
/// disables the ticker.
#[test]
fn pull_interval_zero_disables_the_ticker() {
    assert_eq!(pull_interval(None, Some("0")), None);
    assert_eq!(pull_interval(Some("off"), None), None);
    assert_eq!(
        pull_interval(None, Some("45")),
        Some(Duration::from_secs(45))
    );
}

/// Design choice 5: one `mpm.residency.active` fetch waits at most 5 s.
#[test]
fn active_set_pull_timeout_is_five_seconds() {
    assert_eq!(ACTIVE_SET_PULL_TIMEOUT, Duration::from_secs(5));
}

/// Ruling (b): pre-wake keeps resident members at `cap - 1` at most; with the
/// cap off it wakes every cold member.
#[test]
fn prewake_leaves_one_slot_for_a_non_member() {
    let state = SearchAppState::new(IndexRegistry::new());
    state.registry.register(bare_handle("r"));
    let _guards = state.cold_store.register_cold_entries(vec![
        entry("c1", None),
        entry("c2", None),
        entry("c3", None),
    ]);
    let members: HashSet<String> = ["r", "c1", "c2", "c3"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let ids = |cap| -> Vec<String> {
        prewake_targets(&state, &members, cap)
            .into_iter()
            .map(|id| id.0)
            .collect()
    };
    assert_eq!(ids(Some(3)), vec!["c1".to_string()]);
    assert!(ids(Some(1)).is_empty());
    assert_eq!(ids(None).len(), 3);
}

/// Ruling (b): `/health` names whether the set is trusted and how many
/// registered indexes it pins.
#[tokio::test]
#[serial_test::serial]
async fn health_reports_the_active_set_pin_count_and_trust() {
    let state = SearchAppState::new(IndexRegistry::new());
    state.registry.register(bare_handle("a"));
    let state = Arc::new(state);
    let resp = crate::service::server::health::health_report(state.clone()).await;
    assert_eq!(
        resp["residency_active_set"],
        serde_json::json!({ "trusted": false, "pinned_members": 0 })
    );

    observe(&state, set_of(&["a", "ghost"]), Instant::now());
    let resp = crate::service::server::health::health_report(state.clone()).await;
    assert_eq!(
        resp["residency_active_set"],
        serde_json::json!({ "trusted": true, "pinned_members": 1 })
    );
}

/// Finding 6: each fresh/stale change is reported once.
#[test]
fn freshness_change_reports_each_transition_once() {
    let pins = ResidencyPins::new(STALE, GRACE);
    let t0 = Instant::now();
    assert_eq!(pins.freshness_change(t0), None, "never fetched is stale");
    pins.observe(set_of(&["a"]), t0, &any);
    assert_eq!(pins.freshness_change(t0), Some(true));
    assert_eq!(pins.freshness_change(t0), None);
    assert_eq!(pins.freshness_change(t0 + STALE), Some(false));
    assert_eq!(pins.freshness_change(t0 + STALE), None);
}

/// Ruling (b): the members-fill-the-cap warning fires once per change.
#[test]
fn over_cap_warns_once_per_state_change() {
    let pins = ResidencyPins::new(STALE, GRACE);
    assert!(pins.note_over_cap(true));
    assert!(!pins.note_over_cap(true));
    assert!(!pins.note_over_cap(false));
    assert!(pins.note_over_cap(true));
}
