//! #9011: with no instructional content installed, a real `tcode` logs one
//! ERROR line naming the remedy (D4) and writes no roster state (D5).
//! #9013: tcode's own agents are content too, so with none installed no
//! built-in agent runs; `run-task` fails naming the remedy.
//!
//! Why: the agent loader and the roster deploy both hit the missing content,
//! and the loader runs again on every `agents.list`. Each used to log its own
//! ERROR, so one root cause produced a growing cascade on stderr (D4). The
//! roster deploy pinned `.trusty-code/agents/` and took its lock before it
//! loaded content, so a run with nothing installed left both behind (D5).
//! What: spawns the real binary with an empty `$HOME`, a cwd outside any
//! checkout and a fresh project. D4 sends `agents.list` twice and counts the
//! stderr lines that name `tm content install`; D5 runs both roster-deploy
//! paths (`serve` and `run-task`) and checks the project afterwards; #9013
//! runs `run-task engineer` and checks it refuses.
//! Test: this file IS the test.

use crate::support;

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

/// Runs `tcode serve --stdio` in `project`, writes each of `lines` as one
/// request, closes stdin and returns the output.
fn serve_lines_without_content(home: &Path, project: &Path, lines: &[String]) -> Output {
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
        for line in lines {
            writeln!(stdin, "{line}").expect("write request");
        }
    }
    child.wait_with_output().expect("tcode exits on stdin EOF")
}

/// Runs `tcode serve --stdio` in `project`, sends `agents.list` once per id,
/// closes stdin and returns the output.
fn serve_without_content(home: &Path, project: &Path, ids: &[u32]) -> Output {
    let lines: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"agents.list","params":{{}}}}"#))
        .collect();
    serve_lines_without_content(home, project, &lines)
}

/// No content installed: one ERROR line names `tm content install`, however
/// many consumers and requests hit the missing content. Both requests are
/// still answered (#9013: with an empty roster, since every built-in agent is
/// content).
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
            "agents.list {id} must be answered\nstdout: {stdout}\nstderr: {stderr}"
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
/// `run-task` — leave no `.trusty-code/agents/` directory and no ledger lock.
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
    let _ran = tcode_without_content(
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
        !agents.exists(),
        "run-task left {} behind",
        agents.display()
    );
    assert!(!lock.exists(), "run-task left {} behind", lock.display());
}

/// #9013 (ADR-0064, runtime-only content): with no content installed,
/// `run-task engineer` refuses — `engineer` is no longer compiled in — and the
/// output names the remedy, `tm content update` then the offline
/// `tm content install --from`.
///
/// Why: before #9013 tcode's own 8 agents were `include_str!`-embedded, so this
/// run succeeded on text no content release had shipped. Failing closed is
/// what makes the content release the only source.
/// What: spawns `run-task engineer` with an empty `$HOME` and a cwd outside any
/// checkout; asserts a non-zero exit and that the output names `engineer` and
/// both remedies.
/// Test: this test.
#[test]
fn no_content_run_task_fails_closed_naming_the_remedy() {
    let home = tempfile::tempdir().expect("scratch home");
    let project = tempfile::tempdir().expect("scratch project");
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
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(
        !ran.status.success(),
        "run-task must refuse with no content installed\n{output}"
    );
    for needle in ["engineer", "tm content update", "tm content install --from"] {
        assert!(
            output.contains(needle),
            "output must name `{needle}`\n{output}"
        );
    }
}

/// #9013 (review fix): the bundled-name guard on `skills.create` and
/// `skills.delete` refuses with 403 (`-32001`) even when no content is
/// installed, because it reads the static name table, not the content.
///
/// Why: the guard used the content-backed skill catalog, which is empty with
/// no content, so `create` wrote a skill shadowing a bundled name and `delete`
/// answered 404 instead of 403.
/// What: serves with an empty `$HOME`, sends create then delete for the first
/// `DEFAULT_SKILL_NAMES` entry; asserts both answer `-32001` and no skill
/// directory was written.
/// Test: this test.
#[test]
fn no_content_bundled_skill_name_guard_still_answers_403() {
    let home = tempfile::tempdir().expect("scratch home");
    let project = tempfile::tempdir().expect("scratch project");
    let name = trusty_code::assets::DEFAULT_SKILL_NAMES[0];
    let lines = vec![
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"skills.create","params":{{"name":"{name}","content":"x"}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"skills.delete","params":{{"name":"{name}"}}}}"#
        ),
    ];
    let out = serve_lines_without_content(home.path(), project.path(), &lines);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for id in [1, 2] {
        let reply = stdout
            .lines()
            .find(|l| l.contains(&format!(r#""id":{id},"#)))
            .unwrap_or_else(|| panic!("no reply for {id}\nstdout: {stdout}"));
        assert!(
            reply.contains(r#""code":-32001"#),
            "request {id} must be refused with 403\n{reply}"
        );
    }
    assert!(
        !project
            .path()
            .join(".trusty-code")
            .join("skills")
            .join(name)
            .exists(),
        "skills.create wrote a bundled-name skill with no content installed"
    );
}
