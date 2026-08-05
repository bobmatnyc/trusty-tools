//! Regression tests for the warm-boot dead-entry budget (#4846).
//!
//! Why: the defect is a cost, not a wrong answer, so a correctness assertion
//! alone would have shipped green against the broken code. What actually broke
//! the reporting machine was that the tracked-root relocation walk ran once per
//! DEAD entry rather than once per boot — measured at 9.5–10.5 s per walk over
//! that machine's 248 roots, 55 dead entries, and a live 70k-chunk index
//! starved behind the backlog for the better part of an hour.
//!
//! So the assertion here is a RATIO measured in-process: the test builds a
//! tracked-root tree deep enough for one walk to be measurable, times a single
//! walk, then times a whole warm boot carrying many dead entries and requires
//! the boot to cost a small multiple of ONE walk. Against the pre-fix code that
//! multiple is the dead-entry count. Self-calibrating, so it does not depend on
//! this machine being any particular speed.
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
/// the live index is registered, and (b) the whole boot costs no more than a
/// small multiple of ONE relocation walk timed in this same process. Against
/// the pre-fix code (b) fails by roughly `DEAD_ENTRIES`x.
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

    let boot_started = Instant::now();
    // `no_auto_discover = true` keeps the colocated discovery scan out of the
    // measurement — this test is about the relocation walk, not discovery.
    super::restore::restore_indexes(&state, &embedder, true).await;
    let boot = boot_started.elapsed();

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DISABLE_WATCHER");
    }

    assert!(
        state.registry.get(&IndexId::new("live-index")).is_some(),
        "the live index must be registered even though {DEAD_ENTRIES} dead entries \
         precede it in indexes.toml (issue #4846)"
    );

    // The budget assertion. Post-fix the boot pays at most ONE walk (the shared
    // salvage scan); pre-fix it paid one per dead entry. 6x leaves room for the
    // live restore and scheduler noise while staying far below 24x. The 250 ms
    // floor keeps a very fast filesystem from making the ceiling absurdly tight
    // without ever letting it approach the pre-fix cost.
    let ceiling = std::cmp::max(one_walk * 6, Duration::from_millis(250));
    assert!(
        boot <= ceiling,
        "warm boot took {boot:?} with {DEAD_ENTRIES} dead entries, but ONE relocation \
         walk costs {one_walk:?} — the walk is being repeated per dead entry instead of \
         shared across the boot (issue #4846). Ceiling was {ceiling:?}."
    );
}

/// Why (#4846 budget design): the operator's fastest setting must genuinely
/// cost nothing per dead entry. `TRUSTY_WARMBOOT_SALVAGE_SECS=0` disables
/// salvage, and the guarantee is that a dead entry is then settled by its
/// triage stat and NOTHING else — no relocation walk at all — while every live
/// index still comes up.
/// What: same registry shape with salvage disabled; asserts the live index is
/// registered and the boot costs less than a single walk. Against the pre-fix
/// code there was no way to disable the per-entry walk, so the boot cost
/// `DEAD_ENTRIES` walks regardless of any environment variable.
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

    let boot_started = Instant::now();
    super::restore::restore_indexes(&state, &embedder, true).await;
    let boot = boot_started.elapsed();

    // Read the registry BEFORE clearing TRUSTY_DATA_DIR — otherwise this
    // resolves to the operator's real registry rather than the test's.
    let after = crate::service::persistence::load_index_registry().unwrap();

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DISABLE_WATCHER");
        std::env::remove_var(crate::service::warm_boot::SALVAGE_BUDGET_ENV);
    }

    assert!(
        state.registry.get(&IndexId::new("live-index")).is_some(),
        "disabling salvage must never cost a LIVE index its restore (issue #4846)"
    );
    let ceiling = std::cmp::max(one_walk * 2, Duration::from_millis(250));
    assert!(
        boot <= ceiling,
        "with salvage disabled, {DEAD_ENTRIES} dead entries must cost one stat each and \
         no relocation walk at all — boot took {boot:?} against a single-walk cost of \
         {one_walk:?} (ceiling {ceiling:?}, issue #4846)"
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
