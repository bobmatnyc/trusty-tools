//! `tm architect <verb>` through the built binary (#9822, #9171 A4).
//!
//! Why: the verbs replace the Architect's Python scripts for callers that
//! read stdout and exit status, so only the binary proves the swap: the same
//! argv, the same stdout, and an exit status that fails closed.
//! What: each test runs the built `tm` under a scratch `$HOME` with
//! `crates/trusty-architect/tests/fixtures/a4/fake-tmux.sh` first on `PATH`
//! as `tmux` (no tmux server is reached) and compares stdout with the goldens
//! `regen_a4.py` recorded from the prototype's Python. Every path the poller
//! could write is under a tempdir.
//! Test: `cargo test -p trusty-mpm --test integration tm_architect_verbs::`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common;

/// Variables that would point a spawned poller at a live Architect.
const ARCHITECT_ENV: &[&str] = &[
    "TMUX",
    "TMUX_SOCKET",
    "TMUX_PANE",
    "ARCHITECT_PROJECT_DIR",
    "ARCHITECT_SESSION",
    "ARCHITECT_POLL_SESSION",
    "ARCHITECT_INBOX_DIR",
    "ARCHITECT_QUIET_SESSIONS_FILE",
    "SUPERVISOR_INBOX_DIR",
    "SESSIONS_DIR",
    "TRANSCRIPT_BASE",
    "SELF_CTX_DIR",
    "SELF_CTX_WINDOW",
    "SELF_CTX_THRESHOLD",
    "PANE_CTX_WINDOW",
    "LOAD_FACTOR",
];

fn a4() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../trusty-architect/tests/fixtures/a4")
}

/// A scratch home and Architect directory, and a bin dir whose `tmux` is
/// the fixture fake.
struct VerbEnv {
    home: tempfile::TempDir,
    architect: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl VerbEnv {
    fn new() -> Self {
        let bin = tempfile::tempdir().expect("bin dir");
        std::os::unix::fs::symlink(a4().join("fake-tmux.sh"), bin.path().join("tmux"))
            .expect("fake tmux");
        Self {
            home: tempfile::tempdir().expect("scratch home"),
            architect: tempfile::tempdir().expect("architect dir"),
            bin,
        }
    }

    /// `tm architect <args>` with the fake tmux answering from `panes`.
    fn command(&self, panes: &Path, args: &[&str]) -> Command {
        let mut cmd = common::tm_command_in(self.home.path());
        for key in ARCHITECT_ENV {
            cmd.env_remove(key);
        }
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", self.bin.path().display()))
            .env("FAKE_TMUX_PANES", panes)
            .env("ARCHITECT_PROJECT_DIR", self.architect.path())
            .arg("architect")
            .args(args);
        cmd
    }

    fn run(&self, panes: &Path, args: &[&str]) -> Output {
        self.command(panes, args).output().expect("run tm")
    }
}

fn panes() -> PathBuf {
    a4().join("panes")
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(a4().join(name)).expect("golden")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        stdout(out),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The prototype's `input-state.py <every target> %99 =tm-gone:` printed
/// `input_state_cli.txt` and exited 0; the verb prints the same bytes and
/// exits 1 for the two panes it could not capture.
#[test]
fn input_state_binary_matches_python_and_fails_closed() {
    let env = VerbEnv::new();
    let map = std::fs::read_to_string(panes().join("targets.tsv")).expect("targets");
    let mut args = vec!["input-state"];
    args.extend(map.lines().filter_map(|l| l.split('\t').next()));
    args.extend(["%99", "=tm-gone:"]);
    let out = env.run(&panes(), &args);
    assert_eq!(
        stdout(&out),
        golden("input_state_cli.txt"),
        "{}",
        text(&out)
    );
    assert!(golden("cli_exit.json").contains("\"input_state\": 0"));
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("can't find pane: %99"),
        "{}",
        text(&out)
    );
    // Every target captured: exit 0, as the script.
    let ok = env.run(&panes(), &["input-state", "%3", "=tm-widget-01:"]);
    assert_eq!(ok.status.code(), Some(0), "{}", text(&ok));
    assert_eq!(
        stdout(&ok),
        "%3 empty | \n=tm-widget-01: suggestion | Pause lifted, start #101\n"
    );
}

/// A bare session name is prefix-matched by tmux (#8443): refused, exit 2.
#[test]
fn input_state_binary_refuses_an_inexact_target() {
    let env = VerbEnv::new();
    let out = env.run(&panes(), &["input-state", "%3", "tm-dogfood"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert_eq!(stdout(&out), "", "nothing is captured before the check");
}

#[test]
fn classify_binary_matches_python() {
    let env = VerbEnv::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(panes())
        .expect("panes")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ansi"))
        .collect();
    files.sort();
    let mut got = String::new();
    for file in &files {
        let out = env.run(&panes(), &["classify", &file.display().to_string()]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        got.push_str(&stdout(&out));
    }
    assert_eq!(got, golden("classify.jsonl"));

    // stdin, and an empty capture that must not print a default.
    let mut child = env
        .command(&panes(), &["classify"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run tm");
    assert_eq!(child.status.code(), Some(1), "{}", text(&child));
    assert_eq!(stdout(&child), "");
    child = env.run(&panes(), &["classify", "/nonexistent/capture.ansi"]);
    assert_eq!(child.status.code(), Some(1), "{}", text(&child));
}

/// argparse refuses each `poll_args.jsonl` argv with exit 2; so does the verb.
#[test]
fn poll_binary_refuses_what_argparse_refuses() {
    let env = VerbEnv::new();
    let mut rows = 0;
    for line in golden("poll_args.jsonl").lines() {
        let row: serde_json::Value = serde_json::from_str(line).expect("row");
        let argv: Vec<String> = row[0]
            .as_array()
            .expect("argv")
            .iter()
            .map(|a| a.as_str().unwrap_or_default().to_owned())
            .collect();
        let mut args = vec!["poll"];
        args.extend(argv.iter().map(String::as_str));
        let out = env.run(&panes(), &args);
        assert_eq!(
            out.status.code().map(i64::from),
            row[1].as_i64(),
            "{argv:?}: {}",
            text(&out)
        );
        rows += 1;
    }
    assert!(rows >= 4, "{rows}");
    assert!(
        !env.architect.path().join("inbox").exists(),
        "a refused poll wrote its inbox"
    );
}

/// The prototype logs a failed `list-panes` and exits 0; `--once` exits 1.
#[test]
fn poll_once_binary_fails_closed_without_tmux() {
    let env = VerbEnv::new();
    let empty = tempfile::tempdir().expect("no panes");
    std::fs::write(empty.path().join("targets.tsv"), "").expect("targets");
    let out = env.run(empty.path(), &["poll", "--once"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cycle incomplete"),
        "{}",
        text(&out)
    );
    let log =
        std::fs::read_to_string(env.architect.path().join("inbox/poll.log")).expect("poll.log");
    assert!(log.contains("poll start interval=60s"), "{log}");
    assert!(log.contains("panes=? ERROR"), "{log}");
}

/// `--dry-run` prints the prototype's per-pane line for each watched pane
/// (`poll_dry_panes.txt`), writes nothing and exits 0.
#[test]
fn poll_dry_run_binary_prints_the_python_pane_lines() {
    let env = VerbEnv::new();
    let out = env.run(&panes(), &["poll", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let lines: String = stdout(&out)
        .lines()
        .filter(|l| l.starts_with("  tm-"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(lines, golden("poll_dry_panes.txt"), "{}", text(&out));
    assert!(stdout(&out).contains(" panes=8 "), "{}", text(&out));
    assert!(
        !env.architect.path().join("inbox").exists(),
        "a dry run wrote"
    );
}

#[test]
fn quiet_binary_lists_names_and_fails_closed() {
    let env = VerbEnv::new();
    let out = env.run(&panes(), &["quiet"]);
    assert_eq!(out.status.code(), Some(1), "a missing list: {}", text(&out));
    assert_eq!(stdout(&out), "");
    let local = env.architect.path().join("local");
    std::fs::create_dir_all(&local).expect("local");
    std::fs::write(
        local.join("quiet-sessions.txt"),
        "tm-b  # parked\n\n# c\ntm-a\n",
    )
    .expect("list");
    let out = env.run(&panes(), &["quiet"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(stdout(&out), "tm-a\ntm-b\n");
}

#[test]
fn self_ctx_binary_prints_tokens_and_fails_closed() {
    let env = VerbEnv::new();
    let dir = tempfile::tempdir().expect("transcripts");
    let dir_arg = dir.path().display().to_string();
    let out = env.run(&panes(), &["self-ctx", &dir_arg]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "nothing to measure: {}",
        text(&out)
    );
    assert_eq!(stdout(&out), "");
    std::fs::write(
        dir.path().join("s.jsonl"),
        "{\"entrypoint\": \"cli\", \"type\": \"assistant\", \"message\": {\"usage\": \
         {\"input_tokens\": 10, \"cache_read_input_tokens\": 20}}}\n",
    )
    .expect("transcript");
    let out = env.run(&panes(), &["self-ctx", &dir_arg]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(stdout(&out), "30\n");
}

/// #10063: `tm architect dashboard` writes the store in-process; a stale
/// `--if-version` exits 1 (owner ruling D34) and leaves the bytes, and a
/// read prints the document as `dashboard-db.py get` does.
#[test]
fn dashboard_binary_conflict_exits_1_and_writes_nothing() {
    let env = VerbEnv::new();
    let root = env.architect.path().join("store");
    let root_arg = root.display().to_string();
    let dash = |args: &[&str]| {
        let mut cmd = env.command(&panes(), &["dashboard", "--root", &root_arg]);
        for key in ["DASHBOARD_ROOT", "DASHBOARD_FORMAT", "DASHBOARD_ACTOR"] {
            cmd.env_remove(key);
        }
        cmd.args(args).output().expect("run tm")
    };
    let set = dash(&["set", "items", "D01", "--json", r#"{"title": "é"}"#]);
    assert_eq!(set.status.code(), Some(0), "{}", text(&set));
    let doc = root.join("items").join("D01.json");
    let before = std::fs::read(&doc).expect("written");
    let stale = dash(&[
        "set",
        "items",
        "D01",
        "--json",
        r#"{"title": "x"}"#,
        "--if-version",
        "7",
    ]);
    assert_eq!(stale.status.code(), Some(1), "{}", text(&stale));
    assert_eq!(std::fs::read(&doc).expect("kept"), before, "bytes changed");
    let get = dash(&["get", "items", "D01"]);
    assert_eq!(get.status.code(), Some(0), "{}", text(&get));
    assert_eq!(
        stdout(&get),
        "{\n  \"id\": \"D01\",\n  \"version\": 1,\n  \"data\": {\n    \"title\": \"é\"\n  }\n}\n"
    );
}
