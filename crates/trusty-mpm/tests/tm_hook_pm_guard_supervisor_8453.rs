//! End-to-end proof that the supervisor profile, not `TRUSTY_MPM_PM_UNRESTRICTED`,
//! defines a supervisor session's guard behaviour (#8453).
//!
//! Why: a supervisor acts directly, so the PM delegation rules must not bind
//! it, while the destructive-command and worktree guards must. The exemption
//! needs three things to agree — the launch stamp, the user-level allowlist and
//! the project file — and only the built binary proves the hook reads them.
//! What: spawns `tm hook --pm-guard` with a scratch `$HOME` (its
//! `.trusty-mpm/config.toml` is the allowlist), `CLAUDE_PROJECT_DIR` naming a
//! temp main checkout, `TRUSTY_MPM_PM_UNRESTRICTED` unset, and an unreachable
//! daemon. A budget-eligible direct edit (`sed -i`) is denied on its fourth
//! call for a PM and never for a supervisor; every absolute guard denies both.
//! Every fixture is fake: the guard only classifies the commands.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard_supervisor_8453`.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

/// A direct in-place edit: allowed three times per turn for a PM, then denied.
const SED_EDIT: &str = "sed -i s/a/b/ src/lib.rs";

/// The session's launch-time inputs the hook reads.
struct Session {
    /// The main checkout the session was launched in (`CLAUDE_PROJECT_DIR`).
    project: PathBuf,
    /// The scratch `$HOME`.
    home: tempfile::TempDir,
    /// `TRUSTY_MPM_SESSION_PROFILE`, when stamped.
    stamp: Option<&'static str>,
    _dir: tempfile::TempDir,
}

/// A main checkout whose `.trusty-mpm.toml` holds `profile`, stamped `stamp`,
/// and allow-listed in the user config when `allow_listed`.
fn session(profile: Option<&str>, stamp: Option<&'static str>, allow_listed: bool) -> Session {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("fleet");
    std::fs::create_dir_all(project.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(project.join("src")).expect("mkdir src");
    std::fs::write(project.join("src/lib.rs"), "// dirty\n").expect("write src");
    if let Some(profile) = profile {
        std::fs::write(
            project.join(".trusty-mpm.toml"),
            format!("profile = \"{profile}\"\n"),
        )
        .expect("write project config");
    }
    let home = tempfile::tempdir().expect("home");
    if allow_listed {
        let root = home.path().join(".trusty-mpm");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                project.display().to_string()
            ),
        )
        .expect("write user config");
    }
    Session {
        project,
        home,
        stamp,
        _dir: dir,
    }
}

/// The fully granted supervisor: stamp, allowlist and file all agree.
fn supervisor() -> Session {
    session(Some("supervisor"), Some("supervisor"), true)
}

/// Run the guard on `tool` with `input` for `s`, returning stdout.
fn run_guard(s: &Session, tool: &str, input: serde_json::Value) -> String {
    common::write_disk_threshold(s.home.path(), 100);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": s.project.display().to_string(),
        "tool_name": tool,
        "tool_input": input,
    })
    .to_string();
    let mut cmd = common::tm_command_in(s.home.path());
    cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(&s.project)
        .env("CLAUDE_PROJECT_DIR", &s.project)
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT");
    if let Some(stamp) = s.stamp {
        cmd.env("TRUSTY_MPM_SESSION_PROFILE", stamp);
    }
    let mut child = cmd
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

/// The verdict on the fourth consecutive `sed -i` in `s`.
fn fourth_direct_edit(s: &Session) -> String {
    let bash = || serde_json::json!({ "command": SED_EDIT });
    for _ in 1..=3 {
        assert_eq!(run_guard(s, "Bash", bash()).trim(), "");
    }
    run_guard(s, "Bash", bash())
}

#[test]
fn a_supervisor_session_is_not_bound_by_the_pm_delegation_rules() {
    assert_eq!(fourth_direct_edit(&supervisor()).trim(), "");
}

#[test]
fn a_pm_session_is_still_bound_by_the_pm_delegation_rules() {
    let pm = session(Some("pm"), Some("pm"), true);
    assert!(fourth_direct_edit(&pm).contains("\"deny\""));
}

#[test]
fn a_project_only_switch_keeps_the_pm_delegation_rules() {
    // #3981: the project file is writable by the session and travels in git,
    // so on its own it grants nothing — no stamp, no allowlist entry.
    let project_only = session(Some("supervisor"), None, false);
    assert!(fourth_direct_edit(&project_only).contains("\"deny\""));
    // The stamp and the file without the operator's allowlist entry.
    let unlisted = session(Some("supervisor"), Some("supervisor"), false);
    assert!(fourth_direct_edit(&unlisted).contains("\"deny\""));
    // The allowlist and the file without the launch stamp.
    let unstamped = session(Some("supervisor"), None, true);
    assert!(fourth_direct_edit(&unstamped).contains("\"deny\""));
}

#[test]
fn a_stamp_without_the_file_keeps_the_pm_delegation_rules() {
    // The launch said supervisor; the project file no longer does.
    for profile in [Some("pm"), None] {
        let stale = session(profile, Some("supervisor"), true);
        assert!(
            fourth_direct_edit(&stale).contains("\"deny\""),
            "file profile {profile:?}"
        );
    }
}

#[test]
fn an_undecidable_profile_keeps_the_pm_guards() {
    // Fail-closed to the PM profile: a malformed config is not a supervisor.
    let broken = supervisor();
    std::fs::write(
        broken.project.join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n[[[ not toml\n",
    )
    .expect("write broken config");
    assert!(fourth_direct_edit(&broken).contains("\"deny\""));
}

#[test]
fn every_absolute_guard_still_binds_a_supervisor() {
    // One row per absolute guard that binds a top-level caller. Each is asked
    // BEFORE the profile verdict; moving the verdict above them lets a granted
    // supervisor through, and every row fails.
    let s = supervisor();
    let lib = s.project.join("src/lib.rs").display().to_string();
    let bash = |command: &str| serde_json::json!({ "command": command });
    let rows: [(&str, &str, serde_json::Value, &str); 6] = [
        (
            "secret read (#7266/#8596)",
            "Bash",
            bash("sed -n 1p terraform.tfvars"),
            "7266",
        ),
        (
            "main-checkout write (ADR-0044)",
            "Write",
            serde_json::json!({ "file_path": lib, "content": "x" }),
            "ADR-0044",
        ),
        (
            "destructive git (ADR-0037)",
            "Bash",
            bash("git reset --hard"),
            "ADR-0037",
        ),
        (
            "worktree add outside the project",
            "Bash",
            bash("git worktree add /tmp/x"),
            "worktree",
        ),
        (
            "secret copy into a worktree (#7122)",
            "Bash",
            bash("cp .env .claude/worktrees/agent-x/.env"),
            "7122",
        ),
        ("destructive delete", "Bash", bash("rm -rf /"), "rm -rf"),
    ];
    // Every row runs before the assertion, so a failure names all of them.
    let failed: Vec<String> = rows
        .into_iter()
        .filter_map(|(label, tool, input, marker)| {
            let stdout = run_guard(&s, tool, input);
            let denied = stdout.contains("\"deny\"") && stdout.contains(marker);
            (!denied).then(|| format!("{label} (expected a deny citing `{marker}`): {stdout:?}"))
        })
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
}
