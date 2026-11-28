//! #12029 P2h: plain runs of the shipped `trusty-architect-dashboard-updates`.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! this proves the shipped binary: the working directory is the checkout
//! (its `local/owner-queue` is the default store and `inbox` the default
//! inbox), `note` appends a line and ingests it, and bad usage exits 2.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-dashboard-updates");

fn run(cwd: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", cwd)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn shipped_dashboard_updates_note_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let items = dir.path().join("local/owner-queue/items");
    std::fs::create_dir_all(&items).unwrap();
    std::fs::write(
        items.join("B01.json"),
        r#"{"kind": "task", "task": "Tqa", "status": "open", "version": 1}"#,
    )
    .unwrap();
    let (out, err, code) = run(dir.path(), &["note", "Tqa", "sample note", "--by", "pm"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "{\"added\": {\"B01\": 1}, \"conflicts\": [], \"errors\": []}\n",
            "",
            Some(0)
        )
    );
    let notes = std::fs::read_to_string(dir.path().join("inbox/task-notes.jsonl")).unwrap();
    assert!(
        notes.contains("\"task\": \"Tqa\", \"by\": \"pm\", \"text\": \"sample note\""),
        "{notes}"
    );
    let stored = std::fs::read_to_string(items.join("B01.json")).unwrap();
    assert!(
        stored.contains("\"version\": 2") && stored.contains("sample note"),
        "{stored}"
    );
    // Idempotent: a second pass adds nothing.
    assert_eq!(
        run(dir.path(), &[]).0,
        "{\"added\": {}, \"conflicts\": [], \"errors\": []}\n"
    );
    let (out, err, code) = run(dir.path(), &["note", "Tqa"]);
    assert_eq!((out.as_str(), code), ("", Some(2)));
    assert!(err.starts_with("usage: dashboard_updates.py note"), "{err}");
}
