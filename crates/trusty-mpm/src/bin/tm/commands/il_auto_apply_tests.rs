//! G0b: each hook entry point applies an IL block to its session's store;
//! a refused block writes nothing and lands in the denial store.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use trusty_architect::FakeClock;
use trusty_architect::ledger::writer::Writer;

use super::{Entry, Outcome, Target, run, turn_text};

const BLOCK: &str = "```il\ntask: xa-T01\ndone: d\npct: 5\nnext: ship it\nact: ran the gates\n```";

fn no_secrets(_: &str) -> Option<String> {
    None
}

/// A temp repo holding `pm-small` as `.trusty-mpm/datg/<session>`.
fn repo(session: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join(".git")).expect(".git");
    let dir = tmp.path().join(".trusty-mpm/datg").join(session);
    fs::create_dir_all(&dir).expect("store dir");
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../trusty-architect/tests/fixtures/ledger/pm-small");
    for e in fs::read_dir(src).expect("fixture") {
        let e = e.expect("entry");
        fs::copy(e.path(), dir.join(e.file_name())).expect("copy");
    }
    (tmp, dir)
}

fn target(root: &Path, session: &str) -> Target {
    Target {
        cwd: root.to_path_buf(),
        session: Some(session.to_owned()),
        denials: Some(root.join("denials.jsonl")),
    }
}

fn go(event: &str, payload: &serde_json::Value, t: &Target) -> Outcome {
    let clock = FakeClock::new(1_792_576_800.0);
    let scan = no_secrets;
    run(event, payload, "sid-1", t, &Writer::new(&clock, &scan))
}

fn next_of(dir: &Path) -> String {
    let st: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("state.json")).expect("state"))
            .expect("json");
    st["session"]["next"].as_str().unwrap_or("").to_owned()
}

#[test]
fn each_entry_point_applies_the_block_to_its_own_session() {
    let cases = [
        (
            "PostToolUse",
            json!({"tool_name": "Agent", "tool_response": {"content": [{"type": "text", "text": BLOCK}]}}),
            "pm-a",
            Entry::AgentHandback,
        ),
        (
            "UserPromptSubmit",
            json!({"prompt": format!("[from pm-b]\n{BLOCK}")}),
            "pm-b",
            Entry::InboundMessage,
        ),
        (
            "UserPromptSubmit",
            json!({"prompt": BLOCK}),
            "tm-architect",
            Entry::ArchitectPrompt,
        ),
    ];
    for (event, payload, session, entry) in cases {
        let (tmp, dir) = repo(session);
        let t = target(tmp.path(), session);
        assert_eq!(
            turn_text(event, &payload, Some(session)).map(|(e, _)| e),
            Some(entry)
        );
        match go(event, &payload, &t) {
            Outcome::Applied(a) => assert!(!a.replayed && a.acts == 1, "{session}: {a:?}"),
            other => panic!("{session}: {other:?}"),
        }
        assert_eq!(next_of(&dir), "ship it", "{session}");
        let log = fs::read_to_string(dir.join("log.jsonl")).expect("log");
        assert!(log.contains(&format!("\"kind\":\"act\",\"session\":\"{session}\"")));
        match go(event, &payload, &t) {
            Outcome::Applied(a) => assert!(a.replayed, "{session}: replay"),
            other => panic!("{session} replay: {other:?}"),
        }
    }
}

#[test]
fn a_rejected_block_writes_nothing_and_is_recorded_for_list_recent_errors() {
    let (tmp, dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    let before = fs::read(dir.join("state.json")).expect("state");
    let bad = format!(
        "```il\ntask: xa-T01\ndone: d\npct: 5\nnext: n\nact: {}\n```",
        "y".repeat(250)
    );
    let got = go("UserPromptSubmit", &json!({"prompt": bad}), &t);
    assert!(matches!(&got, Outcome::Failed(c) if c == "LONG"), "{got:?}");
    assert_eq!(fs::read(dir.join("state.json")).expect("state"), before);
    let rec = fs::read_to_string(tmp.path().join("denials.jsonl")).expect("a denial record");
    assert!(rec.contains("il-auto-apply:LONG"), "{rec}");
    assert!(
        rec.contains("il auto-apply refused il:inbound-message [LONG]"),
        "{rec}"
    );
}

#[test]
fn a_failed_write_is_never_reported_as_applied() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    let _ = fs::remove_file(dir.join(".lock"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");
    let got = go("UserPromptSubmit", &json!({"prompt": BLOCK}), &t);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        matches!(&got, Outcome::Failed(c) if c == "WRITE"),
        "{got:?}"
    );
    let rec = fs::read_to_string(tmp.path().join("denials.jsonl")).expect("a denial record");
    assert!(rec.contains("il-auto-apply:WRITE"), "{rec}");
}

#[test]
fn no_block_and_no_ledger_do_nothing() {
    let (tmp, _dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    assert!(matches!(
        go("UserPromptSubmit", &json!({"prompt": "hi"}), &t),
        Outcome::NoBlock
    ));
    let read = json!({"tool_name": "Read", "tool_response": BLOCK});
    assert!(matches!(go("PostToolUse", &read, &t), Outcome::NoBlock));
    let empty = tempfile::tempdir().expect("tempdir");
    let t2 = target(empty.path(), "pm-a");
    assert!(matches!(
        go("UserPromptSubmit", &json!({"prompt": BLOCK}), &t2),
        Outcome::NoLedger
    ));
    assert!(!empty.path().join("denials.jsonl").exists());
}
