//! End-to-end proof that a hooked session may not run `tm fleet relaunch`
//! (#10033).
//!
//! Why: `tm fleet relaunch` (#9092) stops and replaces the Architect's
//! `claude` in its tmux pane. Owner ruling #8902 bars any session from
//! replacing that pane, so only an operator terminal outside every hooked
//! session may run it, and the deny holds under `TRUSTY_MPM_PM_UNRESTRICTED`
//! and `TRUSTY_MPM_DISABLE_HOOKS`.
//! What: the built `tm hook --pm-guard` judges a PM-stamped `Bash` payload in
//! a scratch `$HOME`, once per bypass.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_fleet_relaunch_10033::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// A scratch home and project directory.
struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir root");
        std::fs::create_dir_all(&project).expect("mkdir project");
        common::write_disk_threshold(&home, 100);
        Self {
            _dir: dir,
            home,
            project,
        }
    }

    /// The guard's stdout for a PM-stamped `command` under `env`.
    fn guard(&self, command: &str, env: &[(&str, &str)]) -> String {
        let mut cmd = common::tm_command_in(&self.home);
        cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
            .current_dir(&self.project)
            .env("CLAUDE_PROJECT_DIR", &self.project)
            .env("TRUSTY_MPM_SESSION_PROFILE", "pm")
            .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
            .env_remove("CLAUDE_MPM_SUB_AGENT")
            .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
            .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `tm hook --pm-guard`");
        finish(child, &payload(&self.project, command))
    }
}

/// A main-thread `Bash` payload running `command` in `project`.
fn payload(project: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-1",
        "cwd": project.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string()
}

/// Write `stdin` to the guard and return its stdout; it must exit 0.
fn finish(mut child: Child, stdin: &str) -> String {
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the guard exits 0: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// #10033: a hooked session running `tm fleet relaunch` is denied under each
/// bypass, in the spellings clap accepts; `tm fleet status` is not.
#[test]
fn a_hooked_session_is_denied_tm_fleet_relaunch_under_each_bypass() {
    let fx = Fixture::new();
    let denied = [
        "tm fleet relaunch",
        "tm fleet relaunch --session tm-architect",
        "tm fl rel",
        "trusty-mpm fleet relaunch",
        "bash -c 'tm fleet relaunch'",
        // #10033 critic r1: shell syntax, a fed shell, `cargo run` without `--`.
        "tm fleet relaunch 2>&1",
        "echo tm fleet relaunch | sh",
        "cargo run --bin tm fleet relaunch",
    ];
    let passed = ["tm fleet status", "git ls-files | xargs grep -l trusty-mpm"];
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in denied {
            let out = fx.guard(command, &env);
            assert!(
                out.contains("#10033") && out.contains("deny"),
                "{bypass:?} {command}: {out}"
            );
        }
        for command in passed {
            let out = fx.guard(command, &env);
            assert!(!out.contains("#10033"), "{bypass:?} {command}: {out}");
        }
    }
}
