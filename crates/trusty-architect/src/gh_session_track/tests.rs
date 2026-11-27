//! Unit tests for the `gh-session-track` port (#12087).
//!
//! The recordings in `tests/replay_gh_session_track.rs` pin the script's
//! output and written bytes. These cover what a recording cannot: a version
//! conflict needs a second writer between the read and the batch, and the
//! secret refusal is the Rust store's own.

use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use super::plan::{Entry, Fetched, PrStats, Session, SessionMap, map_session, pr_state};
use super::render::merged_details;
use super::write::{Fail, Pass, apply};
use crate::clock::FakeClock;
use crate::dashboard::Store;
use crate::dashboard::store::Format;
use crate::pyjson_load::{PyValue, loads};

/// 2026-11-27T13:00:00Z, the recordings' clock.
const T: f64 = 1_795_784_400.0;
const NOW: &str = "2026-11-27T13:00:00Z";

fn v(text: &str) -> PyValue {
    loads(text).expect("test JSON parses")
}

fn names(n: &[&str]) -> Vec<String> {
    n.iter().map(|s| (*s).to_owned()).collect()
}

fn no_secret(_: &str) -> Option<String> {
    None
}

/// A tempdir store with `alpha` (version 1, `updated` an hour before `T`).
fn store_with_alpha(dir: &Path) -> Store {
    let store = Store::new(dir, Format::Dir)
        .with_secret_scan(no_secret)
        .with_clock(Arc::new(FakeClock::new(T - 3600.0)));
    let doc = json!({"name": "alpha", "project": "sample-app", "updated": "x", "details": {"note": "kept"}});
    store
        .set("sessions", "alpha", doc, Some(0))
        .expect("seed alpha");
    store
}

fn one_pr() -> Fetched {
    Fetched {
        prs: vec![v(
            r#"{"number": 7, "url": "u7", "labels": [{"name": "ws/alpha"}], "mergeStateStatus": "CLEAN"}"#,
        )],
        issues: Vec::new(),
        merged: Vec::new(),
        closed: Vec::new(),
    }
}

fn read(dir: &Path, rel: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join(rel)).expect("doc exists");
    serde_json::from_str(&text).expect("doc parses")
}

#[test]
fn map_session_prefers_ws_then_crate_then_fallback() {
    let map = SessionMap {
        crates: vec![("sample-crate".into(), "crate-01".into())],
        fallback: Some("catchall".into()),
        mark: String::new(),
    };
    let all = names(&[
        "alpha", "beta-01", "gamma-01", "gamma-02", "crate-01", "catchall",
    ]);
    let item = |labels: &str| v(&format!(r#"{{"labels": [{labels}]}}"#));
    let at = |labels: &str, ns: &[String]| map_session(&item(labels), ns, &map);
    assert_eq!(
        at(r#"{"name": "ws/alpha"}, {"name": "sample-crate"}"#, &all).as_deref(),
        Some("alpha")
    );
    assert_eq!(
        at(r#"{"name": "ws/beta"}"#, &all).as_deref(),
        Some("beta-01")
    );
    assert_eq!(
        at(r#"{"name": "ws/gamma"}, {"name": "sample-crate"}"#, &all).as_deref(),
        Some("crate-01")
    );
    assert_eq!(
        at(r#"{"name": "ws/none"}"#, &all).as_deref(),
        Some("catchall")
    );
    let few = names(&["alpha"]);
    assert_eq!(at(r#"{"name": "sample-crate"}"#, &few), None);
}

#[test]
fn pr_state_priority_order() {
    let red = r#"[{"conclusion": "FAILURE", "status": "COMPLETED"}]"#;
    let pend = r#"[{"status": "IN_PROGRESS", "conclusion": ""}]"#;
    let pr = |draft: bool, checks: &str, ms: &str, labels: &str| {
        pr_state(&v(&format!(
            r#"{{"isDraft": {draft}, "statusCheckRollup": {checks}, "mergeStateStatus": {ms}, "labels": [{labels}]}}"#
        )))
    };
    let hold = r#"{"name": "do-not-merge"}"#;
    assert_eq!(pr(true, red, r#""DIRTY""#, ""), "draft");
    assert_eq!(pr(false, red, r#""DIRTY""#, ""), "red");
    assert_eq!(pr(false, pend, r#""DIRTY""#, ""), "running");
    assert_eq!(
        pr(false, r#"[{"state": "EXPECTED"}]"#, r#""CLEAN""#, ""),
        "running"
    );
    assert_eq!(pr(false, "[]", r#""DIRTY""#, hold), "dirty");
    assert_eq!(pr(false, "[]", r#""CLEAN""#, hold), "hold");
    assert_eq!(pr(false, "[]", r#""CLEAN""#, ""), "ready");
    assert_eq!(pr(false, "[]", r#""BEHIND""#, ""), "behind");
    assert_eq!(pr(false, "[]", "null", ""), "unknown");
}

#[test]
fn merged_details_keeps_other_keys_and_drops_empty_lists() {
    let old = v(r#"{"note": "kept", "prs": [1], "issues": {"open": 2}, "gh_at": "old"}"#);
    let entry = Entry {
        prs: Vec::new(),
        issues: None,
        pr_stats: PrStats {
            open: 0,
            by_state: Vec::new(),
            merged_24h: 0,
            merged_7d: 0,
            capped: false,
        },
    };
    let got = crate::pyval::dumps(&merged_details(Some(&old), &entry, NOW), None, true);
    let want = r#"{"note": "kept", "gh_at": "2026-11-27T13:00:00Z", "pr_stats": {"open": 0, "by_state": {}, "merged_24h": 0, "merged_7d": 0, "merged_7d_capped": false}}"#;
    assert_eq!(got, want);
}

// #12087: the frozen Python store keeps a seeded `updated`; this store
// restamps it with the batch clock (#10678). The write recordings rely on it:
// they seed `updated` at the recorded instant, so both sides write one value.
#[test]
fn store_batch_restamps_updated_with_the_batch_clock() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let seed = Store::new(tmp.path(), Format::Dir).with_secret_scan(no_secret);
    let doc = json!({"name": "alpha", "updated": "2026-11-01T00:00:00Z"});
    seed.set("sessions", "alpha", doc, Some(0)).expect("seed");
    let store = Store::new(tmp.path(), Format::Dir)
        .with_secret_scan(no_secret)
        .with_clock(Arc::new(FakeClock::new(T)));
    store
        .update("sessions", "alpha", json!({"details": {}}), Some(1))
        .expect("batch");
    assert_eq!(read(tmp.path(), "sessions/alpha.json")["updated"], NOW);
}

fn pass(map: &SessionMap) -> Pass<'_> {
    Pass {
        repo: "example-org/example-repo",
        now: NOW,
        map,
    }
}

#[test]
fn gh_session_track_retries_a_conflict_then_writes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = store_with_alpha(tmp.path());
    let root = tmp.path().to_string_lossy().into_owned();
    let map = SessionMap::default();
    let mut attempts = Vec::new();
    let plan = apply(&store, &root, &one_pr(), &pass(&map), &mut |attempt| {
        attempts.push(attempt);
        if attempt == 0 {
            // A second writer moves alpha to version 2 between read and batch.
            store
                .update("sessions", "alpha", json!({"status": "moved"}), None)
                .expect("bump");
        }
    })
    .expect("the second attempt writes");
    assert_eq!(attempts, [0, 1]);
    assert_eq!(plan.sessions.len(), 1);
    let alpha = read(tmp.path(), "sessions/alpha.json");
    assert_eq!(alpha["version"], 3);
    assert_eq!(alpha["status"], "moved");
    assert_eq!(alpha["details"]["note"], "kept");
    assert_eq!(alpha["details"]["prs"][0]["n"], 7);
    assert_eq!(
        read(tmp.path(), "meta/gh_track.json")["repo"],
        "example-org/example-repo"
    );
}

#[test]
fn gh_session_track_gives_up_after_three_conflicts_and_writes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = store_with_alpha(tmp.path());
    let root = tmp.path().to_string_lossy().into_owned();
    let map = SessionMap::default();
    let mut attempts = 0;
    let got = apply(&store, &root, &one_pr(), &pass(&map), &mut |_| {
        attempts += 1;
        store
            .update("sessions", "alpha", json!({"status": "moved"}), None)
            .expect("bump");
    });
    let Err(Fail::Store(msg)) = got else {
        panic!("want the last conflict, got {got:?}");
    };
    assert_eq!(attempts, 3);
    assert!(
        msg.contains("sessions/alpha: if_version 3 but current version is 4"),
        "{msg}"
    );
    let alpha = read(tmp.path(), "sessions/alpha.json");
    assert_eq!(alpha["version"], 4);
    assert!(
        alpha["details"].get("gh_at").is_none(),
        "no batch of this pass landed"
    );
    assert!(!tmp.path().join("meta/gh_track.json").exists());
}

#[test]
fn gh_session_track_secret_refusal_leaves_the_store_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    store_with_alpha(tmp.path());
    let before = std::fs::read(tmp.path().join("sessions/alpha.json")).expect("seeded");
    let store = Store::new(tmp.path(), Format::Dir)
        .with_secret_scan(|s: &str| s.contains("sample-secret").then(|| "sample shape".into()))
        .with_clock(Arc::new(FakeClock::new(T)));
    let mut fetched = one_pr();
    fetched.prs[0] =
        v(r#"{"number": 7, "url": "sample-secret", "labels": [{"name": "ws/alpha"}]}"#);
    let root = tmp.path().to_string_lossy().into_owned();
    let map = SessionMap::default();
    let got = apply(&store, &root, &fetched, &pass(&map), &mut |_| {});
    assert!(
        matches!(&got, Err(Fail::Store(m)) if m.contains("secret-shaped")),
        "{got:?}"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("sessions/alpha.json")).expect("still there"),
        before
    );
    assert!(!tmp.path().join("meta/gh_track.json").exists());
}

#[test]
fn sessions_tracked_by_the_mark_are_planned_in_name_order() {
    let s = |name: &str| Session {
        name: name.into(),
        id: name.into(),
        version: 1,
        data: v(r#"{"project": "sample-app/example-repo"}"#),
    };
    let map = SessionMap {
        mark: "example-repo".into(),
        ..SessionMap::default()
    };
    let plan =
        super::plan::build_plan(&one_pr(), &[s("beta"), s("alpha")], NOW, &map).expect("plan");
    let planned: Vec<&str> = plan.sessions.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        planned,
        ["alpha", "beta"],
        "tracked by the mark, in name order"
    );
}
