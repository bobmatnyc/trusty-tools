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
use crate::service::server::residency_pins::ResidencyPins;
use crate::service::server::residency_pull::{apply_pull, prewake_targets};
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
    state.residency_pins.observe(set_of(&["member"]), t0);

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
    state.residency_pins.observe(set_of(&["member"]), t0);

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
            state.residency_pins.observe(set_of(&["member"]), t0);
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
    state.residency_pins.observe(set_of(&["member"]), t0);
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
    assert_eq!(pins.members(t0), None, "never fetched");

    pins.observe(ActiveProjectSet::default(), t0);
    assert_eq!(pins.members(t0), None, "a set with no index ids");
    assert_eq!(pins.absent_for("x", t0), None);

    pins.observe(set_of(&["a"]), t0);
    assert!(pins.members(t0).is_some_and(|m| m.contains("a")));
    assert_eq!(pins.members(t0 + STALE), None, "stale");
}

/// After a stale gap the grace clock restarts at the first trusted pull, so a
/// recovered producer cannot cause an immediate mass park.
#[test]
fn absence_restarts_after_a_stale_gap() {
    let pins = ResidencyPins::new(STALE, GRACE);
    let t0 = Instant::now();
    pins.observe(set_of(&["a"]), t0);
    assert_eq!(pins.absent_for("b", t0 + GRACE), Some(GRACE));

    let recovered = t0 + STALE * 2;
    pins.observe(set_of(&["a"]), recovered);
    assert_eq!(pins.absent_for("b", recovered), Some(Duration::ZERO));
    assert_eq!(
        pins.absent_for("a", recovered),
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
    let state = Arc::new(state);
    let t0 = Instant::now();

    apply_pull(&state, Ok(set_of(&["a"])), t0).await;
    assert!(state
        .residency_pins
        .members(t0)
        .is_some_and(|m| m.contains("a")));

    let t1 = t0 + Duration::from_secs(30);
    apply_pull(&state, Err("dial failed".to_string()), t1).await;
    assert!(state
        .residency_pins
        .members(t1)
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

    let targets: Vec<String> = prewake_targets(&state, &members)
        .into_iter()
        .map(|id| id.0)
        .collect();
    assert_eq!(targets, vec!["cold".to_string()]);
}
