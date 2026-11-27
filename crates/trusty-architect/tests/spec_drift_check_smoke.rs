//! #12028 P1b: the shipped `trusty-architect-spec-drift-check` binary runs.
//!
//! Lays the PASS-path recording's input files into a temp root, runs the real
//! binary on the live clock, and checks the exit code and the shape of what
//! it prints and logs; no assertion depends on the time.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-spec-drift-check");

#[test]
fn shipped_spec_drift_command_prints_ten_results_and_logs_one_line() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay/spec_drift_check/daily-pass.json");
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    let root = tempfile::tempdir().unwrap();
    for (rel, content) in rec["files"].as_object().unwrap() {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content.as_str().unwrap()).unwrap();
    }
    let soak = root.path().join("local/soak/spec-drift.jsonl");
    let before = std::fs::read_to_string(&soak).unwrap().lines().count();

    // The default root is the current directory.
    let out = Command::new(BIN).current_dir(root.path()).output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(out.stderr.is_empty());
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 10, "{stdout}");
    for (n, line) in lines.iter().enumerate() {
        let mut parts = line.splitn(3, ' ');
        assert_eq!(parts.next(), Some(format!("INV-{:02}", n + 1).as_str()));
        let status = parts.next().unwrap();
        assert!(["PASS", "WARN", "PENDING"].contains(&status), "{line}");
    }
    let log = std::fs::read_to_string(&soak).unwrap();
    assert_eq!(log.lines().count(), before + 1);
    let last: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(last["exit"], 0);
    assert_eq!(last["results"].as_object().unwrap().len(), 10);
    assert!(last["ts"].as_str().unwrap().ends_with('Z'));

    let bad = Command::new(BIN).arg("--bogus").output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    let stderr = String::from_utf8(bad.stderr).unwrap();
    assert!(
        stderr.starts_with("usage: trusty-architect-spec-drift-check "),
        "{stderr}"
    );
}
