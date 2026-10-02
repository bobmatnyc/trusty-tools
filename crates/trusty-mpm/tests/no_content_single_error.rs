//! #9011 D4: with no instructional content installed, a session start prints
//! exactly one line about it, and that line names `tm content install`.
//!
//! Why: the shadow quarantine logged a WARN and also returned the same failure
//! as a provisioning gap, which the caller printed, so one root cause produced
//! two lines; a later roster consumer in the same process added a third.
//! Code-critic r1: no content also means zero agents, even when the framework
//! source still holds a previous binary's roster.
//! What: runs the real `tm sessions start` against a git repository with no
//! remote (the in-place path that provisions in this process) under an empty
//! `$HOME`, optionally seeded with a stale agent source, and checks the deploy
//! count and the output lines that name the remedy.
//! Test: this file IS the test.

use std::path::Path;
use std::process::Command;

use crate::common;

/// A scratch `$HOME` and a git repository with no remote, both under `/tmp`.
fn scratch_home_and_project() -> (tempfile::TempDir, tempfile::TempDir) {
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
    (home, project)
}

/// Runs `tm sessions start` in `project` under `home`; returns stdout, stderr.
fn sessions_start(home: &Path, project: &Path) -> (String, String) {
    let out = common::tm_command_in(home)
        .args(["sessions", "start", "--dir"])
        .arg(project)
        .current_dir(project)
        .env_remove("RUST_LOG")
        .output()
        .expect("spawn tm sessions start");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// No content installed: one `error:` line names `tm content install`, and the
/// session still provisions with no agents rather than with a guessed roster.
#[test]
fn sessions_start_without_content_prints_one_line_naming_the_remedy() {
    let (home, project) = scratch_home_and_project();
    let (stdout, stderr) = sessions_start(home.path(), project.path());

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

/// #9011 code-critic r1: no content means zero agents, even when the framework
/// source still holds a previous binary's roster (an upgrade with no `tm
/// content install`). Nothing deploys from it, and the remedy line stays the
/// only one.
#[test]
fn sessions_start_without_content_deploys_no_stale_agent() {
    let (home, project) = scratch_home_and_project();
    let stale = home.path().join(".trusty-mpm/framework/agents");
    std::fs::create_dir_all(&stale).expect("stale source dir");
    std::fs::write(
        stale.join("stale-agent.md"),
        "---\nname: stale-agent\ndescription: left by an earlier binary\n---\n\nStale.\n",
    )
    .expect("stale agent");

    let (stdout, stderr) = sessions_start(home.path(), project.path());
    assert!(
        stdout.contains("Agents: 0 deployed"),
        "a stale source must not deploy\nstdout: {stdout}\nstderr: {stderr}"
    );
    let remedy = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| l.contains("tm content install"))
        .count();
    assert_eq!(remedy, 1, "stdout: {stdout}\nstderr: {stderr}");
}
