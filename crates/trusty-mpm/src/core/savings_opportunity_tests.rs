//! Tests for the bounded opportunity ledger (#10973).

use super::*;
use crate::core::savings::{append_row, fold_session_per_call, savings_log_in};

fn row_on(day: &str) -> OpportunityRow {
    let mut row = OpportunityRow::new("PostToolUse", "s1", "read", "Read", 40_000, 40_000, 9_000);
    row.ts = format!("{day}T12:00:00Z");
    row
}

#[test]
fn tool_type_of_names_each_category() {
    assert_eq!(tool_type_of("mcp__trusty-search__search"), "mcp");
    assert_eq!(tool_type_of("Agent"), "subagent");
    assert_eq!(tool_type_of("Task"), "subagent");
    assert_eq!(tool_type_of("SubagentStop"), "subagent");
    assert_eq!(tool_type_of("Bash"), "bash");
    assert_eq!(tool_type_of("WebFetch"), "webfetch");
    assert_eq!(tool_type_of("Read"), "read");
}

/// #10981 review: the row goes to its day file and nowhere near `savings.jsonl`.
#[test]
fn an_opportunity_row_lands_in_its_day_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let row = row_on("2026-11-10");
    assert_eq!(
        append_opportunity(dir.path(), &row).expect("append"),
        OpportunityAppend::Written
    );
    let day = opportunity_dir_in(dir.path()).join("2026-11-10.jsonl");
    let value: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&day).expect("read").trim()).expect("json");
    assert_eq!(value["would_compress_bytes"], 9_000);
    assert_eq!(value["tool_type"], "read");
    assert!(value.get("basis").is_none(), "no repeated basis text");
    assert!(!savings_log_in(dir.path()).exists());
}

/// #10981 review: the ledger cannot grow without bound — a full day drops rows,
/// and only the newest `keep_days` day files survive.
#[test]
fn the_opportunity_ledger_is_bounded_by_day_cap_and_retention() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = dir.path().join("opportunity");
    let unrelated = ledger.join("notes.txt");
    std::fs::create_dir_all(&ledger).expect("mkdir");
    std::fs::write(&unrelated, "keep me").expect("write");

    let cap = 1_000;
    let mut written = 0;
    for _ in 0..50 {
        if append_opportunity_bounded(&ledger, &row_on("2026-11-01"), cap, 3).expect("append")
            == OpportunityAppend::Written
        {
            written += 1;
        }
    }
    let day = std::fs::metadata(ledger.join("2026-11-01.jsonl"))
        .expect("stat")
        .len();
    assert!(written < 50, "the cap must drop rows");
    let one_row = day / written;
    assert!(
        day < cap + one_row,
        "a day file stops within one row of its cap: {day}"
    );

    for d in ["2026-11-02", "2026-11-03", "2026-11-04", "2026-11-05"] {
        append_opportunity_bounded(&ledger, &row_on(d), cap, 3).expect("append");
    }
    let mut names: Vec<String> = std::fs::read_dir(&ledger)
        .expect("ls")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "2026-11-03.jsonl",
            "2026-11-04.jsonl",
            "2026-11-05.jsonl",
            "notes.txt"
        ]
    );
}

#[test]
fn a_tagged_compress_row_still_folds_as_a_saving() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = savings_log_in(dir.path());
    let row = SavingsRow {
        ts: "2026-11-10T00:00:00Z".to_string(),
        session_id: "s1".to_string(),
        technique: crate::core::savings::TECHNIQUE_COMPRESS.to_string(),
        tokens_saved: 900,
        tokens_before: 1_000,
        cost_saved_usd: 0.01,
        basis: "b".to_string(),
        model_source: String::new(),
    };
    let tagged = TaggedSavingsRow {
        row: &row,
        tool_type: TOOL_TYPE_BASH,
        tool_name: "git diff",
    };
    append_row(&ledger, &tagged).expect("append");
    assert_eq!(fold_session_per_call(&ledger, "s1").tokens_saved, 900);
}

/// #10981 round 2: a prune is housekeeping. An old day it cannot delete must
/// not cost the new day its row.
#[test]
fn a_prune_that_cannot_delete_an_old_day_still_appends() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = dir.path().join("opportunity");
    // A non-empty directory under a day file's name: `remove_file` fails on it.
    std::fs::create_dir_all(ledger.join("2026-10-01.jsonl/x")).expect("blocker");
    let out = append_opportunity_bounded(&ledger, &row_on("2026-11-01"), 1_000, 1);
    assert_eq!(
        out.expect("the row is appended"),
        OpportunityAppend::Written
    );
    assert!(ledger.join("2026-11-01.jsonl").is_file());
}

/// #10981 round 2: another hook process deleted the old day first.
#[test]
fn a_prune_that_races_a_vanished_file_still_appends() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = dir.path().join("opportunity");
    std::fs::create_dir_all(&ledger).expect("mkdir");
    std::fs::write(ledger.join("2026-10-01.jsonl"), "{}\n").expect("old day");
    let vanished = |_: &Path| Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    let out = append_opportunity_bounded_with(&ledger, &row_on("2026-11-01"), 1_000, 1, &vanished);
    assert_eq!(
        out.expect("the row is appended"),
        OpportunityAppend::Written
    );
}

/// #10981 round 2: a full day is not silent. The first dropped row writes one
/// `day_full` sentinel; every dropped row is counted in `<day>.dropped`.
#[test]
fn hitting_the_day_cap_writes_one_sentinel_and_counts_the_rest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = dir.path().join("opportunity");
    let mut written = 0u64;
    for _ in 0..40 {
        if append_opportunity_bounded(&ledger, &row_on("2026-11-01"), 1_000, 3).expect("append")
            == OpportunityAppend::Written
        {
            written += 1;
        }
    }
    let day = std::fs::read_to_string(ledger.join("2026-11-01.jsonl")).expect("day file");
    let sentinels = day.lines().filter(|l| l.contains("\"day_full\"")).count();
    assert_eq!(sentinels, 1, "{day}");
    let dropped = std::fs::metadata(ledger.join("2026-11-01.dropped"))
        .expect("a drop counter")
        .len();
    assert_eq!(dropped, 40 - written);
}

/// #10981 round 2: a drop counter leaves with its day file.
#[test]
fn drop_counters_are_pruned_with_their_day_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = dir.path().join("opportunity");
    std::fs::create_dir_all(&ledger).expect("mkdir");
    std::fs::write(ledger.join("2026-10-01.jsonl"), "{}\n").expect("old day");
    std::fs::write(ledger.join("2026-10-01.dropped"), "...").expect("old counter");
    append_opportunity_bounded(&ledger, &row_on("2026-11-01"), 1_000, 1).expect("append");
    assert!(!ledger.join("2026-10-01.dropped").exists());
    assert!(!ledger.join("2026-10-01.jsonl").exists());
}
