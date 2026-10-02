//! #9011 D4: with no instructional content installed, a session start prints
//! exactly one line about it, and that line names `tm content install`.
//!
//! Why: the shadow quarantine logged a WARN and also returned the same failure
//! as a provisioning gap, which the caller printed, so one root cause produced
//! two lines; a later roster consumer in the same process added a third.
//! What: runs the real `tm sessions start` against a git repository with no
//! remote (the in-place path that provisions in this process) under an empty
//! `$HOME`, and counts the output lines that name the remedy.
//! Test: this file IS the test.

use std::process::Command;

use crate::common;

/// No content installed: one `error:` line names `tm content install`, and the
/// session still provisions with no agents rather than with a guessed roster.
#[test]
fn sessions_start_without_content_prints_one_line_naming_the_remedy() {
    let home = tempfile::Builder::new()
        .prefix("tm-test-no-content-")
        .tempdir_in("/tmp")
        .expect("scratch home");
    let project = tempfile::Builder::new()
        .prefix("tm-test-no-content-proj-")
        .tempdir_in("/tmp")
        .expect("scratch project");
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(project.path())
        .status()
        .expect("git init");
    assert!(init.success(), "git init failed");

    let out = common::tm_command_in(home.path())
        .args(["sessions", "start", "--dir"])
        .arg(project.path())
        .current_dir(project.path())
        .env_remove("RUST_LOG")
        .output()
        .expect("spawn tm sessions start");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The in-place provisioning ran, and deployed no agent.
    assert!(
        stdout.contains("Agents: 0 deployed"),
        "provisioning must run\nstdout: {stdout}\nstderr: {stderr}"
    );
    let remedy: Vec<&str> = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| l.contains("tm content install"))
        .collect();
    assert_eq!(
        remedy.len(),
        1,
        "exactly one line must name the remedy\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(remedy[0].starts_with("error:"), "{}", remedy[0]);
}
