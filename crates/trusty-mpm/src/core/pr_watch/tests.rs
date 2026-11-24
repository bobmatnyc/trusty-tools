//! Tests for the PR watch table and transition rule (#11673).

use super::gh::parse_pr_view;
use super::table::{Table, Upsert, WatchRow};
use super::verdict::{Kind, Verdict, dedupe_key, detect, judge};
use crate::core::pr_rollup::RollupEntry;

fn runs(json: &str) -> Vec<RollupEntry> {
    serde_json::from_str(json).expect("valid rollup JSON")
}

fn req(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

pub(crate) fn row(head: &str) -> WatchRow {
    WatchRow {
        repo: "o/r".into(),
        pr: 7,
        head: head.into(),
        base: "main".into(),
        required: req(&["ci"]),
        owner: "pm-a".into(),
        project: None,
        verdict: None,
        emitted_key: None,
        ledger_key: None,
        last_woken_key: None,
        added_at: "2026-11-23T00:00:00Z".into(),
    }
}

#[test]
fn judge_reads_only_required_contexts() {
    let r = runs(
        r#"[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"},
            {"name":"lint","status":"COMPLETED","conclusion":"FAILURE"}]"#,
    );
    assert_eq!(judge(&r, &req(&["ci"])), Verdict::Green);
    assert_eq!(judge(&r, &[]), Verdict::Red, "no required list judges all");
}

#[test]
fn judge_red_wins_over_pending() {
    let r = runs(
        r#"[{"name":"a","status":"COMPLETED","conclusion":"FAILURE"},
            {"name":"b","status":"IN_PROGRESS","conclusion":""}]"#,
    );
    assert_eq!(judge(&r, &req(&["a", "b"])), Verdict::Red);
    assert_eq!(judge(&r, &req(&["a", "c"])), Verdict::Red);
}

#[test]
fn judge_missing_required_is_pending() {
    let r = runs(r#"[{"name":"a","status":"COMPLETED","conclusion":"SUCCESS"}]"#);
    assert_eq!(judge(&r, &req(&["a", "b"])), Verdict::Pending);
    assert_eq!(judge(&[], &[]), Verdict::Pending);
}

#[test]
fn judge_uses_the_latest_run() {
    let r = runs(
        r#"[{"name":"ci","status":"COMPLETED","conclusion":"FAILURE","completedAt":"2026-11-23T01:00:00Z"},
            {"name":"ci","status":"COMPLETED","conclusion":"SUCCESS","completedAt":"2026-11-23T02:00:00Z"}]"#,
    );
    assert_eq!(judge(&r, &req(&["ci"])), Verdict::Green);
    let rerun = runs(
        r#"[{"name":"ci","status":"COMPLETED","conclusion":"FAILURE","completedAt":"2026-11-23T01:00:00Z"},
            {"name":"ci","status":"QUEUED","conclusion":""}]"#,
    );
    assert_eq!(judge(&rerun, &req(&["ci"])), Verdict::Pending);
}

#[test]
fn detect_names_each_transition() {
    use Verdict::*;
    assert_eq!(detect("a", None, "a", Green), Some(Kind::Green));
    assert_eq!(detect("a", Some(Pending), "a", Red), Some(Kind::Red));
    assert_eq!(detect("a", Some(Red), "a", Pending), Some(Kind::Rerun));
    assert_eq!(detect("a", Some(Green), "b", Pending), Some(Kind::NewHead));
    assert_eq!(
        dedupe_key("O/R", 7, "ABC", Kind::NewHead),
        "o/r#7@abc:new_head"
    );
}

#[test]
fn detect_ignores_no_change() {
    use Verdict::*;
    assert_eq!(detect("a", Some(Green), "a", Green), None);
    assert_eq!(detect("a", Some(Green), "a", Pending), None);
    assert_eq!(detect("a", None, "a", Pending), None);
    assert_eq!(detect("a", Some(Pending), "a", Pending), None);
}

#[test]
fn parse_pr_view_reads_head_base_and_rollup() {
    let s = parse_pr_view(
        r#"{"state":"OPEN","headRefOid":"abc","baseRefName":"main",
            "statusCheckRollup":[{"name":"ci","status":"QUEUED"}]}"#,
    )
    .expect("parses");
    assert_eq!((s.head.as_str(), s.base.as_str()), ("abc", "main"));
    assert_eq!(s.rollup.len(), 1);
    assert!(!s.is_done());
    assert!(parse_pr_view(r#"{"state":"OPEN"}"#).is_err());
}

#[test]
fn table_upsert_keeps_delivery_marks_on_the_same_head() {
    let dir = tempfile::tempdir().expect("tempdir");
    let table = Table::at(dir.path());
    assert_eq!(table.upsert(row("h1")).expect("add"), Upsert::Added);
    table
        .update("o/r", 7, |r| {
            r.last_woken_key = Some("k".into());
            r.verdict = Some(Verdict::Green);
        })
        .expect("update");
    let mut again = row("h1");
    again.owner = "pm-b".into();
    assert_eq!(table.upsert(again).expect("re-add"), Upsert::Updated);
    let rows = table.list().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].owner, "pm-b");
    assert_eq!(rows[0].last_woken_key.as_deref(), Some("k"));
    assert_eq!(rows[0].verdict, Some(Verdict::Green));
    table.upsert(row("h2")).expect("new head");
    let rows = table.list().expect("list");
    assert_eq!((rows[0].head.as_str(), rows[0].verdict), ("h2", None));
}

#[test]
fn table_update_after_remove_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let table = Table::at(dir.path());
    assert!(table.list().expect("empty").is_empty());
    table.upsert(row("h1")).expect("add");
    assert!(table.remove("O/R", 7).expect("rm"));
    assert!(!table.remove("o/r", 7).expect("rm twice"));
    assert!(
        !table
            .update("o/r", 7, |r| r.head = "x".into())
            .expect("update")
    );
    assert!(table.list().expect("list").is_empty());
}
