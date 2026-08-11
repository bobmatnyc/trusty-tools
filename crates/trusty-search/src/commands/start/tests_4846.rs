//! Regression tests for the warm-boot dead-entry budget (#4846).
//!
//! Why: the defect is a cost, not a wrong answer, so a correctness assertion
//! alone would have shipped green against the broken code. What actually broke
//! the reporting machine was that the tracked-root relocation walk ran once per
//! DEAD entry rather than once per boot — measured at 9.5–10.5 s per walk over
//! that machine's 248 roots, 55 dead entries, and a live 70k-chunk index
//! starved behind the backlog for the better part of an hour.
//!
//! Both tests originally asserted a wall-clock ratio — boot time against a
//! multiple of one timed walk. #5084 replaced that with a COUNT of walks. The
//! clock could not work: a contended post-fix boot measured 416–435 ms (once
//! 833 ms) while the pre-fix cost is 24 walks ≈ 340–400 ms, so every ceiling
//! that cleared the false reds also cleared the real regression. The count has
//! no such overlap — post-fix a boot walks the tracked roots once (or, with
//! salvage disabled, not at all); pre-fix it walked them 24 times.
//!
//! The timings survive as a printed diagnostic (`--nocapture`) so cost erosion
//! stays observable, but nothing asserts on them.
//!
//! Test: the two tests below.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::registry::{IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::PersistedIndex;
use crate::service::SearchAppState;
use trusty_common::embedder::MockEmbedder;

/// How many dead entries a boot carries in these tests.
///
/// Why: large enough that "one walk per dead entry" is unmistakably distinct
/// from "one walk per boot", small enough that even the pre-fix cost stays
/// inside a test's patience if this ever regresses.
const DEAD_ENTRIES: usize = 24;

/// Build a tracked-root tree whose relocation walk is measurable.
///
/// Why: `scan_roots_for_colocated_indexes` recurses to depth 5, calling
/// `read_dir` and `canonicalize` on every subdirectory. A flat tempdir walks in
/// microseconds, which would make the ratio assertion below meaningless. This
/// creates enough directories that one walk costs milliseconds.
fn make_walkable_root(base: &Path) -> PathBuf {
    let root = base.join("tracked-root");
    for a in 0..40 {
        for b in 0..40 {
            std::fs::create_dir_all(root.join(format!("a{a}")).join(format!("b{b}"))).unwrap();
        }
    }
    root
}

/// A colocated entry pointing at `root`.
fn colocated_entry(id: &str, root: PathBuf) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: root,
        colocated: true,
        ..Default::default()
    }
}

/// Populate a live colocated index root so warm-boot can actually restore it.
fn make_live_root(base: &Path, name: &str) -> PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(root.join(COLOCATED_DIR_NAME)).unwrap();
    root
}

/// Print what the boot cost, on PASS as well as on failure (#5084).
///
/// Why: the wall-clock ceiling is gone as a gate, but the numbers behind it are
/// still the early warning that the boot path is getting more expensive. A
/// passing run used to print nothing, so margin erosion was invisible until the
/// day it turned red. `former_ceiling` is the value the retired assertion would
/// have compared against — a reference point for reading the numbers, not a
/// threshold anything checks.
/// What: writes one line to stderr. Shown by `cargo test -- --nocapture`, and
/// replayed automatically by libtest for a failing test.
fn report_cost(test: &str, boot: Duration, one_walk: Duration, walks: usize, multiple: u32) {
    let former_ceiling = std::cmp::max(one_walk * multiple, Duration::from_millis(250));
    eprintln!(
        "#4846 cost report [{test}]: boot={boot:?} one_walk={one_walk:?} \
         tracked_root_walks={walks} (retired wall-clock ceiling was {former_ceiling:?}, \
         non-gating since #5084)"
    );
}

/// Why (#4846, the acceptance test): a live index must come up on a boot whose
/// registry is dominated by dead entries, and it must not wait behind them.
/// Before the fix, warm-boot walked entries in registry order under one shared
/// per-index deadline, and each dead entry dragged a full tracked-root
/// relocation walk with it — so a live index sitting behind 24 dead rows paid
/// 24 walks before anyone could query it. Triage now settles each entry with a
/// single stat and hands the loops two separate vectors, so live entries are
/// restored before a dead one is probed at all.
///
/// What: writes an `indexes.toml` whose dead entries come FIRST (the adversarial
/// order — the pre-fix code would process them first), boots, and asserts (a)
/// the live index is registered, and (b) the boot performed at most ONE
/// tracked-root relocation walk. Against the pre-fix code (b) counts
/// `DEAD_ENTRIES` walks. #5084 replaced a wall-clock form of (b) that no ceiling
/// could separate from a contended pass.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn dead_entries_do_not_consume_the_live_index_budget() {
    let data_tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();

    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        std::env::set_var("TRUSTY_DISABLE_WATCHER", "1");
    }

    let tracked = make_walkable_root(work.path());
    crate::service::roots_registry::upsert_root(tracked.clone()).unwrap();

    // Time ONE relocation walk over the tracked root — the reference cost.
    let grant =
        crate::service::warm_boot::SalvageBudget::with_budget(Some(Duration::from_secs(60)))
            .try_grant()
            .unwrap();
    let started = Instant::now();
    let _ = crate::commands::start_restore::collect_relocation_candidates(&[], &grant);
    let one_walk = started.elapsed();

    // Registry: dead entries FIRST, then the live one.
    let live_root = make_live_root(work.path(), "live-index");
    let mut entries: Vec<PersistedIndex> = (0..DEAD_ENTRIES)
        .map(|i| colocated_entry(&format!("dead-{i}"), work.path().join(format!("gone-{i}"))))
        .collect();
    entries.push(colocated_entry("live-index", live_root.clone()));
    crate::service::persistence::save_index_registry(&entries).unwrap();

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(16));
    let state = SearchAppState::new(IndexRegistry::new());

    // #5084: count the boot's tracked-root walks. Armed after the reference
    // walk above so the count covers the boot and nothing else, and scoped to
    // this test's tempdir so a parallel sibling's scan cannot inflate it.
    let probe = crate::service::fs_discovery::walk_probe::WalkProbe::watching(work.path());
    let boot_started = Instant::now();
    // `no_auto_discover = true` keeps the colocated discovery scan out of the
    // measurement — this test is about the relocation walk, not discovery.
    super::restore::restore_indexes(&state, &embedder, true).await;
    let boot = boot_started.elapsed();
    let walks = probe.walks();

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DISABLE_WATCHER");
    }

    report_cost(
        "dead_entries_do_not_consume_the_live_index_budget",
        boot,
        one_walk,
        walks,
        6,
    );

    assert!(
        state.registry.get(&IndexId::new("live-index")).is_some(),
        "the live index must be registered even though {DEAD_ENTRIES} dead entries \
         precede it in indexes.toml (issue #4846)"
    );

    // The budget assertion. Post-fix the whole cohort shares ONE salvage walk;
    // pre-fix every dead entry paid its own, so this counts `DEAD_ENTRIES`.
    // Counting the walks rather than timing them is what makes the check
    // load-independent (#5084) — 1 and 24 do not overlap at any machine speed.
    assert!(
        walks <= 1,
        "warm boot walked the tracked roots {walks} times with {DEAD_ENTRIES} dead \
         entries — the walk must be shared across the boot, not repeated per dead \
         entry (issue #4846). One walk costs {one_walk:?}; the boot took {boot:?}."
    );
}

/// Why (#4846 budget design): the operator's fastest setting must genuinely
/// cost nothing per dead entry. `TRUSTY_WARMBOOT_SALVAGE_SECS=0` disables
/// salvage, and the guarantee is that a dead entry is then settled by its
/// triage stat and NOTHING else — no relocation walk at all — while every live
/// index still comes up.
/// What: same registry shape with salvage disabled; asserts the live index is
/// registered and the boot performed ZERO tracked-root walks. Against the
/// pre-fix code there was no way to disable the per-entry walk, so the boot
/// walked `DEAD_ENTRIES` times regardless of any environment variable. #5084
/// replaced a wall-clock form of the second assertion — "nothing but a stat" is
/// a claim about work done, and zero is the exact number that claim names.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn disabled_salvage_budget_costs_a_dead_entry_nothing_but_a_stat() {
    let data_tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();

    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        std::env::set_var("TRUSTY_DISABLE_WATCHER", "1");
        std::env::set_var(crate::service::warm_boot::SALVAGE_BUDGET_ENV, "0");
    }

    let tracked = make_walkable_root(work.path());
    crate::service::roots_registry::upsert_root(tracked.clone()).unwrap();

    let grant =
        crate::service::warm_boot::SalvageBudget::with_budget(Some(Duration::from_secs(60)))
            .try_grant()
            .unwrap();
    let started = Instant::now();
    let _ = crate::commands::start_restore::collect_relocation_candidates(&[], &grant);
    let one_walk = started.elapsed();

    let live_root = make_live_root(work.path(), "live-index");
    let mut entries: Vec<PersistedIndex> = (0..DEAD_ENTRIES)
        .map(|i| colocated_entry(&format!("dead-{i}"), work.path().join(format!("gone-{i}"))))
        .collect();
    entries.push(colocated_entry("live-index", live_root));
    crate::service::persistence::save_index_registry(&entries).unwrap();

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(16));
    let state = SearchAppState::new(IndexRegistry::new());

    // #5084: see the sibling test — armed after the reference walk, scoped to
    // this test's tempdir.
    let probe = crate::service::fs_discovery::walk_probe::WalkProbe::watching(work.path());
    let boot_started = Instant::now();
    super::restore::restore_indexes(&state, &embedder, true).await;
    let boot = boot_started.elapsed();
    let walks = probe.walks();

    // Read the registry BEFORE clearing TRUSTY_DATA_DIR — otherwise this
    // resolves to the operator's real registry rather than the test's.
    let after = crate::service::persistence::load_index_registry().unwrap();

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DISABLE_WATCHER");
        std::env::remove_var(crate::service::warm_boot::SALVAGE_BUDGET_ENV);
    }

    report_cost(
        "disabled_salvage_budget_costs_a_dead_entry_nothing_but_a_stat",
        boot,
        one_walk,
        walks,
        2,
    );

    assert!(
        state.registry.get(&IndexId::new("live-index")).is_some(),
        "disabling salvage must never cost a LIVE index its restore (issue #4846)"
    );
    assert_eq!(
        walks, 0,
        "with salvage disabled, {DEAD_ENTRIES} dead entries must cost one stat each and \
         no relocation walk at all — the boot walked the tracked roots {walks} times \
         (one walk costs {one_walk:?}; the boot took {boot:?}; issue #4846)"
    );

    // And the dead registrations are still there: a missing root is a reason to
    // spend less time, never a reason to deregister or delete (#4846 operator
    // note — an unloaded index 404s whether it holds 0 chunks or 70,180).
    assert_eq!(
        after.len(),
        DEAD_ENTRIES + 1,
        "no registration may be removed by a failed or skipped probe (issue #4846)"
    );
}
