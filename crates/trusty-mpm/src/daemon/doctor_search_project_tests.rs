//! Unit tests for `daemon::doctor_search_project` (#9169 B2).
//!
//! Why: split out so the production module stays under the 500-SLOC cap.
//! What: every row's healthy, problem and not-checked states, over a real
//! socket where the transport matters and through the pure builders where it
//! does not.
//! Test: this module IS the test suite for `super`.

use std::time::{Duration, Instant};

use super::*;
use crate::uds_mock::{self, MockFuture, RpcError};

/// Limits small enough for a test to hit each bound quickly.
const FAST: FanoutLimits = FanoutLimits {
    max_groups: 8,
    per_call: Duration::from_millis(300),
    budget: Duration::from_secs(5),
};

/// A map body pinning `ids`, each in its own group, by `pinned_by`.
fn map_body(ids: &[&str], pinned_by: &str) -> Value {
    let projects: Vec<Value> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "key": format!("o/{id}"), "index_id": id, "root_path": format!("/src/{id}"),
                "pinned_by": pinned_by, "pinned_unix": 1,
            })
        })
        .collect();
    serde_json::json!({ "path": "/d/project-map.toml", "schema": 1, "projects": projects, "parked": [] })
}

/// A resolve answer for `id` at `/src/<id>` with `duplicates`.
fn resolved(id: &str, duplicates: Value) -> Value {
    serde_json::json!({
        "index_id": id, "root_path": format!("/src/{id}"), "kind": "main_checkout",
        "matched_by": "index_id", "duplicates": duplicates,
    })
}

/// One duplicate row.
fn dup(id: &str, kind: &str, root: &str, parked: bool) -> Value {
    serde_json::json!({ "index_id": id, "kind": kind, "root_path": root, "parked": parked })
}

/// A mock daemon: `map` answers the map; `resolve` answers every resolve.
async fn daemon(
    map: Result<Value, RpcError>,
    resolve: Result<Value, RpcError>,
) -> uds_mock::MockUdsDaemon {
    uds_mock::spawn(move |method: &str, _params: Value| -> MockFuture {
        let answer = match method {
            METHOD_MAP => map.clone(),
            METHOD_RESOLVE => resolve.clone(),
            _ => Err(RpcError::new(CODE_METHOD_NOT_FOUND, "method not found")),
        };
        Box::pin(async move { answer })
    })
    .await
}

/// The row named `name`.
fn row<'a>(rows: &'a [DoctorCheck], name: &str) -> &'a DoctorCheck {
    rows.iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} row"))
}

fn pin(by: &str) -> Pin {
    Pin {
        key: "o/a".into(),
        index_id: "a".into(),
        pinned_by: by.into(),
    }
}

#[tokio::test]
async fn a_healthy_daemon_reports_four_ok_rows() {
    let d = daemon(
        Ok(map_body(&["a"], "operator")),
        Ok(resolved("a", serde_json::json!([]))),
    )
    .await;
    let project = tempfile::tempdir().unwrap();
    let rows = check_search_project_at(d.socket(), Some(project.path()), FAST).await;
    let names: Vec<&str> = rows.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, [NO_INDEX, DUPLICATE, TWO_CHECKOUTS, MAP_UNREADABLE]);
    for r in &rows {
        assert_eq!(r.status, CheckStatus::Ok, "{}: {}", r.name, r.message);
    }
}

#[tokio::test]
async fn a_down_daemon_is_not_checked_on_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let rows =
        check_search_project_at(&dir.path().join("none.sock"), Some(project.path()), FAST).await;
    assert_eq!(rows.len(), 4);
    for r in &rows {
        assert_eq!(r.status, CheckStatus::Warn, "{}: {}", r.name, r.message);
        assert!(r.message.starts_with("not checked"), "{}", r.message);
    }
}

#[tokio::test]
async fn a_daemon_without_the_map_method_is_not_checked() {
    let d = daemon(
        Err(RpcError::new(CODE_METHOD_NOT_FOUND, "method not found")),
        Ok(resolved("a", serde_json::json!([]))),
    )
    .await;
    let rows = check_search_project_at(d.socket(), None, FAST).await;
    let map = row(&rows, MAP_UNREADABLE);
    assert_eq!(map.status, CheckStatus::Warn);
    assert!(map.message.contains("0.60.0"), "{}", map.message);
    for name in [NO_INDEX, DUPLICATE, TWO_CHECKOUTS] {
        let r = row(&rows, name);
        assert_eq!(r.status, CheckStatus::Warn, "{name}: {}", r.message);
        assert!(r.message.starts_with("not checked"), "{}", r.message);
    }
}

/// Fail-Open: a group whose resolve is refused is reported, never `Ok`.
#[tokio::test]
async fn a_group_whose_resolve_fails_is_not_ok() {
    let d = daemon(
        Ok(map_body(&["a"], "migration")),
        Err(RpcError::new(CODE_INTERNAL_ERROR, "registry unreadable")),
    )
    .await;
    let rows = check_search_project_at(d.socket(), None, FAST).await;
    for name in [DUPLICATE, TWO_CHECKOUTS] {
        let r = row(&rows, name);
        assert_eq!(r.status, CheckStatus::Warn, "{name}: {}", r.message);
        assert!(
            r.message.contains("not checked for 1 group(s): `o/a`"),
            "{}",
            r.message
        );
    }
}

#[tokio::test]
async fn a_resolve_that_times_out_stops_the_fanout_and_is_not_ok() {
    let map = map_body(&["a", "b", "c"], "migration");
    let d = uds_mock::spawn(move |method: &str, _params: Value| -> MockFuture {
        let map = map.clone();
        let method = method.to_string();
        Box::pin(async move {
            if method == METHOD_MAP {
                return Ok(map);
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(resolved("a", serde_json::json!([])))
        })
    })
    .await;
    let started = Instant::now();
    let rows = check_search_project_at(d.socket(), None, FAST).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let r = row(&rows, DUPLICATE);
    assert_eq!(r.status, CheckStatus::Warn, "{}", r.message);
    assert!(
        r.message.contains("not checked for 3 group(s)"),
        "{}",
        r.message
    );
    assert!(r.message.contains("stopped answering"), "{}", r.message);
}

#[tokio::test]
async fn groups_past_the_cap_are_not_checked_not_ok() {
    let d = daemon(
        Ok(map_body(&["a", "b"], "migration")),
        Ok(resolved("a", serde_json::json!([]))),
    )
    .await;
    let limits = FanoutLimits {
        max_groups: 1,
        ..FAST
    };
    let rows = check_search_project_at(d.socket(), None, limits).await;
    let r = row(&rows, DUPLICATE);
    assert_eq!(r.status, CheckStatus::Warn, "{}", r.message);
    assert!(
        r.message.contains("`o/b` (past the 1-group cap)"),
        "{}",
        r.message
    );
}

#[test]
fn two_main_checkouts_warn_until_an_operator_pins_one() {
    let body = resolved(
        "a",
        serde_json::json!([dup("a2", "main_checkout", "/src/a2", false)]),
    );
    let map = MapRead::Readable {
        path: "m".into(),
        pins: vec![pin("migration")],
    };
    for (by, want) in [
        ("migration", CheckStatus::Warn),
        ("operator", CheckStatus::Ok),
    ] {
        let fanout = Fanout {
            groups: vec![classify(&pin(by), &body).unwrap()],
            unchecked: vec![],
        };
        let [dups, two, _] = build_group_rows(&map, Some(&fanout));
        assert_eq!(dups.status, CheckStatus::Ok, "{}", dups.message);
        assert_eq!(two.status, want, "{by}: {}", two.message);
        if want == CheckStatus::Warn {
            assert!(two.message.contains("`a2`"), "{}", two.message);
            assert!(two.message.contains("project pin"), "{}", two.message);
        }
    }
}

#[test]
fn a_same_root_sibling_is_a_duplicate_not_a_checkout() {
    let body = resolved(
        "a",
        serde_json::json!([dup("a-1a2b3c4d", "main_checkout", "/src/a", false)]),
    );
    let finding = classify(&pin("migration"), &body).unwrap();
    assert_eq!(finding.parkable.len(), 1, "{finding:?}");
    assert!(finding.checkouts.is_empty(), "{finding:?}");
}

#[test]
fn a_parked_duplicate_is_ok() {
    let body = resolved(
        "a",
        serde_json::json!([dup("a-wt", "worktree", "/wt/a", true)]),
    );
    let finding = classify(&pin("migration"), &body).unwrap();
    assert_eq!(
        finding,
        GroupFinding {
            key: "o/a".into(),
            answer: "a".into(),
            pinned_by: "migration".into(),
            ..GroupFinding::default()
        }
    );
}

#[test]
fn a_project_with_no_index_is_warn() {
    let rpc = Rpc::Refused {
        code: CODE_NOT_FOUND,
        message: "no index matches".into(),
        data: Some(serde_json::json!({ "error": "project_not_found", "candidates": [] })),
    };
    let r = build_no_index(Some(Path::new("/src/x")), Some(&rpc));
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(
        r.message.contains("trusty-search index /src/x"),
        "{}",
        r.message
    );
}

#[test]
fn a_project_with_only_worktree_indexes_is_warn() {
    let rpc = Rpc::Refused {
        code: CODE_NOT_FOUND,
        message: "only worktree".into(),
        data: Some(serde_json::json!({ "error": "no_live_index", "candidates": [] })),
    };
    let r = build_no_index(Some(Path::new("/src/x")), Some(&rpc));
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains("main checkout"), "{}", r.message);
}

/// Fail-Open (#9169 critic HIGH): a resolve that answers but lacks a field the
/// verdict reads is not checked, never a clean group.
#[tokio::test]
async fn a_malformed_resolve_answer_is_not_checked_not_clean() {
    let cases = [
        (
            "duplicates",
            serde_json::json!({ "index_id": "a", "root_path": "/src/a" }),
        ),
        (
            "duplicates",
            serde_json::json!({ "index_id": "a", "root_path": "/src/a", "duplicates": {} }),
        ),
        (
            "index_id",
            serde_json::json!({ "root_path": "/src/a", "duplicates": [] }),
        ),
        (
            "root_path",
            serde_json::json!({ "index_id": "a", "duplicates": [] }),
        ),
        (
            "kind",
            serde_json::json!({ "index_id": "a", "root_path": "/src/a",
                "duplicates": [{ "index_id": "b", "root_path": "/src/b", "parked": false }] }),
        ),
    ];
    // Every case runs, so one failing case never hides another.
    let mut failures = Vec::new();
    for (field, body) in cases {
        let d = daemon(Ok(map_body(&["a"], "migration")), Ok(body.clone())).await;
        let rows = check_search_project_at(d.socket(), None, FAST).await;
        for name in [DUPLICATE, TWO_CHECKOUTS] {
            let r = row(&rows, name);
            let reason = format!("resolve answered without `{field}`");
            if r.status != CheckStatus::Warn || !r.message.contains(&reason) {
                failures.push(format!("{name} over {body}: {:?} {}", r.status, r.message));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Hang guard: the total budget, not the per-call bound, stops a fan-out of
/// slow answers, and the groups it cut off say so.
#[tokio::test]
async fn the_fanout_budget_cuts_off_slow_groups_as_not_checked() {
    let map = map_body(&["a", "b", "c", "d"], "migration");
    let d = uds_mock::spawn(move |method: &str, _params: Value| -> MockFuture {
        let map = map.clone();
        let method = method.to_string();
        Box::pin(async move {
            if method == METHOD_MAP {
                return Ok(map);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(resolved("a", serde_json::json!([])))
        })
    })
    .await;
    let limits = FanoutLimits {
        max_groups: 8,
        per_call: Duration::from_millis(300),
        budget: Duration::from_millis(50),
    };
    let started = Instant::now();
    let rows = check_search_project_at(d.socket(), None, limits).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let r = row(&rows, DUPLICATE);
    assert_eq!(r.status, CheckStatus::Warn, "{}", r.message);
    assert!(r.message.contains("past the 50ms budget"), "{}", r.message);
    assert!(
        r.message.contains("`o/d` (past the 50ms budget)"),
        "{}",
        r.message
    );
}

/// Fail-Open error arms of `read_map`: a map answer tm cannot use leaves every
/// map row not-ok.
#[tokio::test]
async fn a_map_answer_that_cannot_be_read_is_never_ok() {
    let pin_row = |key: Option<&str>, id: Option<&str>| {
        let mut row = serde_json::json!({ "root_path": "/src/a", "pinned_by": "migration" });
        if let Some(k) = key {
            row["key"] = serde_json::json!(k);
        }
        if let Some(i) = id {
            row["index_id"] = serde_json::json!(i);
        }
        serde_json::json!({ "path": "/d/m.toml", "schema": 1, "projects": [row], "parked": [] })
    };
    let cases: [(Result<Value, RpcError>, &str); 4] = [
        (
            Ok(serde_json::json!({ "path": "/d/m.toml", "schema": 1 })),
            "without a `projects` list",
        ),
        (Ok(pin_row(None, Some("a"))), "a pin without a key or id"),
        (Ok(pin_row(Some("o/a"), None)), "a pin without a key or id"),
        (
            Err(RpcError::new(CODE_NOT_FOUND, "nope")),
            "refused (-32004): nope",
        ),
    ];
    // Every case runs, so one failing case never hides another.
    let mut failures = Vec::new();
    for (answer, why) in cases {
        let d = daemon(answer, Ok(resolved("a", serde_json::json!([])))).await;
        let rows = check_search_project_at(d.socket(), None, FAST).await;
        let map = row(&rows, MAP_UNREADABLE);
        if map.status != CheckStatus::Warn || !map.message.contains(why) {
            failures.push(format!("map ({why}): {:?} {}", map.status, map.message));
        }
        for name in [DUPLICATE, TWO_CHECKOUTS] {
            let r = row(&rows, name);
            if r.status != CheckStatus::Warn || !r.message.starts_with("not checked") {
                failures.push(format!("{name} ({why}): {:?} {}", r.status, r.message));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
