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

/// Why: #10534 — an idle worktree-session index is evicted at 30 days,
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

/// Why: #10534 — a stamp of 0 is an unset field, not activity at the epoch,
/// so it is an unknown age and the index is kept.
/// Test: this test.
#[test]
fn retention_keeps_an_index_with_a_zero_stamp() {
    let zero = RetentionInput {
        last_activity_unix: Some(0),
        ..covered(0)
    };
    assert_eq!(
        retention_verdict(zero, NOW),
        Verdict::Keep(KeepReason::UnknownAge)
    );
    let zero_but_parked = RetentionInput {
        parked_unix: Some(NOW - 400 * DAY),
        ..zero
    };
    assert_eq!(
        retention_verdict(zero_but_parked, NOW),
        Verdict::Keep(KeepReason::UnknownAge)
    );
}

/// A worktree sibling `repo/.claude/worktrees/<name>`.
fn worktree(main: &Path, name: &str) -> PathBuf {
    let root = main.join(".claude").join("worktrees").join(name);
    std::fs::create_dir_all(&root).expect("worktree");
    std::fs::write(root.join(".git"), "gitdir: x\n").expect(".git");
    root
}

/// Load `id` (rooted at `root`) into the hot registry.
fn load(rig: &Rig, id: &str, root: &Path) {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId};
    let indexer = Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(id, root)));
    rig.state.registry.register(IndexHandle::bare(
        IndexId::new(id.to_string()),
        indexer,
        root.to_path_buf(),
    ));
}

/// Why: #10534 — the pass reads busy state and the active set once, then
/// waits on each delete's teardown lock. An index loaded, or named by the
/// active set, during that wait is kept.
/// What: a held shared teardown lock parks the delete after its scan; the
/// test loads or trusts the index, then releases the lock.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn an_index_loaded_or_made_live_mid_pass_is_kept() {
    use crate::core::registry::IndexId;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    for loaded in [true, false] {
        let id = if loaded {
            "s-race-loaded"
        } else {
            "s-race-live"
        };
        let mut root = PathBuf::new();
        let rig = rig(|main, _| {
            root = worktree(main, id);
            vec![row("repo", main, Some(1)), row(id, &root, Some(45))]
        });
        let now = Instant::now();
        rig.trust(&["repo"], now);
        let key = IndexId::new(id.to_string());
        let cancel = crate::service::reindex::index_cancel_flag(&key);
        cancel.store(false, Ordering::Release);
        let writer = crate::service::reindex::acquire_index_teardown_read(&key).await;
        let state = Arc::clone(&rig.state);
        let pass = tokio::spawn(async move { evict_idle_at(&state, now, NOW).await });

        // The delete signals the cancel flag, then waits on the lock.
        let deadline = Instant::now() + Duration::from_secs(1);
        while !cancel.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "the pass never reached the delete"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if loaded {
            load(&rig, id, &root);
        } else {
            rig.trust(&["repo", id], Instant::now());
        }
        drop(writer);

        assert_eq!(pass.await.expect("pass"), 0, "loaded={loaded}");
        assert!(rig.registered(id), "loaded={loaded}: the row is kept");
        assert!(
            rig.marker(id).exists(),
            "loaded={loaded}: the corpus is kept"
        );
    }
}

/// Why: #10534 — grep, global grep, chunk pages and the fan-out are use.
/// Each records `last_queried_unix`, so an index read that way within 30
/// days is kept after it is unloaded.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_grep_chunk_or_fan_out_read_keeps_an_idle_worktree_index() {
    use crate::core::registry::IndexId;
    for read in ["grep", "grep-all", "chunks", "fan-out"] {
        let wall = crate::core::project_map::now_unix();
        let stamped = |id: &str, root: &Path, idle: u64| PersistedIndex {
            last_queried_unix: Some(wall - idle),
            ..PersistedIndex::new(id, root)
        };
        let mut wt_root = PathBuf::new();
        let rig = rig(|main, wt| {
            wt_root = wt.to_path_buf();
            vec![stamped("repo", main, DAY), stamped("s1", wt, 45 * DAY)]
        });
        let now = Instant::now();
        rig.trust(&["repo"], now);
        load(&rig, "s1", &wt_root);

        // The result does not matter here; the read is the activity.
        match read {
            "grep" | "grep-all" => {
                let mut body = serde_json::json!({ "pattern": "fn" });
                if read == "grep-all" {
                    body["index_id"] = "s1".into();
                }
                let req: crate::service::grep::GrepRequest =
                    serde_json::from_value(body).expect("grep req");
                if read == "grep" {
                    let _ = super::files::grep_report(&rig.state, "s1", req).await;
                } else {
                    let _ = super::files::global_grep_report(&rig.state, req).await;
                }
            }
            "chunks" => {
                let params: super::files::ChunksParams =
                    serde_json::from_value(serde_json::json!({})).expect("chunk params");
                let _ = super::files::index_chunks_report(&rig.state, "s1", &params).await;
            }
            _ => {
                let req: super::search_global::GlobalSearchRequest =
                    serde_json::from_value(serde_json::json!({ "query": "fn" }))
                        .expect("fan-out req");
                let _ = super::search_global::global_search_report(&rig.state, req).await;
            }
        }
        // Unloaded again: the retention pass sees only the persisted stamps.
        let _ = rig
            .state
            .registry
            .remove_and_get(&IndexId::new("s1".to_string()));

        assert_eq!(
            evict_idle_at(&rig.state, now, wall).await,
            0,
            "{read}: a read within 30 days keeps the index"
        );
        assert!(rig.registered("s1") && rig.marker("s1").exists(), "{read}");
    }
}
