//! Pure half of the #11674 round trip: grammar, verdict, ledger line, block.

use super::*;

fn bound(task: Option<&str>) -> StopBinding {
    StopBinding {
        delegation_found: true,
        tool_use_id: Some("toolu_1".into()),
        ledger_task: task.map(str::to_owned),
    }
}

fn judge(binding: Option<&StopBinding>, msg: Option<&str>) -> RoundtripEvent {
    classify(
        binding,
        Some("aid_1"),
        msg,
        "2026-11-23T00:00:00Z".into(),
        "tm-x",
    )
}

#[test]
fn subagent_task_roundtrip_dispatch_grammar() {
    assert_eq!(
        dispatch_task("Task: rv-T0c@1123\nbody").as_deref(),
        Some("rv-T0c@1123")
    );
    assert_eq!(
        dispatch_task("  Task: ti-T03@1123 | x").as_deref(),
        Some("ti-T03@1123")
    );
    assert_eq!(dispatch_task("Task: ti-G01@1123"), None, "goal id");
    assert_eq!(dispatch_task("Task: #11674"), None, "issue number");
    assert_eq!(dispatch_task("Task: ti-T03"), None, "no stamp");
    assert_eq!(
        dispatch_task("see (Task: ti-T03@1123)"),
        None,
        "not a header line"
    );
    assert_eq!(
        dispatch_task("Task: a27bb9016b9bde7ed@1123"),
        None,
        "agent id"
    );
    assert_eq!(
        dispatch_task("Task: ti-G01@1123\nTask: ti-T05@1123").as_deref(),
        Some("ti-T05@1123")
    );
}

#[test]
fn subagent_task_roundtrip_pulse_grammar() {
    assert_eq!(
        pulse_task("x\nTask: rv-T0c@1123 | done: built it\n\n"),
        Pulse::Tagged("rv-T0c@1123".into())
    );
    assert_eq!(pulse_task("Task: rv-T0c@1123"), Pulse::Malformed, "no done");
    assert_eq!(
        pulse_task("Task: rv-T0c | done: x"),
        Pulse::Malformed,
        "no stamp"
    );
    assert_eq!(
        pulse_task("Task: rv-T0c@1123 | done: x\ntrailing prose"),
        Pulse::Absent,
        "not the last line"
    );
    assert_eq!(pulse_task(""), Pulse::Absent);
}

#[test]
fn subagent_task_roundtrip_cases() {
    let b = bound(Some("ti-T03@1123"));
    let ok = judge(Some(&b), Some("r\nTask: ti-T03@1123 | done: r"));
    assert_eq!(
        (ok.outcome, ok.returned.as_deref()),
        (Outcome::Ok, Some("ti-T03@1123"))
    );
    assert_eq!(ok.error, None);
    let missing = judge(Some(&b), Some("## Result\nall done"));
    assert_eq!(missing.outcome, Outcome::Missing);
    let mismatch = judge(Some(&b), Some("Task: ts-T07@1123 | done: r"));
    assert_eq!(
        (mismatch.outcome, mismatch.returned.as_deref()),
        (Outcome::Mismatch, Some("ts-T07@1123"))
    );
    let stamp = judge(Some(&b), Some("Task: ti-T03@1122 | done: r"));
    assert_eq!(
        stamp.outcome,
        Outcome::Mismatch,
        "a different stamp is another tag"
    );
    let unbound = judge(Some(&bound(None)), Some("Task: ti-T03@1123 | done: r"));
    assert_eq!(unbound.outcome, Outcome::Unbound);
    assert_eq!(unbound.bound, None);
}

#[test]
fn subagent_task_roundtrip_error_arms_never_record_ok() {
    // Fail-open check: every lookup/parse failure arm yields a non-ok verdict.
    let good = Some("Task: ti-T03@1123 | done: r");
    let no_daemon = judge(None, good);
    assert_eq!(no_daemon.outcome, Outcome::Unbound);
    assert!(no_daemon.error.is_some());
    let not_found = StopBinding {
        delegation_found: false,
        tool_use_id: None,
        ledger_task: Some("ti-T03@1123".into()),
    };
    let nf = judge(Some(&not_found), good);
    assert_eq!(
        nf.outcome,
        Outcome::Unbound,
        "a binding without a match is not trusted"
    );
    let b = bound(Some("ti-T03@1123"));
    let no_msg = judge(Some(&b), None);
    assert_eq!(no_msg.outcome, Outcome::Missing);
    assert!(no_msg.error.is_some());
    let bad = judge(Some(&b), Some("Task: ti-T03@1123 done r"));
    assert_eq!(bad.outcome, Outcome::Missing);
    assert!(
        bad.error
            .as_deref()
            .is_some_and(|e| e.contains("malformed"))
    );
}

#[test]
fn subagent_task_roundtrip_block_is_claimed_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = bound(Some("ti-T03@1123"));
    let missing = judge(Some(&b), Some("no tag"));
    assert!(wants_block(&missing, false));
    assert!(
        !wants_block(&missing, true),
        "stop_hook_active never blocks again"
    );
    assert!(
        !wants_block(&judge(Some(&bound(None)), Some("x")), false),
        "unbound never blocks"
    );
    assert!(claim_block_once(dir.path(), "toolu_1"));
    assert!(
        !claim_block_once(dir.path(), "toolu_1"),
        "second claim refused"
    );
    assert!(
        !claim_block_once(dir.path(), "../evil"),
        "unsafe key refused"
    );
    let mut blocked = missing.clone();
    blocked.blocked = true;
    let out = hook_output(&blocked);
    assert_eq!(out["decision"], "block");
    assert!(
        out["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Task: ti-T03@1123 | done:"))
    );
    assert!(hook_output(&missing).get("decision").is_none());
}

#[test]
fn subagent_task_roundtrip_ledger_line_is_a_datg_notice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log.jsonl");
    let ev = judge(Some(&bound(Some("ti-T03@1123"))), Some("none"));
    assert!(
        append_ledger_event(&log, &ev).is_err(),
        "no log file, no write"
    );
    std::fs::write(&log, "").expect("seed");
    append_ledger_event(&log, &ev).expect("append");
    let text = std::fs::read_to_string(&log).expect("read");
    let line: Value = serde_json::from_str(text.trim_end()).expect("json");
    assert_eq!(line["kind"], LEDGER_EVENT_KIND);
    assert_eq!(line["outcome"], "missing");
    assert_eq!(line["bound"], "ti-T03@1123");
    assert_eq!(line["tool_use_id"], "toolu_1");
    assert_eq!(line["agent_id"], "aid_1");
    assert!(line["returned"].is_null());
}
