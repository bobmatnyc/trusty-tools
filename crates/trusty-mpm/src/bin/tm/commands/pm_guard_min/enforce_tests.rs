//! Rule 1 enforces in shadow mode; rules 2, 3 and 5 only record (#10690,
//! owner ruling D0u).

use std::path::Path;

use serde_json::{Value, json};

use super::*;

fn env_as(architect: bool) -> impl Fn(&Value, &Path) -> GuardEnv {
    move |_, home| GuardEnv {
        home: home.to_path_buf(),
        project_dir: None,
        cwd: home.to_path_buf(),
        r5_allow: Vec::new(),
        architect,
    }
}

fn payload(tool: &str, input: Value) -> Vec<u8> {
    json!({ "session_id": "s-10690", "tool_name": tool, "tool_input": input })
        .to_string()
        .into_bytes()
}

#[test]
fn an_r1_write_is_refused_and_recorded() {
    let home = tempfile::tempdir().unwrap();
    let anchor = home.path().join(".claude/settings.json");
    let calls = [
        payload("Write", json!({ "file_path": anchor, "content": "{}" })),
        payload("Bash", json!({ "command": "cp ./tm ~/.cargo/bin/tm" })),
        // A body over the Bash parse bound is still judged on its path.
        payload(
            "Write",
            json!({ "file_path": anchor, "content": "a".repeat(2 << 20) }),
        ),
    ];
    for architect in [false, true] {
        for raw in &calls {
            let Outcome::Refuse(line) = decide(Some(raw), Some(home.path()), &env_as(architect))
            else {
                panic!("rule 1 must refuse (architect = {architect})");
            };
            assert_eq!(line["rule"], "R1");
            assert_eq!(line["enforced"], true);
            assert_eq!(line["session_id"], "s-10690");
            assert!(line["fact"]["target"].is_string());
        }
    }
    let Outcome::Refuse(line) = decide(Some(&calls[0]), Some(home.path()), &env_as(false)) else {
        unreachable!()
    };
    append_log(home.path(), &line);
    let log = std::fs::read_to_string(home.path().join(LOG_PATH)).unwrap();
    let recorded: Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(recorded["rule"], "R1");
    assert_eq!(recorded["enforced"], true);
    let reason = refusal_reason(&recorded);
    assert!(reason.contains("settings.json"), "{reason}");
}

#[test]
fn observe_only_rules_record_and_allow() {
    let home = tempfile::tempdir().unwrap();
    let raw = payload("Bash", json!({ "command": "rm -rf ~" }));
    match decide(Some(&raw), Some(home.path()), &env_as(false)) {
        Outcome::Record(line) => {
            assert_eq!(line["rule"], "R2");
            assert_eq!(line["enforced"], false);
        }
        other => panic!("rule 2 records only: {other:?}"),
    }
    assert_eq!(
        decide(Some(&raw), Some(home.path()), &env_as(true)),
        Outcome::Allow,
        "the Architect runs rule 1 only"
    );
    let big = payload(
        "Write",
        json!({ "file_path": home.path().join("notes.md"), "content": "a".repeat(2 << 20) }),
    );
    assert_eq!(
        decide(Some(&big), Some(home.path()), &env_as(false)),
        Outcome::Allow
    );
}

#[test]
fn no_stdin_or_no_home_refuses() {
    let home = tempfile::tempdir().unwrap();
    let raw = payload("Write", json!({ "file_path": "/tmp/x", "content": "" }));
    for (outcome, kind) in [
        (
            decide(None, Some(home.path()), &env_as(false)),
            "stdin_unreadable",
        ),
        (decide(Some(&raw), None, &env_as(false)), "no_home_dir"),
    ] {
        let Outcome::Refuse(line) = outcome else {
            panic!("{kind} must refuse");
        };
        assert_eq!(line["fact"]["error"], kind);
        assert!(refusal_reason(&line).contains(kind));
    }
}
