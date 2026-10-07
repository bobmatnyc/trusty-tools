//! #9011: with no instructional content installed, a real `tcode` logs one
//! ERROR line naming the remedy (D4) and writes no roster state (D5).
//!
//! Why: the agent loader and the roster deploy both hit the missing content,
//! and the loader runs again on every `agents.list`. Each used to log its own
//! ERROR, so one root cause produced a growing cascade on stderr (D4). The
//! roster deploy pinned `.trusty-code/agents/` and took its lock before it
//! loaded content, so a run with nothing installed left both behind (D5).
//! What: spawns the real binary with an empty `$HOME`, a cwd outside any
//! checkout and a fresh project. D4 sends `agents.list` twice and counts the
//! stderr lines that name `tm content install`; D5 runs both roster-deploy
//! paths (`serve` and `run-task`) and checks the project afterwards.
//! Test: this file IS the test.

mod support;

use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};

use trusty_agents_common::agents::manifest::MANIFEST_FILE;

/// `tcode` with `args`, an empty `$HOME` and its cwd in `project`, so no
/// content resolves. `tcode_command` sets the test-harness guard (#3036).
fn tcode_without_content(home: &Path, project: &Path, args: &[&str]) -> support::TcodeCommand {
    let mut cmd = support::tcode_command();
    cmd.args(args)
        .current_dir(project)
        .env("HOME", home)
        .env_remove("RUST_LOG");
    cmd
}

/// Runs `tcode serve --stdio` in `project`, sends `agents.list` once per id,
/// closes stdin and returns the output.
fn serve_without_content(home: &Path, project: &Path, ids: &[u32]) -> Output {
    let project_arg = project.display().to_string();
    // #9139: bound so its isolation tree outlives the spawned child.
    let mut cmd = tcode_without_content(
        home,
        project,
        &["serve", "--project", &project_arg, "--stdio"],
    );
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tcode serve --stdio");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for id in ids {
            writeln!(
                stdin,
                r#"{{"jsonrpc":"2.0","id":{id},"method":"agents.list","params":{{}}}}"#
            )
            .expect("write request");
        }
    }
    child.wait_with_output().expect("tcode exits on stdin EOF")
}

/// No content installed: one ERROR line names `tm content install`, however
/// many consumers and requests hit the missing content. The built-in agents
/// still answer both requests.
#[test]
fn no_content_logs_one_error_naming_tm_content_install() {
    let home = tempfile::tempdir().expect("scratch home");
    let project = tempfile::tempdir().expect("scratch project");
    let out = serve_without_content(home.path(), project.path(), &[1, 2]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Both requests were answered, so the loader ran twice.
    for id in [1, 2] {
        assert!(
            stdout.contains(&format!(r#""id":{id},"result""#)),
            "agents.list {id} must answer from the built-in agents\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
    let remedy: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("tm content install"))
        .collect();
    assert_eq!(
        remedy.len(),
        1,
        "exactly one line must name the remedy\nstderr: {stderr}"
    );
    assert!(remedy[0].contains("ERROR"), "{}", remedy[0]);
    // #9396: the remedy named first is `tm content update`.
    assert!(remedy[0].contains("tm content update"), "{}", remedy[0]);
    assert!(
        !stdout.contains("tm content install"),
        "the remedy is a log line, not protocol output\nstdout: {stdout}"
    );
}

/// #9011 D5 (owner ruling 35): no content installed writes no roster state.
/// Both paths that deploy the roster — `serve` at startup and the in-process
/// `run-task` — leave no `.trusty-code/agents/` directory and no ledger lock,
/// and `run-task` still runs its built-in `engineer` agent.
#[test]
fn no_content_writes_no_agents_dir_and_no_lock() {
    let home = tempfile::tempdir().expect("scratch home");
    let project = tempfile::tempdir().expect("scratch project");
    let agents = project.path().join(".trusty-code").join("agents");
    let lock = agents.join(format!("{MANIFEST_FILE}.lock"));

    let served = serve_without_content(home.path(), project.path(), &[1]);
    assert!(
        String::from_utf8_lossy(&served.stdout).contains(r#""id":1,"result""#),
        "serve must answer agents.list\nstderr: {}",
        String::from_utf8_lossy(&served.stderr)
    );
    assert!(!agents.exists(), "serve left {} behind", agents.display());
    assert!(!lock.exists(), "serve left {} behind", lock.display());

    let project_arg = project.path().display().to_string();
    let ran = tcode_without_content(
        home.path(),
        project.path(),
        &[
            "run-task",
            "engineer",
            "say hi",
            "--project",
            &project_arg,
            "--json",
        ],
    )
    .env("TCODE_MOCK_LLM", "echo")
    .output()
    .expect("spawn tcode run-task");
    assert!(
        ran.status.success(),
        "run-task with the built-in engineer must still run\nstderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(
        !agents.exists(),
        "run-task left {} behind",
        agents.display()
    );
    assert!(!lock.exists(), "run-task left {} behind", lock.display());
}
