//! #12029 P2b: plain runs of the shipped `trusty-architect-dashboard-jsonl`.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! this proves the shipped binary's one command, `fsck [ROOT]`, on a clean
//! store, a damaged one, a missing root and the default root (the working
//! directory), with the script's streams and exit codes. It writes nothing.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-dashboard-jsonl");

fn run(cwd: &std::path::Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn shipped_dashboard_jsonl_fsck() {
    let dir = tempfile::tempdir().unwrap();
    let clean = dir.path().join("clean");
    std::fs::create_dir(&clean).unwrap();
    std::fs::write(
        clean.join("items.jsonl"),
        "{\"data\":{\"t\":\"one\"},\"id\":\"A01\",\"sv\":1,\"version\":1}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("items.jsonl"),
        "{\"id\":\"A01\",\"data\":{},\"sv\":1,\"version\":1}\n",
    )
    .unwrap();
    let before = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(
        run(dir.path(), &["clean"]),
        (String::new(), String::new(), Some(0))
    );
    assert_eq!(
        run(dir.path(), &["nowhere"]),
        (String::new(), String::new(), Some(0))
    );
    let bad = "./items.jsonl: bytes are not canonical (re-render differs)\n";
    assert_eq!(run(dir.path(), &[]), (bad.into(), String::new(), Some(1)));
    assert_eq!(
        run(dir.path(), &[".", "clean"]),
        (bad.into(), String::new(), Some(1))
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
}
