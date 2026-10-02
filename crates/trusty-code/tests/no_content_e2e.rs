//! #9011 D4: with no instructional content installed, a real `tcode serve`
//! logs exactly one ERROR line about it, and that line names the remedy.
//!
//! Why: the agent loader and the roster deploy both hit the missing content,
//! and the loader runs again on every `agents.list`. Each used to log its own
//! ERROR, so one root cause produced a growing cascade on stderr.
//! What: spawns the real binary with an empty `$HOME`, a cwd outside any
//! checkout and a fresh project, sends `agents.list` twice, closes stdin, and
//! counts the stderr lines that name `tm content install`.
//! Test: this file IS the test.

mod support;

use std::io::Write;
use std::process::Stdio;

/// No content installed: one ERROR line names `tm content install`, however
/// many consumers and requests hit the missing content. The built-in agents
/// still answer both requests.
#[test]
fn no_content_logs_one_error_naming_tm_content_install() {
    let home = tempfile::tempdir().expect("scratch home");
    let project = tempfile::tempdir().expect("scratch project");
    // `tcode_command` sets the test-harness guard (#3036).
    let mut child = support::tcode_command()
        .args(["serve", "--project"])
        .arg(project.path())
        .arg("--stdio")
        .current_dir(project.path())
        .env("HOME", home.path())
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tcode serve --stdio");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for id in [1, 2] {
            writeln!(
                stdin,
                r#"{{"jsonrpc":"2.0","id":{id},"method":"agents.list","params":{{}}}}"#
            )
            .expect("write request");
        }
    }
    let out = child.wait_with_output().expect("tcode exits on stdin EOF");
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
    assert!(
        !stdout.contains("tm content install"),
        "the remedy is a log line, not protocol output\nstdout: {stdout}"
    );
}
