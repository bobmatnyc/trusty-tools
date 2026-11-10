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
