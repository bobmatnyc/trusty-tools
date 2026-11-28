//! #12028 P1f: one plain run of each `trusty-architect-il-agent` subcommand.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! these prove the shipped binary finds the ledger and the R library under
//! the Architect checkout (marked by `ARCHITECT.md`) from a working directory
//! that is not the checkout, refuses with exit 1 and no writes when there is
//! no checkout, and asks tmux for a pane's session only through the `Tmux`
//! trait.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use trusty_architect::ScriptedTmux;
use trusty_architect::il_agent::cli::{Options, run};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-il-agent");

const STATE: &str = r#"{"code": "qx", "v": 1, "next": {},
 "goals": [{"id": "qx-G01", "rank": 1, "status": "open", "title": "Sample goal"}],
 "items": [{"id": "qx-D01", "goal": "qx-G01", "status": "open", "title": "Pick one"}],
 "tasks": [{"id": "qx-T01", "goal": "qx-G01", "status": "open", "title": "Sample task",
            "created": "2026-11-15T10:00Z"}]}
"#;
const RLIB: &str = r#"{"R01": {"title": "Ack", "options": {"a": "Merged {sha}."}}}"#;
const HANDBACK: &str =
    "```il\ntask: qx-T01@1115\ndone: R01-a sha=abc\npct: 50\nnext: review\n```\n";

/// A checkout with `ARCHITECT.md`, a ledger under `local/il`, the R library
/// under `local/ilio`, and a `sub/` directory to run from.
fn checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("ARCHITECT.md"), "# Architect\n").unwrap();
    let datg = root.join("local/il/.trusty-mpm/datg");
    std::fs::create_dir_all(&datg).unwrap();
    std::fs::write(datg.join("state.json"), STATE).unwrap();
    std::fs::create_dir_all(root.join("local/ilio")).unwrap();
    std::fs::write(root.join("local/ilio/r-library.json"), RLIB).unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    dir
}

fn command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.args(args).current_dir(cwd);
    for k in [
        "ILIO_PROJECT",
        "ILIO_DIR",
        "DATG_STATE_DIR",
        "DATG_SESSION",
        "TMUX_PANE",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

fn run_bin(cwd: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = command(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    use std::io::Write;
    // A run refused before it reads stdin may close the pipe first.
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    child.wait_with_output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Every file under `dir`, for the no-writes checks.
fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(dir).unwrap().display().to_string());
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn assert_refused(out: &Output, foreign: &Path) {
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("il_agent.py: no Architect checkout (ARCHITECT.md)"),
        "{}",
        text(&out.stderr)
    );
    assert!(tree(foreign).is_empty(), "{:?}", tree(foreign));
}

#[test]
fn shipped_il_agent_brief_reads_the_checkout_ledger() {
    let co = checkout();
    let before = tree(co.path());
    let out = run_bin(&co.path().join("sub"), &["brief", "qx-T01", "qx-D01"], "");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let brief = text(&out.stdout);
    assert!(
        brief.starts_with(
            "IL BRIEF qx-T01@1115 (ledger qx, read-only)\ngoal: qx-G01 r1 open Sample goal\n"
        ),
        "{brief}"
    );
    assert!(
        brief.contains("\ncited qx-D01: qx-D01 open Pick one\n"),
        "{brief}"
    );
    assert!(brief.ends_with("```il\ntask: qx-T01@1115\ndone: <what you did, with a sha or path>\npct: <0-100>\nnext: <what the PM should do next>\n```\n"), "{brief}");
    assert_eq!(tree(co.path()), before, "brief wrote a file");
}

#[test]
fn shipped_il_agent_dispatch_names_the_binary() {
    let co = checkout();
    let out = run_bin(
        &co.path().join("sub"),
        &["dispatch", "qx-T01", "--ask", "port it"],
        "",
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let lines = text(&out.stdout);
    let mut it = lines.lines();
    assert_eq!(it.next(), Some("Task: qx-T01@1115"));
    let il = it.next().unwrap();
    assert!(
        il.starts_with("IL: ") && il.ends_with(" brief qx-T01@1115"),
        "{il}"
    );
    // The IL line runs: it is this binary printing the brief.
    let cmd = il
        .trim_start_matches("IL: ")
        .trim_end_matches(" brief qx-T01@1115");
    let exe = Path::new(cmd.trim_matches('\''));
    assert_eq!(
        exe.file_name().and_then(|n| n.to_str()),
        Some("trusty-architect-il-agent")
    );
    assert_eq!(it.next(), Some("Ask: port it"));
    let brief = run_bin(&co.path().join("sub"), &["brief", "qx-T01@1115"], "");
    assert_eq!(brief.status.code(), Some(0), "{}", text(&brief.stderr));
}

#[test]
fn shipped_il_agent_handback_validates_stdin() {
    let co = checkout();
    let before = tree(co.path());
    let out = run_bin(
        &co.path().join("sub"),
        &["handback", "--task", "qx-T01"],
        HANDBACK,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "OK\ntask: qx-T01@1115 (Sample task)\ndone: Merged abc.\npct: 50\nnext: review\n\
         pulse: Task: qx-T01@1115 | done: R01-a sha=abc | pct: 50\n"
    );
    let out = run_bin(&co.path().join("sub"), &["handback"], "no block\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        "IL-REJECT NO-BLOCK il: end your final message with one ```il fenced block\n"
    );
    assert_eq!(tree(co.path()), before, "handback wrote a file");
}

#[test]
fn shipped_il_agent_finds_the_checkout_from_a_foreign_directory() {
    let co = checkout();
    let foreign = tempfile::tempdir().unwrap();
    for args in [
        &["brief", "qx-T01"][..],
        &["dispatch", "qx-T01"],
        &["handback"],
    ] {
        assert_refused(&run_bin(foreign.path(), args, HANDBACK), foreign.path());
    }
    // `--project` alone still needs the checkout's R library.
    let project = co.path().join("local/il");
    let p = project.to_str().unwrap();
    let out = run_bin(foreign.path(), &["--project", p, "brief", "qx-T01"], "");
    assert_refused(&out, foreign.path());
    // `--project` and `$ILIO_DIR` name both: no checkout is needed.
    let out = command(foreign.path(), &["brief", "qx-T01", "--project", p])
        .env("ILIO_DIR", co.path().join("local/ilio"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("IL BRIEF qx-T01@1115"));
    assert!(tree(foreign.path()).is_empty());
}

#[test]
fn il_agent_asks_tmux_through_the_trait_for_the_pane_session() {
    // Two per-PM ledgers; only the pane's session names `pm-two`.
    let co = checkout();
    let datg = co.path().join("local/il/.trusty-mpm/datg");
    for (name, code) in [("pm-one", "qa"), ("pm-two", "qb")] {
        std::fs::create_dir(datg.join(name)).unwrap();
        let st = STATE.replace("\"qx\"", &format!("\"{code}\""));
        std::fs::write(datg.join(name).join("state.json"), st).unwrap();
    }
    let tmux = ScriptedTmux::new().script("display-message", "pm-two\n");
    let env = |k: &str| (k == "TMUX_PANE").then(|| "%7".to_owned());
    let mut stdin = std::io::empty();
    let mut opts = Options {
        checkout: Some(co.path().to_path_buf()),
        ilio_project: None,
        ilio_dir: None,
        brief_cmd: vec!["il-agent".into()],
        env: &env,
        tmux: &tmux,
        stdin: &mut stdin,
    };
    let out = run(&["brief".into(), "qx-T01".into()], &mut opts);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .starts_with("IL BRIEF qx-T01@1115 (ledger qb, read-only)"),
        "{}",
        out.stdout
    );
    assert_eq!(
        tmux.calls(),
        [["display-message", "-p", "-t", "%7", "#{session_name}"]]
    );

    // A failed tmux call is no session: the legacy directory, which has the `qx` ledger.
    let tmux = ScriptedTmux::new().fail("display-message", "no server");
    opts.tmux = &tmux;
    let out = run(&["brief".into(), "qx-T01".into()], &mut opts);
    assert!(
        out.stdout
            .starts_with("IL BRIEF qx-T01@1115 (ledger qx, read-only)"),
        "{}",
        out.stdout
    );
}
