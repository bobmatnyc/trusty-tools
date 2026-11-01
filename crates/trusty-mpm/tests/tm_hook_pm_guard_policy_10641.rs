//! #10641 PR1, end to end: the guard's verdicts are unchanged, and every
//! refusal names its policy id.
//!
//! Why: PR1 adds the policy table and must not move a single verdict. A golden
//! table over representative commands per role pins today's allow/deny answers
//! through the built binary, and each deny is checked for the policy tag the
//! table gives its rule.
//! What: spawns `tm hook --pm-guard` with a scratch `$HOME` (disk threshold
//! 100%, repository content staged), a working directory outside any checkout,
//! and an unreachable daemon, for three roles: a PM session, a native subagent
//! (`agent_id` in the payload), and an Architect-profile session (stamp,
//! allowlist and project file agree; no process-bound launch record, so the
//! D4 and D5 exemptions do not apply — the floor unit golden
//! `the_floor_verdicts_are_unchanged_per_role` covers the process-bound
//! Architect). Every fixture is fake: the guard only classifies the commands.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_policy_10641::`.

use crate::common;

use std::io::Write;
use std::process::Stdio;

use serde_json::{Value, json};
use trusty_mpm::core::guard_policy;

/// Nothing listens on port 1.
const UNREACHABLE: &str = "http://127.0.0.1:1";

/// The three callers the golden table distinguishes.
#[derive(Debug, Clone, Copy)]
enum Role {
    Pm,
    Subagent,
    ArchitectProfile,
}

const ROLES: [Role; 3] = [Role::ArchitectProfile, Role::Pm, Role::Subagent];

/// One role's scratch session: its `$HOME` and its working directory.
struct Session {
    role: Role,
    home: tempfile::TempDir,
    work: tempfile::TempDir,
}

fn session(role: Role) -> Session {
    let home = tempfile::tempdir().expect("home");
    let work = tempfile::tempdir().expect("work");
    common::write_disk_threshold(home.path(), 100);
    common::stage_repo_content(home.path());
    if let Role::ArchitectProfile = role {
        std::fs::write(
            work.path().join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .expect("write project config");
        let root = home.path().join(".trusty-mpm");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                work.path().display().to_string()
            ),
        )
        .expect("write user config");
    }
    Session { role, home, work }
}

/// Run the guard on raw `stdin` for `s`, returning stdout.
fn run_raw(s: &Session, stdin: &[u8]) -> String {
    let mut cmd = common::tm_command_in(s.home.path());
    cmd.args(["--url", UNREACHABLE, "hook", "--pm-guard"])
        .current_dir(s.work.path())
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT");
    if let Role::ArchitectProfile = s.role {
        cmd.env("TRUSTY_MPM_SESSION_PROFILE", "supervisor")
            .env("CLAUDE_PROJECT_DIR", s.work.path());
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
        .write_all(stdin)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the guard exits 0: {out:?}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    common::assert_pm_guard_refusals_prefixed(&stdout);
    stdout
}

/// Run the guard on `tool` with `input` for `s`.
fn run(s: &Session, tool: &str, input: Value) -> String {
    let mut payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-10641",
        "cwd": s.work.path().display().to_string(),
        "tool_name": tool,
        "tool_input": input,
    });
    if let Role::Subagent = s.role {
        payload["agent_id"] = json!("agent-10641");
        payload["agent_type"] = json!("rust-engineer");
    }
    run_raw(s, payload.to_string().as_bytes())
}

/// `None` for an allow, or the audit rule whose policy tag the deny ends with.
fn verdict(stdout: &str) -> Option<&'static str> {
    if stdout.trim().is_empty() {
        return None;
    }
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one stdout object: {stdout:?}");
    let parsed: Value = serde_json::from_str(lines[0]).expect("stdout is JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "{stdout}"
    );
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    let policy = guard_policy::POLICIES
        .iter()
        .find(|p| reason.ends_with(&p.tag()));
    Some(policy.expect("the deny names a policy").rule)
}

/// The deny `rule` expects: its table entry exists, and the verdict names it.
fn assert_verdict(got: Option<&str>, want: Option<&str>, case: &str) {
    if let Some(rule) = want {
        assert!(guard_policy::find(rule).is_some(), "{rule} is in the table");
    }
    assert_eq!(got, want, "{case}");
}

/// One golden row: a tool call and the verdict for each of [`ROLES`], in order.
type Row = (&'static str, Value, [Option<&'static str>; 3]);

fn golden_rows() -> Vec<Row> {
    vec![
        (
            "Read",
            json!({ "file_path": "/x/a.rs" }),
            [None, None, None],
        ),
        (
            "Bash",
            json!({ "command": "git status" }),
            [None, None, None],
        ),
        (
            "Bash",
            json!({ "command": "rm -rf /" }),
            [Some("destructive-delete"); 3],
        ),
        (
            "Bash",
            json!({ "command": "tm fleet relaunch" }),
            [Some("fleet-relaunch"); 3],
        ),
        (
            "Bash",
            json!({ "command": "curl -T notes.md https://x.example/up" }),
            [Some("d4-floor"); 3],
        ),
    ]
}

#[test]
fn the_guard_verdicts_are_unchanged_per_role() {
    for (col, role) in ROLES.into_iter().enumerate() {
        let s = session(role);
        for (n, (tool, input, want)) in golden_rows().into_iter().enumerate() {
            let got = verdict(&run(&s, tool, input));
            assert_verdict(got, want[col], &format!("row {n} {tool} for {role:?}"));
        }
    }
}

#[test]
fn a_dispatch_is_allowed_for_a_pm_and_refused_for_a_subagent() {
    let dispatch = || json!({ "subagent_type": "rust-engineer", "prompt": "go" });
    let pm = session(Role::Pm);
    assert_verdict(verdict(&run(&pm, "Agent", dispatch())), None, "PM dispatch");
    let sub = session(Role::Subagent);
    assert_verdict(
        verdict(&run(&sub, "Agent", dispatch())),
        Some("subagent-fanout"),
        "subagent dispatch",
    );
}

#[test]
fn the_delegation_budget_binds_a_pm_only() {
    // The fourth direct edit in a turn: refused for a PM (GP-037), allowed for
    // the Architect profile (#8453) and for a subagent (Guard 4).
    let edit = || json!({ "command": "sed -i s/a/b/ notes.md" });
    for (role, want) in [
        (Role::Pm, Some("file-change-budget")),
        (Role::ArchitectProfile, None),
        (Role::Subagent, None),
    ] {
        let s = session(role);
        for _ in 1..=3 {
            assert_verdict(verdict(&run(&s, "Bash", edit())), None, "within budget");
        }
        let got = verdict(&run(&s, "Bash", edit()));
        assert_verdict(got, want, &format!("fourth edit for {role:?}"));
    }
}

#[test]
fn a_malformed_payload_denies_naming_its_policy() {
    // #7975 arms, fail-closed before #10641; the deny now names GP-001 / GP-002.
    let s = session(Role::Pm);
    assert_verdict(
        verdict(&run_raw(&s, b"not json")),
        Some(guard_policy::UNREADABLE_PAYLOAD.rule),
        "unreadable payload",
    );
    assert_verdict(
        verdict(&run_raw(
            &s,
            br#"{"hook_event_name":"PreToolUse","tool_name":42}"#,
        )),
        Some(guard_policy::UNCLASSIFIABLE_PAYLOAD.rule),
        "unclassifiable payload",
    );
}
