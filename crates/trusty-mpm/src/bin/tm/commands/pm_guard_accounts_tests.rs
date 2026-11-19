//! Tests for the ops-dispatch account block (#11390).

use super::*;
use crate::commands::pm_guard_worktree_grant::{WorktreeGrant, evaluate_worktree_grant};
use trusty_mpm::core::project_accounts::ACCOUNT_RULE;

const ACCOUNTS_TOML: &str = "[accounts.gh]\naccount = \"octo-user\"\nverify = \"gh auth status\"\n\n\
     [accounts.neon]\naccount = \"neo.user\"\nscope = \"Some-Org\"\nverify = \"neonctl me\"\n";

fn r() -> &'static AgentRoster {
    crate::commands::install::test_roster_ref()
}

fn project(toml: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".trusty-mpm.toml"), toml).expect("write config");
    dir
}

fn dispatch(agent: &str) -> Value {
    serde_json::json!({"subagent_type": agent, "prompt": "deploy it"})
}

fn prompt(v: &Value) -> &str {
    v["prompt"].as_str().expect("prompt")
}

/// Removing the role check (or the call) leaves `None` and fails this.
#[test]
fn ops_dispatch_gets_the_account_block() {
    let dir = project(ACCOUNTS_TOML);
    let out = account_rewrite(Some(r()), "Agent", Some(&dispatch("local-ops")), dir.path())
        .expect("an ops dispatch is rewritten");
    let p = prompt(&out);
    assert!(p.starts_with("deploy it"), "{p}");
    assert!(p.contains(ACCOUNT_BLOCK_HEADING), "{p}");
    assert!(
        p.contains("- `gh`: account `octo-user`; verify: `gh auth status`"),
        "{p}"
    );
    assert!(
        p.contains("account `neo.user`, scope `Some-Org`; verify: `neonctl me`"),
        "{p}"
    );
    assert!(p.ends_with(ACCOUNT_RULE), "{p}");
    assert_eq!(out["subagent_type"], "local-ops");
}

#[test]
fn non_ops_dispatch_is_unchanged() {
    let dir = project(ACCOUNTS_TOML);
    for agent in ["research", "rust-engineer", "not-an-agent"] {
        assert_eq!(
            account_rewrite(Some(r()), "Agent", Some(&dispatch(agent)), dir.path()),
            None,
            "{agent}"
        );
    }
}

#[test]
fn task_dispatch_is_unchanged() {
    let dir = project(ACCOUNTS_TOML);
    let input = dispatch("local-ops");
    assert_eq!(
        account_rewrite(Some(r()), "Task", Some(&input), dir.path()),
        None
    );
}

#[test]
fn no_roster_means_no_block() {
    let dir = project(ACCOUNTS_TOML);
    let input = dispatch("local-ops");
    assert_eq!(
        account_rewrite(None, "Agent", Some(&input), dir.path()),
        None
    );
}

#[test]
fn block_is_appended_once() {
    let dir = project(ACCOUNTS_TOML);
    let once = account_rewrite(Some(r()), "Agent", Some(&dispatch("local-ops")), dir.path())
        .expect("first pass rewrites");
    assert_eq!(
        account_rewrite(Some(r()), "Agent", Some(&once), dir.path()),
        None
    );
    let composed = compose(Some(r()), "Agent", once.clone(), dir.path());
    assert_eq!(prompt(&composed).matches(ACCOUNT_BLOCK_HEADING).count(), 1);
}

#[test]
fn missing_map_still_carries_the_rule() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = account_rewrite(
        Some(r()),
        "Agent",
        Some(&dispatch("vercel-ops")),
        dir.path(),
    )
    .expect("rewritten with no config");
    assert!(prompt(&out).contains("No account map is configured"));
    assert!(prompt(&out).ends_with(ACCOUNT_RULE));
}

/// A secret-shaped value fails the whole file; the token never reaches the prompt.
#[test]
fn secret_shaped_value_is_never_injected() {
    let token = format!("ghp_{}", "a".repeat(36));
    let dir = project(&format!("[accounts.gh]\naccount = \"{token}\"\n"));
    let out = account_rewrite(Some(r()), "Agent", Some(&dispatch("local-ops")), dir.path())
        .expect("rewritten");
    assert!(!prompt(&out).contains(&token));
    assert!(
        prompt(&out).contains(
            "The project account map was rejected (accounts.gh.account looks like a credential"
        ),
        "{}",
        prompt(&out)
    );
    assert!(prompt(&out).contains("ask the PM before any account-scoped action"));
    assert!(!prompt(&out).contains("No account map is configured"));
}

/// The #5814 in-place rewrite and the account block print as ONE input.
#[test]
fn block_composes_with_the_in_place_note() {
    let dir = project(&format!("agent_worktree = false\n{ACCOUNTS_TOML}"));
    std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
    let grant = evaluate_worktree_grant(r(), "Agent", Some(&dispatch("rust-engineer")), dir.path());
    let Some(WorktreeGrant::InPlace(updated)) = grant else {
        panic!("expected an in-place rewrite, got {grant:?}");
    };
    let mut ops = updated.clone();
    ops["subagent_type"] = Value::String("local-ops".into());
    let composed = compose(Some(r()), "Agent", ops, dir.path());
    let p = prompt(&composed);
    assert!(p.contains("Worktree isolation is OFF"), "{p}");
    assert!(p.contains(ACCOUNT_BLOCK_HEADING), "{p}");
    assert!(composed.get("isolation").is_none());
    // A non-ops rewrite passes through untouched.
    assert_eq!(
        compose(Some(r()), "Agent", updated.clone(), dir.path()),
        updated
    );
}

/// #7684: a deployed agent starts with `omitClaudeMd: true`, so the project's
/// build and test rules must ride on the dispatch prompt instead — for a
/// non-ops agent too, and alongside the account block for an ops one.
#[test]
fn any_agent_dispatch_gets_the_project_rules() {
    use trusty_mpm::core::project_rules::{PROJECT_RULES_HEADING, RULES_CLOSE, RULES_OPEN};
    let dir = project(ACCOUNTS_TOML);
    let md =
        format!("private\n{RULES_OPEN}\nrun `cargo test -p x --no-fail-fast`\n{RULES_CLOSE}\n");
    std::fs::write(dir.path().join("CLAUDE.md"), md).expect("write CLAUDE.md");

    let out = account_rewrite(
        Some(r()),
        "Agent",
        Some(&dispatch("rust-engineer")),
        dir.path(),
    )
    .expect("a non-ops dispatch carries the rules");
    let p = prompt(&out);
    assert!(p.starts_with("deploy it"), "{p}");
    assert!(p.contains(PROJECT_RULES_HEADING), "{p}");
    assert!(p.ends_with("run `cargo test -p x --no-fail-fast`"), "{p}");
    assert!(!p.contains("private"), "{p}");
    assert!(!p.contains(ACCOUNT_BLOCK_HEADING), "{p}");

    let ops = account_rewrite(Some(r()), "Agent", Some(&dispatch("local-ops")), dir.path())
        .expect("an ops dispatch carries both");
    assert!(prompt(&ops).contains(ACCOUNT_BLOCK_HEADING));
    assert!(prompt(&ops).contains(PROJECT_RULES_HEADING));
    // No roster still carries the rules; so does `Task`, without accounts.
    assert!(account_rewrite(None, "Agent", Some(&dispatch("qa")), dir.path()).is_some());
    let task = account_rewrite(Some(r()), "Task", Some(&dispatch("local-ops")), dir.path())
        .expect("a Task dispatch carries the rules");
    assert!(prompt(&task).contains(PROJECT_RULES_HEADING));
    assert!(!prompt(&task).contains(ACCOUNT_BLOCK_HEADING));
}

/// #7684: a project with no `CLAUDE.md` leaves a non-ops dispatch untouched.
#[test]
fn no_claude_md_leaves_a_dispatch_unchanged() {
    let dir = project("");
    for tool in ["Agent", "Task"] {
        assert_eq!(
            account_rewrite(Some(r()), tool, Some(&dispatch("research")), dir.path()),
            None,
            "{tool}"
        );
    }
    let payload = serde_json::json!({
        "tool_name": "Agent", "cwd": dir.path(), "tool_input": dispatch("research"),
    });
    assert_eq!(rules_carrier_response(&payload, dir.path()), None);
}

/// #7684: the carrier object holds the rewritten input and no decision.
#[test]
fn carrier_response_holds_only_the_updated_input() {
    use trusty_mpm::core::project_rules::PROJECT_RULES_HEADING;
    let dir = project("");
    std::fs::write(dir.path().join("CLAUDE.md"), "rule").expect("write CLAUDE.md");
    let payload = serde_json::json!({
        "tool_name": "Agent", "cwd": dir.path(), "tool_input": dispatch("research"),
    });
    let out = rules_carrier_response(&payload, Path::new("/")).expect("carried");
    let hso = &out["hookSpecificOutput"];
    assert!(hso.get("permissionDecision").is_none(), "{out}");
    assert!(
        prompt(&hso["updatedInput"]).contains(PROJECT_RULES_HEADING),
        "{out}"
    );
    // The egress share: silent while pm-guard is on, the carrier when off.
    assert_eq!(egress_rules_carrier(Some(&payload), || true), None);
    assert_eq!(egress_rules_carrier(Some(&payload), || false), Some(out));
    let read = serde_json::json!({"tool_name": "Read", "cwd": dir.path()});
    assert_eq!(
        egress_rules_carrier(Some(&read), || panic!("config read for a non-dispatch")),
        None
    );
}

#[test]
fn project_rules_are_appended_once() {
    use trusty_mpm::core::project_rules::PROJECT_RULES_HEADING;
    let dir = project("");
    std::fs::write(dir.path().join("CLAUDE.md"), "rule").expect("write CLAUDE.md");
    let once = account_rewrite(Some(r()), "Agent", Some(&dispatch("local-ops")), dir.path())
        .expect("first pass rewrites");
    assert_eq!(
        account_rewrite(Some(r()), "Agent", Some(&once), dir.path()),
        None
    );
    let composed = compose(Some(r()), "Agent", once, dir.path());
    assert_eq!(prompt(&composed).matches(PROJECT_RULES_HEADING).count(), 1);
    assert_eq!(prompt(&composed).matches(ACCOUNT_BLOCK_HEADING).count(), 1);
}
