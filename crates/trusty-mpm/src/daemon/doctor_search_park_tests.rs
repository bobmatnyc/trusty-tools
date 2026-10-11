//! Unit tests for `daemon::doctor::doctor_search_park` (#9664).
//!
//! Why: split out so the production module stays under the 500-SLOC cap.
//! What: a mock trusty-search socket answers the map, the resolves and the
//! park; each test reads the steps the repair returns and the parks the mock
//! received.
//! Test: this module IS the test suite for `super`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use trusty_common::uds::server::CODE_INTERNAL_ERROR;

use super::*;
use crate::core::doctor_repair::StepStatus;
use crate::daemon::search_rpc::CODE_CONFLICT;
use crate::uds_mock::{self, MockFuture, RpcError};

/// Limits small enough for a test to hit each bound quickly.
const FAST: FanoutLimits = FanoutLimits {
    max_groups: 8,
    per_call: Duration::from_millis(300),
    budget: Duration::from_secs(5),
};

/// One pinned group `o/a` whose live answer is `a` at `/src/a`.
fn map_body(parked: &[&str]) -> Value {
    let parked: Vec<Value> = parked
        .iter()
        .map(|id| serde_json::json!({ "index_id": id, "key": "o/a", "reason": "worktree" }))
        .collect();
    serde_json::json!({
        "path": "/d/project-map.toml", "schema": 1,
        "projects": [{ "key": "o/a", "index_id": "a", "root_path": "/src/a",
                       "pinned_by": "migration", "pinned_unix": 1 }],
        "parked": parked,
    })
}

/// The group's resolve answer with `duplicates`.
fn resolved(duplicates: Value) -> Value {
    serde_json::json!({
        "index_id": "a", "root_path": "/src/a", "kind": "main_checkout",
        "matched_by": "index_id", "duplicates": duplicates,
    })
}

/// One duplicate row.
fn dup(id: &str, kind: &str, root: &str, parked: bool) -> Value {
    serde_json::json!({ "index_id": id, "kind": kind, "root_path": root, "parked": parked })
}

/// What the park method answers in a test.
#[derive(Clone)]
enum Park {
    /// The daemon's real success shape.
    Ok,
    /// A fixed answer.
    Answer(Value),
    /// An error frame.
    Refuse(RpcError),
    /// Never answers within any test bound.
    Hang,
}

/// A mock daemon plus the `index_id` of every park it received.
async fn daemon(
    map: Result<Value, RpcError>,
    resolve: Value,
    park: Park,
) -> (uds_mock::MockUdsDaemon, Arc<Mutex<Vec<String>>>) {
    let parks = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&parks);
    let d = uds_mock::spawn(move |method: &str, params: Value| -> MockFuture {
        let (map, resolve, park) = (map.clone(), resolve.clone(), park.clone());
        let method = method.to_string();
        let id = params
            .get("index_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if method == "search.project.park" {
            seen.lock().expect("park log").push(id.clone());
        }
        Box::pin(async move {
            match method.as_str() {
                "search.project.map" => map,
                "search.project.resolve" => Ok(resolve),
                "search.project.park" => match park {
                    Park::Ok => Ok(serde_json::json!({
                        "index_id": id, "parked": true, "key": "o/a", "reason": "worktree",
                        "canonical_index_id": "a", "unpinned": [], "unloaded": true,
                    })),
                    Park::Answer(v) => Ok(v),
                    Park::Refuse(e) => Err(e),
                    Park::Hang => {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        Ok(serde_json::json!({}))
                    }
                },
                _ => Err(RpcError::new(-32601, "method not found")),
            }
        })
    })
    .await;
    (d, parks)
}

/// A project directory that is none of the fixture roots.
fn elsewhere() -> tempfile::TempDir {
    tempfile::tempdir().expect("project dir")
}

/// The steps naming `id`.
fn steps_for<'a>(steps: &'a [RepairStep], id: &str) -> Vec<&'a RepairStep> {
    let tick = format!("`{id}`");
    steps.iter().filter(|s| s.what.contains(&tick)).collect()
}

/// A flagged worktree is parked; the live answer, a second main checkout
/// (`search_project_two_checkouts`, not this row) and a parked duplicate are
/// never sent to the park method (#9664).
#[tokio::test]
async fn fix_parks_only_the_flagged_duplicates() {
    let resolve = resolved(serde_json::json!([
        dup("a-wt", "worktree", "/src/a/.worktrees/x", false),
        dup("a2", "main_checkout", "/src/a2", false),
        dup("a-old", "orphaned", "/gone/a", true),
    ]));
    let (d, parks) = daemon(Ok(map_body(&[])), resolve, Park::Ok).await;
    let dir = elsewhere();
    let steps = repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::Apply).await;
    assert_eq!(*parks.lock().unwrap(), vec!["a-wt".to_string()]);
    let wt = steps_for(&steps, "a-wt");
    assert_eq!(wt.len(), 1, "{steps:?}");
    assert!(
        matches!(wt[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert_eq!(wt[0].check, "search_project_duplicate_index");
    for untouched in ["a2", "a-old"] {
        assert!(
            steps_for(&steps, untouched).is_empty(),
            "{untouched}: {steps:?}"
        );
    }
}

/// `--fix` without `--yes` plans the park and sends nothing (#9664).
#[tokio::test]
async fn a_dry_run_plans_the_park_and_calls_no_park() {
    let resolve = resolved(serde_json::json!([dup("a-wt", "worktree", "/wt/a", false)]));
    let (d, parks) = daemon(Ok(map_body(&[])), resolve, Park::Ok).await;
    let dir = elsewhere();
    let steps = repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::DryRun).await;
    assert!(parks.lock().unwrap().is_empty());
    let wt = steps_for(&steps, "a-wt");
    assert_eq!(wt.len(), 1, "{steps:?}");
    assert_eq!(wt[0].status, StepStatus::Planned, "{steps:?}");
}

/// The index of the project `tm doctor` runs in is never parked, even when
/// the row flags it (#9664).
#[tokio::test]
async fn fix_never_parks_the_callers_own_project() {
    let dir = elsewhere();
    let own = trusty_common::resolve_project_root(dir.path());
    let resolve = resolved(serde_json::json!([dup(
        "a-mine",
        "worktree",
        &own.to_string_lossy(),
        false
    )]));
    let (d, parks) = daemon(Ok(map_body(&[])), resolve, Park::Ok).await;
    let steps = repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::Apply).await;
    assert!(parks.lock().unwrap().is_empty(), "{steps:?}");
    let mine = steps_for(&steps, "a-mine");
    assert_eq!(mine.len(), 1, "{steps:?}");
    assert!(
        matches!(mine[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
}

/// Every way a park can fail is a failed step naming the reason, never an
/// applied one (#9664 fail-open check).
#[tokio::test]
async fn a_park_that_fails_is_failed_not_fixed() {
    let cases = [
        (
            "refused",
            Park::Refuse(
                RpcError::new(CODE_CONFLICT, "a-wt is the live index of o/a")
                    .with_data(serde_json::json!({ "error": "park_refused", "message": "live" })),
            ),
            "the live index",
        ),
        (
            "internal error",
            Park::Refuse(RpcError::new(
                CODE_INTERNAL_ERROR,
                "project-map.toml unreadable",
            )),
            "project-map.toml unreadable",
        ),
        (
            "answer without parked",
            Park::Answer(serde_json::json!({ "index_id": "a-wt" })),
            "parked",
        ),
        (
            "parked: false",
            Park::Answer(serde_json::json!({ "index_id": "a-wt", "parked": false })),
            "parked",
        ),
        ("no answer", Park::Hang, "no answer"),
    ];
    let mut failures = Vec::new();
    for (name, park, needle) in cases {
        let resolve = resolved(serde_json::json!([dup("a-wt", "worktree", "/wt/a", false)]));
        let (d, _parks) = daemon(Ok(map_body(&[])), resolve, park).await;
        let dir = elsewhere();
        let steps =
            repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::Apply).await;
        let wt = steps_for(&steps, "a-wt");
        match wt.as_slice() {
            [step] => match &step.status {
                StepStatus::Failed(why) if why.contains(needle) => {}
                other => failures.push(format!("{name}: {other:?}")),
            },
            _ => failures.push(format!("{name}: expected one step, got {steps:?}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A park with no answer is bounded, stops the run, and leaves the next
/// duplicate failed as not attempted rather than skipped (#9664).
#[tokio::test]
async fn a_park_with_no_answer_is_bounded_and_stops_the_run() {
    let resolve = resolved(serde_json::json!([
        dup("a-wt1", "worktree", "/wt/1", false),
        dup("a-wt2", "worktree", "/wt/2", false),
    ]));
    let (d, parks) = daemon(Ok(map_body(&[])), resolve, Park::Hang).await;
    let dir = elsewhere();
    let started = Instant::now();
    let steps = repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::Apply).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(*parks.lock().unwrap(), vec!["a-wt1".to_string()]);
    let second = steps_for(&steps, "a-wt2");
    assert_eq!(second.len(), 1, "{steps:?}");
    match &second[0].status {
        StepStatus::Failed(why) => assert!(why.contains("not attempted"), "{why}"),
        other => panic!("expected a failed step, got {other:?}"),
    }
}

/// An unreadable map, a down daemon, or a daemon without the map method
/// refuses the whole repair with the reason and parks nothing (#9664).
#[tokio::test]
async fn fix_refuses_to_park_when_the_map_is_not_read() {
    let resolve = resolved(serde_json::json!([dup("a-wt", "worktree", "/wt/a", false)]));
    let mut failures = Vec::new();
    for (name, map, needle) in [
        (
            "unreadable",
            Err(RpcError::new(
                CODE_INTERNAL_ERROR,
                "project-map.toml: bad toml",
            )),
            "bad toml",
        ),
        (
            "no map method",
            Err(RpcError::new(-32601, "method not found")),
            "0.60.0",
        ),
    ] {
        let (d, parks) = daemon(map, resolve.clone(), Park::Ok).await;
        let dir = elsewhere();
        let steps =
            repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::Apply).await;
        if !parks.lock().unwrap().is_empty() {
            failures.push(format!("{name}: parked {:?}", parks.lock().unwrap()));
        }
        match steps.as_slice() {
            [step] => match &step.status {
                StepStatus::Refused(why) if why.contains(needle) => {}
                other => failures.push(format!("{name}: {other:?}")),
            },
            _ => failures.push(format!("{name}: expected one step, got {steps:?}")),
        }
    }
    let gone = elsewhere();
    let dir = elsewhere();
    let steps = repair_duplicate_indexes_at(
        &gone.path().join("absent.sock"),
        dir.path(),
        FAST,
        RepairMode::Apply,
    )
    .await;
    match steps.as_slice() {
        [step] if matches!(&step.status, StepStatus::Refused(why) if why.contains("did not answer")) =>
            {}
        _ => failures.push(format!("down daemon: {steps:?}")),
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A group whose resolve was refused is refused by name, so the run never
/// reads as clean while a group went unchecked (#9664).
#[tokio::test]
async fn an_unchecked_group_is_refused_not_clean() {
    let d = uds_mock::spawn(move |method: &str, _params: Value| -> MockFuture {
        let answer = match method {
            "search.project.map" => Ok(map_body(&[])),
            _ => Err(RpcError::new(CODE_INTERNAL_ERROR, "registry unreadable")),
        };
        Box::pin(async move { answer })
    })
    .await;
    let dir = elsewhere();
    let steps = repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::Apply).await;
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].what.contains("`o/a`"), "{steps:?}");
    match &steps[0].status {
        StepStatus::Refused(why) => assert!(why.contains("registry unreadable"), "{why}"),
        other => panic!("expected a refused step, got {other:?}"),
    }
}

/// An index the map already lists as parked is reported as already parked
/// and never sent to the park method again (#9664).
#[tokio::test]
async fn an_already_parked_index_is_reported_not_parked_again() {
    // The resolve still says live: the race between the two reads.
    let resolve = resolved(serde_json::json!([dup("a-wt", "worktree", "/wt/a", false)]));
    let (d, parks) = daemon(Ok(map_body(&["a-wt"])), resolve, Park::Ok).await;
    let dir = elsewhere();
    let steps = repair_duplicate_indexes_at(d.socket(), dir.path(), FAST, RepairMode::Apply).await;
    assert!(parks.lock().unwrap().is_empty(), "{steps:?}");
    let wt = steps_for(&steps, "a-wt");
    assert_eq!(wt.len(), 1, "{steps:?}");
    assert!(wt[0].what.contains("already parked"), "{steps:?}");
    assert!(
        matches!(wt[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
}

/// The blocking entry `tm doctor --fix` calls runs inside a tokio runtime
/// without panicking, against the socket `TRUSTY_SEARCH_SOCKET` names (#9664).
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn the_blocking_entry_runs_inside_a_runtime() {
    let resolve = resolved(serde_json::json!([dup("a-wt", "worktree", "/wt/a", false)]));
    let (d, parks) = daemon(Ok(map_body(&[])), resolve, Park::Ok).await;
    // SAFETY: `#[serial]` keeps env writers apart.
    unsafe {
        std::env::set_var(
            crate::daemon::search_rpc::TRUSTY_SEARCH_SOCKET_ENV,
            d.socket(),
        );
    }
    let dir = elsewhere();
    let steps = repair_duplicate_indexes(dir.path(), RepairMode::Apply);
    unsafe {
        std::env::remove_var(crate::daemon::search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
    }
    assert_eq!(
        *parks.lock().unwrap(),
        vec!["a-wt".to_string()],
        "{steps:?}"
    );
}
