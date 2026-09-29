//! End-to-end proof of the Architect pane floor (#8902).
//!
//! Why: owner ruling 2026-09-29 12:20Z — a session that is not the Architect
//! is denied every tmux verb that types into or replaces the Architect's pane,
//! and the deny holds under `TRUSTY_MPM_PM_UNRESTRICTED` and
//! `TRUSTY_MPM_DISABLE_HOOKS`. Only the built binary, a real launch record and
//! a real tmux server prove the live probe resolves pane, window and session
//! ids to the Architect.
//! What: a private tmux server (its own `TMUX_TMPDIR`) runs `tm-architect` and
//! `tm8902-pm`; a scratch `$HOME` records a live `sleep` as the Architect's
//! launch. `tm hook --pm-guard` runs as a PM under each bypass.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_architect_pane_8902::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use trusty_mpm::core::architect_launch::record_architect;

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// A scratch home, a private tmux server and a live recorded Architect.
struct Fixture {
    _dir: tempfile::TempDir,
    tmux_dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    architect: Child,
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
        // A short path: a tmux socket path is capped near 104 bytes on macOS.
        let tmux_dir = tempfile::tempdir_in("/tmp").expect("tmux dir");
        let architect = Command::new("sleep")
            .arg("120")
            .spawn()
            .expect("spawn the recorded Architect");
        record_architect(&home.join(".trusty-mpm"), architect.id(), &project)
            .expect("record the Architect");
        let fx = Self {
            _dir: dir,
            tmux_dir,
            home,
            project,
            architect,
        };
        for name in ["tm-architect", "tm8902-pm"] {
            fx.tmux(&["new-session", "-d", "-s", name, "sleep", "600"]);
        }
        fx
    }

    /// Run tmux on the private server; it must succeed.
    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .args(args)
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .expect("run tmux");
        assert!(out.status.success(), "tmux {args:?}: {out:?}");
        String::from_utf8(out.stdout)
            .expect("utf-8")
            .trim()
            .to_owned()
    }

    /// The Architect session's `%N @N $N` ids.
    fn architect_ids(&self) -> Vec<String> {
        let ids = self.tmux(&[
            "display-message",
            "-p",
            "-t",
            "=tm-architect:",
            "#{pane_id} #{window_id} #{session_id}",
        ]);
        ids.split(' ').map(str::to_owned).collect()
    }

    /// The guard's stdout for a PM-stamped `command` under `env`.
    fn guard(&self, command: &str, env: &[(&str, &str)]) -> String {
        let mut cmd = common::tm_command_in(&self.home);
        cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
            .current_dir(&self.project)
            .env("CLAUDE_PROJECT_DIR", &self.project)
            .env("TRUSTY_MPM_SESSION_PROFILE", "pm")
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            // The scratch `$HOME` otherwise refuses tmux spawns (#5784).
            .env("TRUSTY_MPM_ALLOW_HOST_STATE", "1")
            .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
            .env_remove("CLAUDE_MPM_SUB_AGENT")
            .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
            .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT");
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

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.architect.kill();
        let _ = self.architect.wait();
        let _ = Command::new("tmux")
            .arg("kill-server")
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            .env_remove("TMUX")
            .status();
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

/// Owner ruling 2026-09-29 12:20Z: a PM is denied the Architect's pane by
/// session name, pane id, window id, session id and an expanded target,
/// under each bypass, and keeps its own tmux use.
#[test]
fn a_pm_is_denied_the_architect_pane_under_each_bypass() {
    let fx = Fixture::new();
    let ids = fx.architect_ids();
    let [pane, window, session] = ids.as_slice() else {
        panic!("three ids: {ids:?}");
    };
    let denied = [
        "tmux send-keys -t =tm-architect: 'hello' Enter".to_owned(),
        "tmux kill-session -t =tm-architect".to_owned(),
        format!("tmux respawn-pane -k -t {pane}"),
        format!("tmux kill-window -t {window}"),
        format!("tmux kill-session -t '{session}'"),
        "tmux send-keys -t \"$T\" x".to_owned(),
    ];
    let allowed = [
        "tmux send-keys -t =tm8902-pm: 'Run the gates' Enter",
        "tmux capture-pane -p -t =tm-architect: -S -200",
    ];
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in &denied {
            let out = fx.guard(command, &env);
            assert!(out.contains("#8902"), "{bypass:?} {command}: {out}");
        }
        for command in allowed {
            let out = fx.guard(command, &env);
            assert!(!out.contains("#8902"), "{bypass:?} {command}: {out}");
        }
    }
}

/// With no live Architect launch record the rule does not apply.
#[test]
fn no_live_architect_record_lifts_the_pane_floor() {
    let mut fx = Fixture::new();
    fx.architect.kill().expect("kill the recorded Architect");
    fx.architect.wait().expect("reap it");
    let out = fx.guard("tmux kill-session -t =tm-architect", &[]);
    assert!(!out.contains("#8902"), "{out}");
}
