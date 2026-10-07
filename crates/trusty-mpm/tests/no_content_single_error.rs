//! #9011 D4: with no instructional content installed, a session start prints
//! exactly one line about it, and that line names `tm content install`.
//!
//! Why: the shadow quarantine logged a WARN and also returned the same failure
//! as a provisioning gap, which the caller printed, so one root cause produced
//! two lines; a later roster consumer in the same process added a third.
//! Code-critic r1: no content also means zero agents, even when the framework
//! source still holds a previous binary's roster. #9012: the PM instructions
//! are content too, so with none the start is refused before anything is
//! provisioned — still one line, still naming the remedy.
//! What: runs the real `tm sessions start` against a git repository with no
//! remote (the in-place path that provisions in this process) under an empty
//! `$HOME`, optionally seeded with a stale agent source, and checks the exit
//! status, that no agent reached a deploy tier, and the output lines that name
//! the remedy.
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

/// Runs `tm sessions start` in `project` under `home`; returns success,
/// stdout, stderr.
fn sessions_start(home: &Path, project: &Path) -> (bool, String, String) {
    let out = common::tm_command_in(home)
        .args(["sessions", "start", "--dir"])
        .arg(project)
        .current_dir(project)
        .env_remove("RUST_LOG")
        .output()
        .expect("spawn tm sessions start");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every `name` file under `root`, recursively.
fn files_named(root: &Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_named(&path, name));
        } else if path.file_name().is_some_and(|n| n == name) {
            out.push(path);
        }
    }
    out
}

/// No content installed: the start is refused (#9012: no PM instructions to
/// compose), and exactly one error line names `tm content install`.
#[test]
fn sessions_start_without_content_prints_one_line_naming_the_remedy() {
    let (home, project) = scratch_home_and_project();
    let (ok, stdout, stderr) = sessions_start(home.path(), project.path());

    // #9012: refused before provisioning — no agent deploy summary.
    assert!(
        !ok,
        "a start with no content must be refused\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("Agents:"),
        "nothing may provision without content\nstdout: {stdout}\nstderr: {stderr}"
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
    assert!(
        remedy[0].to_ascii_lowercase().starts_with("error:"),
        "{}",
        remedy[0]
    );
    // #9396: the remedy named first is `tm content update`.
    assert!(remedy[0].contains("tm content update"), "{}", remedy[0]);
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

    let (ok, stdout, stderr) = sessions_start(home.path(), project.path());
    assert!(!ok, "stdout: {stdout}\nstderr: {stderr}");
    // The stale agent never left its source directory for a deploy tier.
    let mut copies = files_named(home.path(), "stale-agent.md");
    copies.extend(files_named(project.path(), "stale-agent.md"));
    assert_eq!(
        copies,
        vec![stale.join("stale-agent.md")],
        "a stale source must not deploy\nstdout: {stdout}\nstderr: {stderr}"
    );
    let remedy = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| l.contains("tm content install"))
        .count();
    assert_eq!(remedy, 1, "stdout: {stdout}\nstderr: {stderr}");
}
