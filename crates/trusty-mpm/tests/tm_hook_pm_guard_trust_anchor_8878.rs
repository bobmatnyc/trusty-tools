//! End-to-end proof that the trust-anchor floor binds the bypasses (#8878 D8).
//!
//! Why: owner ruling 2026-09-29 — a write to `~/.trusty-mpm/config.toml` is
//! denied for every session but the Architect's main thread, and the deny
//! holds under `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`,
//! which return before every other rule. Only the built binary proves the
//! stdin read moved ahead of them.
//! What: spawns `tm hook --pm-guard` with a scratch `$HOME` holding the anchor
//! and an unreachable daemon, and checks each bypass, an unreadable payload
//! under each bypass, and the Architect's main thread and subagent — run under
//! a fake `claude` recorded as the Architect's launch, and the same
//! environment with no record (ruling A).
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_trust_anchor_8878::`.

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

/// A scratch home with the anchor, and an Architect project allow-listed in it.
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
        let project = root.join("architect");
        std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir root");
        std::fs::create_dir_all(&project).expect("mkdir project");
        std::fs::write(
            project.join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .expect("write profile");
        std::fs::write(
            home.join(".trusty-mpm/config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                project.display().to_string()
            ),
        )
        .expect("write anchor");
        common::write_disk_threshold(&home, 100);
        Self {
            _dir: dir,
            home,
            project,
        }
    }

    fn anchor(&self) -> PathBuf {
        self.home.join(".trusty-mpm/config.toml")
    }
}

/// Run the guard with raw `stdin`, the named env pairs, and `stamp`.
fn run(fx: &Fixture, stdin: &str, env: &[(&str, &str)], stamp: Option<&str>) -> String {
    let mut cmd = common::tm_command_in(&fx.home);
    cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"]);
    guard_env(fx, &mut cmd, env, stamp);
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    finish(child, stdin)
}

/// The hook's environment: the project, `stamp`, and `env` over no bypass.
fn guard_env(fx: &Fixture, cmd: &mut Command, env: &[(&str, &str)], stamp: Option<&str>) {
    cmd.current_dir(&fx.project)
        .env("CLAUDE_PROJECT_DIR", &fx.project)
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT");
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(stamp) = stamp {
        cmd.env("TRUSTY_MPM_SESSION_PROFILE", stamp);
    }
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

/// Run the supervisor-stamped guard as the child of a fake `claude` (a
/// `/bin/sh` symlink named `claude`), recording that `claude` as the
/// Architect's launch first when `recorded` — what `tm fleet init` does.
fn run_under_claude(fx: &Fixture, stdin: &str, recorded: bool) -> String {
    let scratch = fx.home.parent().expect("scratch root");
    let fake = scratch.join("claude");
    if !fake.exists() {
        // A symlink, not a copy: macOS kills an unsigned copy of a system binary.
        std::os::unix::fs::symlink("/bin/sh", &fake).expect("symlink fake claude");
    }
    let go = scratch.join(format!("go-{recorded}"));
    let mut cmd = Command::new(&fake);
    common::isolate_spawned_tm(&mut cmd, &fx.home);
    // The shell waits for the record, then runs the guard as its own child.
    cmd.args([
        "-c",
        "while [ ! -e \"$GO\" ]; do sleep 0.05; done; \"$TM\" --url http://127.0.0.1:1 hook \
         --pm-guard; exit $?",
    ])
    .env("GO", &go)
    .env("TM", common::tm_bin());
    guard_env(fx, &mut cmd, &[], Some("supervisor"));
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the fake claude");
    if recorded {
        record_architect(&fx.home.join(".trusty-mpm"), child.id(), &fx.project)
            .expect("record the fake claude");
    }
    std::fs::write(&go, "").expect("release the fake claude");
    finish(child, stdin)
}

/// A main-thread `Write` payload targeting `path`.
fn write_payload(fx: &Fixture, path: &Path) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-1",
        "cwd": fx.project.display().to_string(),
        "tool_name": "Write",
        "tool_input": { "file_path": path.display().to_string(), "content": "x" },
    })
}

#[test]
fn the_floor_denies_an_anchor_write_under_each_bypass() {
    let fx = Fixture::new();
    let stdin = write_payload(&fx, &fx.anchor()).to_string();
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        let out = run(&fx, &stdin, &env, None);
        assert!(out.contains("\"deny\""), "{bypass:?}: {out}");
        assert!(out.contains("#8878"), "{bypass:?}: {out}");
    }
    // A bypass still lifts every other rule: a write elsewhere passes.
    let other = write_payload(&fx, &fx.project.join("notes.md")).to_string();
    for bypass in BYPASSES.into_iter().flatten() {
        assert_eq!(run(&fx, &other, &[bypass], None).trim(), "", "{bypass:?}");
    }
}

#[test]
fn an_unreadable_payload_denies_under_each_bypass() {
    let fx = Fixture::new();
    for bypass in BYPASSES.into_iter().flatten() {
        for stdin in ["", "not json", "[1]"] {
            let out = run(&fx, stdin, &[bypass], None);
            assert!(out.contains("\"deny\""), "{bypass:?} {stdin:?}: {out}");
        }
    }
}

/// Ruling A (#8878): the Architect is the recorded `claude`, over the real
/// process table; the environment alone is the critic's spoof.
#[test]
fn the_architect_main_thread_writes_and_its_subagent_does_not() {
    let fx = Fixture::new();
    let main = write_payload(&fx, &fx.anchor());
    assert_eq!(run_under_claude(&fx, &main.to_string(), true).trim(), "");
    let mut sub = main.clone();
    sub["agent_id"] = serde_json::json!("agent-7");
    let out = run_under_claude(&fx, &sub.to_string(), true);
    assert!(out.contains("#8878"), "{out}");
    // The stamp, project and allowlist with no record: the spoof.
    let out = run_under_claude(&fx, &main.to_string(), false);
    assert!(out.contains("#8878"), "{out}");
    let out = run(&fx, &main.to_string(), &[], Some("supervisor"));
    assert!(out.contains("#8878"), "{out}");
    // A PM stamp in the same directory is not the Architect.
    let out = run(&fx, &main.to_string(), &[], Some("pm"));
    assert!(out.contains("#8878"), "{out}");
}
