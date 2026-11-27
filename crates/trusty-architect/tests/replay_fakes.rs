//! #12028: fake executables in the replay sandbox, driven through the CLI.
//!
//! Each test records a `/bin/sh -c` script that shells out to a fake `gh`
//! declared with `record --fakes FILE`, then inspects the fixture JSON or
//! replays it. The tests read the fixture as `serde_json::Value`, so they
//! name no library type the change adds.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-replay");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Writes `fakes` to `dir/fakes.json` and returns the path.
fn fakes_file(dir: &Path, fakes: &Value) -> String {
    let p = dir.join("fakes.json");
    std::fs::write(&p, fakes.to_string()).unwrap();
    p.to_string_lossy().into_owned()
}

/// `record --fakes F --out O -- /bin/sh -c SCRIPT`.
fn record(fakes: &str, out: &str, script: &str) -> Output {
    run(&[
        "record", "--fakes", fakes, "--out", out, "--", "/bin/sh", "-c", script,
    ])
}

fn fixture(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

const GH_LIST: &str = "gh pr list --json number; echo \"rc=$?\"";

#[test]
fn a_script_calling_a_fake_gh_records_and_replays_clean() {
    let dir = tempfile::tempdir().unwrap();
    let fakes = json!({"gh": [
        {"argv": ["pr", "view"], "stdout": "wrong\n", "stderr": "", "exit": 4},
        {"argv": ["pr", "list", "--json", "number"], "stdout": "[{\"number\":7}]\n",
         "stderr": "", "exit": 0},
    ]});
    let f = fakes_file(dir.path(), &fakes);
    let out = dir.path().join("gh.json");
    let o = out.to_str().unwrap();
    let rec = record(&f, o, GH_LIST);
    assert_eq!(rec.status.code(), Some(0), "{}", text(&rec.stderr));
    let fx = fixture(&out);
    assert_eq!(fx["version"], 2, "a recording with fakes is version 2");
    assert_eq!(fx["stdout"], "[{\"number\":7}]\nrc=0\n");
    assert_eq!(fx["fakes"]["stubs"], fakes);
    assert_eq!(
        fx["fakes"]["calls"],
        json!([["gh", "pr", "list", "--json", "number"]])
    );
    let rep = run(&["replay", o, "--", "/bin/sh"]);
    assert_eq!(rep.status.code(), Some(0), "{}", text(&rep.stdout));
    // A version-1 reader must not take a recording with fakes.
    let mut v1 = fx.clone();
    v1["version"] = json!(1);
    let old = dir.path().join("v1.json");
    std::fs::write(&old, v1.to_string()).unwrap();
    let refused = run(&["check", old.to_str().unwrap()]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stdout).contains("version 1"),
        "{}",
        text(&refused.stdout)
    );
}

#[test]
fn replay_reports_a_diff_when_the_port_calls_a_fake_with_changed_argv() {
    let dir = tempfile::tempdir().unwrap();
    // No `argv`: one answer for any call, so only the call list differs.
    let f = fakes_file(dir.path(), &json!({"gh": [{"stdout": "ok\n"}]}));
    let out = dir.path().join("gh.json");
    let o = out.to_str().unwrap();
    let rec = record(&f, o, "gh pr list");
    assert_eq!(rec.status.code(), Some(0), "{}", text(&rec.stderr));
    let same = run(&["replay", o, "--", "/bin/sh"]);
    assert_eq!(same.status.code(), Some(0), "{}", text(&same.stdout));
    let changed = run(&["replay", o, "--", "/bin/sh", "-c", "gh pr view", "sh"]);
    let report = text(&changed.stdout);
    assert_eq!(changed.status.code(), Some(1), "{report}");
    assert!(report.contains("FakeCalls: call 0"), "{report}");
    assert!(report.contains("\"view\""), "{report}");
    assert!(
        !report.contains("Stdout"),
        "only the calls differ: {report}"
    );
}

#[test]
fn an_undeclared_real_binary_is_not_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let f = fakes_file(dir.path(), &json!({"gh": [{"stdout": "ok\n"}]}));
    let out = dir.path().join("git.json");
    let o = out.to_str().unwrap();
    // On record: the trap answers, and the recording is refused.
    let rec = record(&f, o, "git --version; gh pr list");
    assert_eq!(rec.status.code(), Some(2), "{}", text(&rec.stderr));
    assert!(text(&rec.stderr).contains("git"), "{}", text(&rec.stderr));
    assert!(!out.exists(), "a refused record writes no fixture");
    // On replay: a port that calls `git` meets the trap, not the host git.
    let rec = record(&f, o, "gh pr list");
    assert_eq!(rec.status.code(), Some(0), "{}", text(&rec.stderr));
    let port = "gh pr list; git --version; echo \"rc=$?\"";
    let rep = run(&["replay", o, "--", "/bin/sh", "-c", port, "sh"]);
    let report = text(&rep.stdout);
    assert_eq!(rep.status.code(), Some(1), "{report}");
    assert!(report.contains("undeclared executable \"git\""), "{report}");
    assert!(report.contains("rc=127"), "{report}");
    assert!(
        !report.contains("git version"),
        "the host git ran: {report}"
    );
}

#[test]
fn a_secret_shaped_value_in_a_fake_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("leak.json");
    let o = out.to_str().unwrap();
    // Built at run time so the source holds no secret-shaped literal.
    let token = format!("ghp_{}", "a1".repeat(12));
    let f = fakes_file(
        dir.path(),
        &json!({"gh": [{"stdout": format!("{token}\n")}]}),
    );
    // The script never prints the fake's stdout: the stub itself is refused.
    let rec = record(&f, o, "gh auth token > /dev/null");
    let err = text(&rec.stderr);
    assert_eq!(rec.status.code(), Some(1), "{err}");
    assert!(err.contains("fakes#") && err.contains("Secret"), "{err}");
    assert!(!err.contains(&token), "the refusal names no value: {err}");
    assert!(!out.exists(), "a refused record writes no fixture");
    // A URL password in an answer's argv is refused the same way.
    let url = format!("https://bot:{}@example.com/x", "hunter2");
    let f = fakes_file(
        dir.path(),
        &json!({"gh": [{"argv": ["api", url], "stdout": "ok\n"}]}),
    );
    let rec = record(&f, o, "gh pr list");
    let err = text(&rec.stderr);
    assert_eq!(rec.status.code(), Some(1), "{err}");
    assert!(err.contains("fakes#") && err.contains("Secret"), "{err}");
    assert!(!out.exists());
}

#[test]
fn a_recorded_path_env_cannot_displace_the_fake_stubs() {
    // #12028 fix round: `--env PATH=...` used to replace the stub PATH, so
    // the host git ran untrapped and the fixture held no calls.
    let dir = tempfile::tempdir().unwrap();
    let f = fakes_file(dir.path(), &json!({"gh": [{"stdout": "ok\n"}]}));
    let out = dir.path().join("path.json");
    let o = out.to_str().unwrap();
    for env in ["PATH=/usr/bin:/bin", "PATH", "HOME=/tmp"] {
        let rec = run(&[
            "record",
            "--fakes",
            &f,
            "--env",
            env,
            "--out",
            o,
            "--",
            "/bin/sh",
            "-c",
            "git --version; gh pr list",
        ]);
        let err = text(&rec.stderr);
        assert_eq!(rec.status.code(), Some(2), "--env {env}: {err}");
        assert!(
            err.contains("sets HOME and PATH itself"),
            "--env {env}: {err}"
        );
        assert!(
            !out.exists(),
            "--env {env}: a refused record writes no fixture"
        );
    }
}
