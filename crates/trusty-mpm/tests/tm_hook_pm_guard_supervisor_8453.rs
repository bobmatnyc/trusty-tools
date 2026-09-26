//! End-to-end proof that the supervisor profile, not `TRUSTY_MPM_PM_UNRESTRICTED`,
//! defines a supervisor session's guard behaviour (#8453).
//!
//! Why: a supervisor acts directly, so the PM delegation rules must not bind
//! it, while the destructive-command and worktree guards must. Only the built
//! binary proves the profile is read from the launch directory the hook sees.
//! What: spawns `tm hook --pm-guard` with `CLAUDE_PROJECT_DIR` naming a temp
//! project, `TRUSTY_MPM_PM_UNRESTRICTED` unset, and an unreachable daemon.
//! A budget-eligible direct edit (`sed -i`) is denied on its fourth call for a
//! PM and never for a supervisor; `rm -rf /` is denied for both.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard_supervisor_8453`.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// A direct in-place edit: allowed three times per turn for a PM, then denied.
const SED_EDIT: &str = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"sed -i s/a/b/ src/lib.rs"}}"#;

/// A destructive delete every caller is refused.
const RM_ROOT: &str =
    r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#;

/// Run the guard for a session launched in `project`, returning stdout.
fn run_guard(payload: &str, project: &Path, home: &Path) -> String {
    common::write_disk_threshold(home, 100);
    let mut child = common::tm_command_in(home)
        .args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(project)
        .env("HOME", home)
        .env("CLAUDE_PROJECT_DIR", project)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the guard exits 0: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// A temp project with `.trusty-mpm.toml` holding `toml`.
fn project_with(toml: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".trusty-mpm.toml"), toml).expect("write config");
    dir
}

/// The verdict on the fourth consecutive `sed -i` in `project`.
fn fourth_direct_edit(project: &Path) -> String {
    let home = tempfile::tempdir().expect("home");
    for _ in 1..=3 {
        assert_eq!(run_guard(SED_EDIT, project, home.path()).trim(), "");
    }
    run_guard(SED_EDIT, project, home.path())
}

#[test]
fn a_supervisor_session_is_not_bound_by_the_pm_delegation_rules() {
    let supervisor = project_with("profile = \"supervisor\"\n");
    assert_eq!(fourth_direct_edit(supervisor.path()).trim(), "");
}

#[test]
fn a_pm_session_is_still_bound_by_the_pm_delegation_rules() {
    let pm = project_with("profile = \"pm\"\n");
    assert!(fourth_direct_edit(pm.path()).contains("\"deny\""));
}

#[test]
fn an_undecidable_profile_keeps_the_pm_guards() {
    // Fail-open to the PM profile: a malformed config is not a supervisor.
    let broken = project_with("profile = \"supervisor\"\n[[[ not toml\n");
    assert!(fourth_direct_edit(broken.path()).contains("\"deny\""));
}

#[test]
fn a_supervisor_session_is_still_refused_a_destructive_delete() {
    let supervisor = project_with("profile = \"supervisor\"\n");
    let home = tempfile::tempdir().expect("home");
    let stdout = run_guard(RM_ROOT, supervisor.path(), home.path());
    assert!(stdout.contains("\"deny\""), "{stdout}");
}
