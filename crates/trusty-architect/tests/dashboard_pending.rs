//! #10403: `goal_sessions`, `ordered` and `run_progress` against goldens the
//! live `dashboard_derive.py` wrote (see `tests/fixtures/dashboard/pending/README.md`).
//! #12029 P2e deleted the serde `pending`, `stale_tasks` and `last_activity`;
//! the `dashboard_derive` and `owner_answers` recordings cover the port.

use serde_json::Value;
use trusty_architect::dashboard::{Doc, goal_sessions, ordered, run_progress};
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

#[test]
fn derive_parity_goldens() {
    let goldens = load("goldens.json");
    let d = load("derive_cases.json");
    let g = &goldens["derive"];
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
