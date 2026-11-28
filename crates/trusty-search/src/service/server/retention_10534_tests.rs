//! The 30-day retention rule and its evict pass (#10534).
//!
//! Why: eviction deletes a corpus (rung 5), so the boundaries and every
//! fail-closed arm are pinned here.
//! What: pure tests of [`retention_class`] and [`retention_verdict`], then
//! passes over a sandboxed data dir (`TRUSTY_DATA_DIR`, serial) with cold
//! registrations and a trusted active set.
//! Test: this module. Run with `cargo test -p trusty-search retention_10534`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use trusty_common::residency::{ActiveProject, ActiveProjectSet};

use super::retention::{
    evict_idle_at, retention_class, retention_verdict, KeepReason, RetentionClass, RetentionInput,
    Verdict, RETENTION_SECS,
};
use super::state::SearchAppState;
use crate::core::project_map::ParkReason;
use crate::core::registry::IndexRegistry;
use crate::service::persistence::{indexes_toml_path, save_index_registry_at, PersistedIndex};
use crate::service::project_resolve::RootKind;

const DAY: u64 = 86_400;
const NOW: u64 = 2_000_000_000;

fn covered(idle_days: u64) -> RetentionInput {
    RetentionInput {
        class: Some(RetentionClass::Duplicate),
        last_activity_unix: Some(NOW - idle_days * DAY),
        parked_unix: None,
        live: false,
    }
}

/// Why: the park reasons and root kinds the rule covers, and the ones it
/// must never reach (a main checkout, a missing root).
/// Test: this test.
#[test]
fn retention_class_maps_park_reasons_and_root_kinds() {
    use RetentionClass::{Duplicate, Unregistered};
    let cases = [
        (None, RootKind::Worktree, Some(Duplicate)),
        (None, RootKind::MainCheckout, None),
        (None, RootKind::Checkout, None),
        (None, RootKind::Orphaned, None),
        (None, RootKind::Indeterminate, None),
        (
            Some(ParkReason::Worktree),
            RootKind::Worktree,
            Some(Duplicate),
        ),
        (
            Some(ParkReason::Operator),
            RootKind::Checkout,
            Some(Duplicate),
        ),
        (
            Some(ParkReason::Decommissioned),
            RootKind::MainCheckout,
            Some(Unregistered),
        ),
        (Some(ParkReason::Orphaned), RootKind::Checkout, None),
        // The 7-day orphan path owns a missing root, whatever its park row.
        (Some(ParkReason::Decommissioned), RootKind::Orphaned, None),
        (Some(ParkReason::Worktree), RootKind::Orphaned, None),
    ];
    for (reason, kind, want) in cases {
        assert_eq!(retention_class(reason, kind), want, "{reason:?} {kind:?}");
    }
}

/// Why: AC 4 boundaries — 29 days kept, 30 and 31 evicted.
/// Test: this test.
#[test]
fn retention_keeps_29_days_and_evicts_30_and_31() {
    assert_eq!(RETENTION_SECS, 30 * DAY);
    assert_eq!(
        retention_verdict(covered(29), NOW),
        Verdict::Keep(KeepReason::Recent)
    );
    for days in [30, 31] {
        assert_eq!(
            retention_verdict(covered(days), NOW),
            Verdict::Evict {
                class: RetentionClass::Duplicate,
                idle_secs: days * DAY
            },
            "{days} days"
        );
    }
    let unregistered = RetentionInput {
        class: Some(RetentionClass::Unregistered),
        ..covered(30)
    };
    assert!(matches!(
        retention_verdict(unregistered, NOW),
        Verdict::Evict {
            class: RetentionClass::Unregistered,
            ..
        }
    ));
}

/// Why: AC 2 — a registered, non-duplicate index is never evicted.
/// Test: this test.
#[test]
fn retention_keeps_a_registered_index_however_idle() {
    let input = RetentionInput {
        class: None,
        ..covered(400)
    };
    assert_eq!(
        retention_verdict(input, NOW),
        Verdict::Keep(KeepReason::NotCovered)
    );
}

/// Why: Fail-Open Check — no activity stamp means the age is unknown, and an
/// unknown age keeps the index; it is never read as idle since the epoch.
/// A clock behind the stamp reads 0 idle, not a huge age.
/// Test: this test.
#[test]
fn retention_keeps_an_index_of_unknown_age() {
    let unknown = RetentionInput {
        last_activity_unix: None,
        parked_unix: Some(NOW - 400 * DAY),
        ..covered(0)
    };
    assert_eq!(
        retention_verdict(unknown, NOW),
        Verdict::Keep(KeepReason::UnknownAge)
    );
    let skewed = RetentionInput {
        last_activity_unix: Some(NOW + 60 * DAY),
        ..covered(0)
    };
    assert_eq!(
        retention_verdict(skewed, NOW),
        Verdict::Keep(KeepReason::Recent)
    );
}

/// Why: AC 2 — an index a live session uses is never evicted.
/// Test: this test.
#[test]
fn retention_keeps_a_live_session_member() {
    let input = RetentionInput {
        live: true,
        ..covered(90)
    };
    assert_eq!(
        retention_verdict(input, NOW),
        Verdict::Keep(KeepReason::LiveSession)
    );
}

/// Why: a newly parked duplicate gets its full 30 days from the park.
/// Test: this test.
#[test]
fn retention_restarts_the_clock_at_the_park_time() {
    let input = RetentionInput {
        parked_unix: Some(NOW - 10 * DAY),
        ..covered(60)
    };
    assert_eq!(
        retention_verdict(input, NOW),
        Verdict::Keep(KeepReason::Recent)
    );
}

/// A sandboxed daemon: registered cold rows, each with a data-dir marker.
struct Rig {
    data: super::tests_components::IsolatedDataDir,
    _tmp: tempfile::TempDir,
    state: Arc<SearchAppState>,
}

impl Rig {
    fn marker(&self, id: &str) -> PathBuf {
        self.data
            .path()
            .join("indexes")
            .join(id)
            .join("corpus.marker")
    }

    fn registered(&self, id: &str) -> bool {
        crate::service::persistence::find_index_registry_entry(id)
            .expect("registry readable")
            .is_some()
    }

    /// `state.residency_pins` trusts a set naming `live` at `now`.
    fn trust(&self, live: &[&str], now: Instant) {
        let mut project = ActiveProject::default();
        project.index_ids = live.iter().map(|s| s.to_string()).collect();
        let mut set = ActiveProjectSet::default();
        set.projects = vec![project];
        let registered = super::residency_pins::registered_in(&self.state);
        self.state.residency_pins.observe(set, now, &registered);
    }
}

fn row(id: &str, root: &Path, idle_days: Option<u64>) -> PersistedIndex {
    PersistedIndex {
        last_queried_unix: idle_days.map(|d| NOW - d * DAY),
        ..PersistedIndex::new(id, root)
    }
}

/// `repo` (a main checkout) and its session worktree `repo/.claude/worktrees/s1`,
/// plus any extra rows; every row is registered cold with a data-dir marker.
fn rig(rows: impl FnOnce(&Path, &Path) -> Vec<PersistedIndex>) -> Rig {
    let data = super::tests_components::IsolatedDataDir::new();
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("repo");
    std::fs::create_dir_all(main.join(".git")).expect("main checkout");
    let wt = main.join(".claude").join("worktrees").join("s1");
    std::fs::create_dir_all(&wt).expect("worktree");
    std::fs::write(wt.join(".git"), "gitdir: elsewhere\n").expect("linked .git");
    let rows = rows(&main, &wt);
    save_index_registry_at(&indexes_toml_path().expect("toml path"), &rows).expect("seed");
    for r in &rows {
        let dir = data.path().join("indexes").join(&r.id);
        std::fs::create_dir_all(&dir).expect("data dir");
        std::fs::write(dir.join("corpus.marker"), b"corpus").expect("marker");
    }
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    state.cold_store.register_cold_entries(rows);
    Rig {
        data,
        _tmp: tmp,
        state,
    }
}

/// Why: ruling Dta — an idle worktree-session index is evicted at 30 days,
/// corpus included, while the main checkout (registered, non-duplicate), a
/// 29-day worktree, an unknown-age worktree and a live-session worktree,
/// all equally old or older, are kept.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn an_idle_worktree_session_index_is_evicted_at_30_days() {
    let rig = rig(|main, wt| {
        let sibling = |name: &str| {
            let root = main.join(".claude").join("worktrees").join(name);
            std::fs::create_dir_all(&root).expect("sibling");
            std::fs::write(root.join(".git"), "gitdir: x\n").expect(".git");
            root
        };
        vec![
            row("repo", main, Some(400)),
            row("s1", wt, Some(30)),
            row("s29", &sibling("s29"), Some(29)),
            row("s-unknown", &sibling("s-unknown"), None),
            row("s-live", &sibling("s-live"), Some(90)),
        ]
    });
    let now = Instant::now();
    rig.trust(&["s-live"], now);

    assert_eq!(evict_idle_at(&rig.state, now, NOW).await, 1);

    assert!(!rig.registered("s1"), "the 30-day worktree row is removed");
    assert!(!rig.marker("s1").exists(), "its corpus is deleted");
    for kept in ["repo", "s29", "s-unknown", "s-live"] {
        assert!(rig.registered(kept), "{kept} is kept");
        assert!(rig.marker(kept).exists(), "{kept}'s corpus is kept");
    }
}

/// Why: Fail-Open Check — an unreadable project map or registry evicts
/// nothing and leaves every corpus in place.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn retention_evicts_nothing_when_the_map_or_registry_is_unreadable() {
    let rig = rig(|main, wt| vec![row("repo", main, Some(1)), row("s1", wt, Some(45))]);
    let now = Instant::now();
    rig.trust(&["repo"], now);
    let toml = indexes_toml_path().expect("toml path");
    let map = crate::core::project_map_io::map_path_beside(&toml);

    std::fs::write(&map, "garbage [[").expect("corrupt map");
    assert_eq!(evict_idle_at(&rig.state, now, NOW).await, 0);
    assert!(
        rig.registered("s1") && rig.marker("s1").exists(),
        "no map, no evict"
    );

    std::fs::remove_file(&map).expect("drop map");
    let good = std::fs::read(&toml).expect("registry bytes");
    std::fs::write(&toml, "garbage [[").expect("corrupt registry");
    assert_eq!(evict_idle_at(&rig.state, now, NOW).await, 0);
    assert!(rig.marker("s1").exists(), "no registry, no evict");

    // The same fixture with both readable evicts, so the arms above were real.
    std::fs::write(&toml, good).expect("restore registry");
    assert_eq!(evict_idle_at(&rig.state, now, NOW).await, 1);
}

/// Why: design section 3 — without a trusted active set the pass cannot
/// tell which index a live session uses, so it evicts nothing.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn retention_evicts_nothing_without_a_trusted_active_set() {
    let rig = rig(|main, wt| vec![row("repo", main, Some(1)), row("s1", wt, Some(45))]);
    assert_eq!(evict_idle_at(&rig.state, Instant::now(), NOW).await, 0);
    assert!(rig.registered("s1") && rig.marker("s1").exists());
}
