//! #10403: `pending`, `stale_tasks`, `goal_sessions`, `ordered`,
//! `last_activity` and `run_progress` against goldens the live
//! `dashboard_derive.py` wrote (see `tests/fixtures/dashboard/pending/README.md`).

use serde_json::{Value, json};
use trusty_architect::dashboard::{
    Doc, Snapshot, goal_sessions, last_activity, ordered, pending, run_progress, stale_tasks,
};
use trusty_architect::to_python_json;

const DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/dashboard/pending/"
);

fn load(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{DIR}{name}")).expect("fixture");
    serde_json::from_str(&text).expect("fixture json")
}

fn docs(rows: &Value) -> Vec<Doc> {
    rows.as_array()
        .expect("rows")
        .iter()
        .map(|r| Doc {
            id: r["id"].as_str().expect("id").to_owned(),
            version: 1,
            data: r["data"].as_object().cloned().expect("data"),
        })
        .collect()
}

fn py(v: &impl serde::Serialize) -> String {
    to_python_json(v, true).expect("json")
}

const NOW: f64 = 1_791_763_200.0;

fn snapshot(v: &Value) -> Snapshot {
    v.as_object()
        .expect("snap")
        .iter()
        .map(|(k, rows)| (k.clone(), docs(rows)))
        .collect()
}

#[test]
fn pending_parity_goldens() {
    let goldens = load("goldens.json");
    let cases = load("cases.json");
    let mut kinds = std::collections::BTreeSet::new();
    for case in cases.as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let got = pending(&snapshot(&case["snap"]), case["now"].as_f64().expect("now"));
        kinds.extend(got.events().iter().map(|e| e.kind.clone()));
        assert_eq!(
            py(&got.events()),
            goldens["pending"][name].as_str().expect("golden"),
            "case {name}"
        );
    }
    assert_eq!(kinds.len(), 17, "one fixture per event kind: {kinds:?}");

    let d = load("derive_cases.json");
    let g = &goldens["derive"];
    let items = docs(&d["items"]);
    let stale: Vec<Value> = stale_tasks(&items, NOW, 24.0)
        .tasks()
        .iter()
        .map(|t| json!([t.last, t.id, t.session, t.title]))
        .collect();
    assert_eq!(py(&stale), g["stale_tasks"].as_str().expect("golden"));
    let last: Vec<Value> = items
        .iter()
        .map(|i| json!([i.id, last_activity(&i.data)]))
        .collect();
    assert_eq!(py(&last), g["last_activity"].as_str().expect("golden"));
    let gs = goal_sessions(&docs(&d["goals"]), &docs(&d["sessions"]));
    assert_eq!(
        py(&(gs.by_goal, gs.by_session)),
        g["goal_sessions"].as_str().expect("golden")
    );
    let ranked = docs(&d["ranked"]);
    let ids: Vec<&str> = ordered(&ranked).iter().map(|d| d.id.as_str()).collect();
    assert_eq!(py(&ids), g["ordered"].as_str().expect("golden"));
    // A tuple, not `json!`: `json!` objects sort their keys.
    let runs: Vec<_> = docs(&d["runs"])
        .iter()
        .map(|r| (r.id.clone(), run_progress(&r.data, NOW)))
        .collect();
    assert_eq!(py(&runs), g["run_progress"].as_str().expect("golden"));
}

#[test]
fn pending_missing_collections_yield_nothing() {
    assert!(pending(&Snapshot::new(), NOW).events().is_empty());
    let mut only_goals = Snapshot::new();
    only_goals.insert("goals".into(), Vec::new());
    only_goals.insert(
        "unknown".into(),
        docs(&json!([{"id": "Z01", "data": {"status": "answered"}}])),
    );
    let got = pending(&only_goals, NOW);
    assert!(got.events().is_empty() && got.skipped().is_empty());
}

#[test]
fn pending_is_deterministic() {
    let cases = load("cases.json");
    for case in cases.as_array().expect("cases") {
        let snap = snapshot(&case["snap"]);
        assert_eq!(pending(&snap, NOW), pending(&snap, NOW));
    }
}

#[test]
fn pending_unparseable_until_is_skipped() {
    let snap = snapshot(&json!({"items": [
        {"id": "P06", "data": {"status": "open", "parked": {"until": "next tuesday"}}},
        {"id": "P07", "data": {"status": "open", "parked": {"until": "2026-10-01"}}}
    ]}));
    let got = pending(&snap, NOW);
    // Parity: the unparseable until raises no event (Python's `_due` is false).
    let ids: Vec<(&str, &str)> = got
        .events()
        .iter()
        .map(|e| (e.id.as_str(), e.kind.as_str()))
        .collect();
    assert_eq!(ids, [("P07", "owner_unpark_due")]);
    let skipped = py(&got.skipped());
    assert_eq!(skipped, r#"[{"id": "P06", "field": "parked.until"}]"#);
}

#[test]
fn stale_unparseable_updated_is_skipped() {
    let items = docs(&json!([
        {"id": "T01", "data": {"kind": "task", "status": "open", "updated": "garbage",
            "updates": [{"at": "2026-10-01"}, {"at": 5}, {"note": "no at"}]}},
        {"id": "T02", "data": {"kind": "task", "status": "open"}}
    ]));
    let got = stale_tasks(&items, NOW, 24.0);
    let ids: Vec<&str> = got.tasks().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T02", "T01"]);
    assert_eq!(
        py(&got.skipped()),
        r#"[{"id": "T01", "field": "updated"}, {"id": "T01", "field": "updates[1].at"}]"#
    );
}
