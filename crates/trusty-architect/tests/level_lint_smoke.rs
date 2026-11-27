//! #12028 P1c: the shipped `trusty-architect-level-lint` binary runs.
//!
//! Checks the exit codes and output shape of the real binary, including the
//! stored-level path the replay cannot record (the script reads its store
//! from beside itself, outside the sandbox): with no `--level` the binary
//! reads `local/owner-queue` under its working directory.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-level-lint");

fn store_at(level: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let meta = root.path().join("local/owner-queue/meta");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(
        meta.join("settings.json"),
        format!("{{\"technical_level\": \"{level}\"}}\n"),
    )
    .unwrap();
    root
}

#[test]
fn shipped_level_lint_command_warns_and_exits_like_the_script() {
    let pm = store_at("pm");
    let out = Command::new(BIN)
        .args(["--strict", "run", "cargo", "on", "src/lib.rs"])
        .current_dir(pm.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(out.stderr.is_empty());
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines.iter().all(|l| l.starts_with("level_lint [pm]: ")));

    // The stored level is engineer: nothing is checked.
    let eng = store_at("engineer");
    let quiet = Command::new(BIN)
        .args(["--strict", "cargo"])
        .current_dir(eng.path())
        .output()
        .unwrap();
    assert_eq!(quiet.status.code(), Some(0));
    assert!(quiet.stdout.is_empty());

    let bad = Command::new(BIN).arg("--bogus").output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    let stderr = String::from_utf8(bad.stderr).unwrap();
    assert!(
        stderr.starts_with("usage: trusty-architect-level-lint "),
        "{stderr}"
    );
}

// See #12028: a stdin read error must not read as empty text, which would
// pass `--strict`.
#[test]
fn a_stdin_that_cannot_be_read_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .args(["--level", "pm", "--strict"])
        .stdin(std::fs::File::open(dir.path()).unwrap())
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(out.stdout.is_empty());
    assert!(stderr.starts_with("IsADirectoryError: [Errno "), "{stderr}");
}
