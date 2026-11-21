//! #7684: the project-rules carrier through the real binary.
//!
//! Why: deployed agents start with `omitClaudeMd: true`, so a dispatch that
//! reaches a subagent without the `## Project rules` block leaves it with no
//! build or test rules at all. The pm-guard bypasses are operator escape
//! hatches and turn the carrier off with the rest of the hook, and
//! `[pm_guard] enabled = false` removes the pm-guard entry, where the
//! always-installed egress guard carries the block instead.
//! What: pipes an `Agent` dispatch whose `cwd` holds a `CLAUDE.md` into
//! `tm hook --pm-guard` under each bypass variable, and into
//! `tm hook --egress-guard` with the guard on and off. Every child runs in its
//! own process, so no test touches this process's environment.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_rules_carrier_7684`.

use crate::common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Nothing listens on port 1, so no audit POST reaches a daemon.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// The heading the carrier appends; kept literal so a renamed const fails here.
const HEADING: &str = "## Project rules (from CLAUDE.md)";

/// The rule text the project's `CLAUDE.md` carries.
const RULE: &str = "run cargo test -p x --no-fail-fast";

/// The env vars a test strips before setting its own.
const SCRUB: &[&str] = &[
    "TRUSTY_MPM_DISABLE_HOOKS",
    "TRUSTY_MPM_PM_UNRESTRICTED",
    "CLAUDE_MPM_SUB_AGENT",
    "TRUSTY_MPM_PM_DENY_BY_DEFAULT",
    "TRUSTY_MPM_SESSION_PROFILE",
];

/// A project directory whose `CLAUDE.md` is the one rule.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project dir");
    std::fs::write(dir.path().join("CLAUDE.md"), RULE).expect("write CLAUDE.md");
    dir
}

/// A home whose `~/.trusty-mpm/config.toml` is `config`.
fn home(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("home dir");
    let root = dir.path().join(".trusty-mpm");
    std::fs::create_dir_all(&root).expect("mkdir .trusty-mpm");
    std::fs::write(root.join("config.toml"), config).expect("write config");
    dir
}

/// An `Agent` dispatch payload rooted at `cwd`.
fn dispatch(cwd: &Path) -> String {
    serde_json::json!({
        "session_id": "s-7684",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Agent",
        "tool_input": {"subagent_type": "research", "prompt": "look"},
    })
    .to_string()
}

/// Run `tm hook <mode>` over `stdin` and return its stdout.
fn run(mode: &str, home: &Path, data: &Path, stdin: &str, env: &[(&str, &str)]) -> String {
    let mut command: Command = common::tm_command_in(home);
    command
        .args(["--url", UNREACHABLE_DAEMON, "hook", mode])
        .env("TRUSTY_DATA_DIR_OVERRIDE", data)
        .current_dir(data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in SCRUB {
        command.env_remove(key);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn tm hook");
    let mut pipe = child.stdin.take().expect("child stdin");
    let _ = pipe.write_all(stdin.as_bytes());
    drop(pipe);
    let output = child.wait_with_output().expect("wait for tm hook");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

/// The rewritten prompt in a carrier response, or a panic naming `stdout`.
fn carried_prompt(stdout: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("no carrier object ({e}): {stdout:?}"));
    let out = &v["hookSpecificOutput"];
    assert!(out.get("permissionDecision").is_none(), "{stdout}");
    out["updatedInput"]["prompt"]
        .as_str()
        .unwrap_or_else(|| panic!("no rewritten prompt: {stdout}"))
        .to_string()
}

/// An operator escape hatch means hooks off: no carrier, no output.
/// Fails against 6aa6305d08, where the bypass still emitted the carrier.
#[test]
fn a_bypassed_dispatch_carries_no_rules() {
    let project = project();
    let home = home("");
    for var in ["TRUSTY_MPM_DISABLE_HOOKS", "TRUSTY_MPM_PM_UNRESTRICTED"] {
        let data = tempfile::tempdir().expect("data dir");
        let out = run(
            "--pm-guard",
            home.path(),
            data.path(),
            &dispatch(project.path()),
            &[(var, "1")],
        );
        assert_eq!(out.trim(), "", "{var}: the bypass must print nothing");
    }
}

/// Without a bypass the same dispatch carries the rules.
#[test]
fn an_unbypassed_dispatch_carries_the_rules() {
    let project = project();
    let home = home("");
    // #9011: an enforcing pm-guard needs the agent roster to allow a dispatch.
    common::stage_repo_content(home.path());
    let data = tempfile::tempdir().expect("data dir");
    let out = run(
        "--pm-guard",
        home.path(),
        data.path(),
        &dispatch(project.path()),
        &[],
    );
    let prompt = carried_prompt(&out);
    assert!(prompt.starts_with("look"), "{prompt}");
    assert!(prompt.contains(HEADING), "{prompt}");
    assert!(prompt.ends_with(RULE), "{prompt}");
}

/// Fails against 86f82e6cf3: with the pm-guard entry gone nothing carried the
/// rules. With the guard on, the egress hook stays silent so the input is
/// rewritten once.
#[test]
fn the_egress_carrier_runs_only_when_pm_guard_is_off() {
    let project = project();
    let data = tempfile::tempdir().expect("data dir");
    let off = home("[pm_guard]\nenabled = false\n");
    let out = run(
        "--egress-guard",
        off.path(),
        data.path(),
        &dispatch(project.path()),
        &[],
    );
    let prompt = carried_prompt(&out);
    assert!(prompt.contains(HEADING), "{prompt}");
    assert!(prompt.ends_with(RULE), "{prompt}");

    let on = home("");
    let out = run(
        "--egress-guard",
        on.path(),
        data.path(),
        &dispatch(project.path()),
        &[],
    );
    assert_eq!(out.trim(), "", "an enabled pm-guard is the only carrier");
}
