//! #12029 P2g: plain runs of the shipped `trusty-architect-dashboard-db`.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! this proves the shipped binary: the working directory is the checkout
//! (its `local/owner-queue` is the default store), `tally` and `footer` read
//! it, a write lands in it, and a bad command line is argparse's exit 2.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-dashboard-db");

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
fn shipped_dashboard_db_tally_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let items = dir.path().join("local/owner-queue/items");
    std::fs::create_dir_all(&items).unwrap();
    std::fs::write(
        items.join("D01.json"),
        r#"{"kind": "decision", "status": "open", "version": 1}"#,
    )
    .unwrap();
    std::fs::write(
        items.join("B01.json"),
        r#"{"kind": "blocker", "status": "open", "version": 1}"#,
    )
    .unwrap();
    assert_eq!(
        run(dir.path(), &["tally"]),
        ("1 decisions, 1 blockers\n".into(), String::new(), Some(0))
    );
    assert_eq!(
        run(dir.path(), &["footer"]),
        (
            "Dashboard: [local](http://127.0.0.1:7890/) \u{2014} 1 decisions, 1 blockers\n".into(),
            String::new(),
            Some(0)
        )
    );
    let (out, err, code) = run(dir.path(), &["log", "items", "D01", "noted"]);
    assert_eq!((err.as_str(), code), ("", Some(0)), "{out}");
    assert!(out.contains("\"version\": 2"), "{out}");
    let (out, err, code) = run(dir.path(), &[]);
    assert_eq!((out.as_str(), code), ("", Some(2)));
    assert!(
        err.ends_with("dashboard-db.py: error: the following arguments are required: cmd\n"),
        "{err}"
    );
    let (_, err, code) = run(dir.path(), &["get", "items", "D0z"]);
    assert_eq!(
        (err.as_str(), code),
        ("dashboard-db: not_found: items/D0z not found\n", Some(1))
    );
}
