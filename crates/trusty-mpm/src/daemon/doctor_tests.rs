//! Unit tests for `daemon::doctor` — split out of `doctor.rs` (test-file
//! budget: 1500 SLOC; the inline module pushed the production file over its
//! 500-SLOC cap once issue #2940 added the `hooks_contamination` /
//! `hooks_foreign_conflict` probes).
//! What: exercises `index_present`, the memory/search reachability probes,
//! the managed-workspace-scoped `agents` probe, the full `run_doctor` check
//! roster, the `gh_account` / `oauth_token` advisory checks, and the
//! worktree-orphan scan.
//! Test: this module IS the test suite for `super`.

use super::*;

#[test]
fn index_present_matches_each_shape() {
    // Bare string array.
    let strings = serde_json::json!(["other", "trusty-mpm"]);
    assert!(index_present(&strings, "trusty-mpm"));
    // Objects with an `id` field, nested under `indexes`.
    let objects = serde_json::json!({"indexes": [{"id": "trusty-mpm"}]});
    assert!(index_present(&objects, "trusty-mpm"));
    // Objects with a `name` field.
    let named = serde_json::json!([{"name": "trusty-mpm"}]);
    assert!(index_present(&named, "trusty-mpm"));
    // Absent index.
    let missing = serde_json::json!(["a", "b"]);
    assert!(!index_present(&missing, "trusty-mpm"));
}

#[tokio::test]
async fn memory_unreachable_is_fail() {
    // Port 0 never accepts a connection, so the probe must fail cleanly
    // rather than hang.
    unsafe {
        std::env::set_var("TRUSTY_MEMORY_ADDR", "127.0.0.1:0");
    }
    let tmp = tempfile::tempdir().unwrap();
    let check = check_memory(tmp.path()).await;
    unsafe {
        std::env::remove_var("TRUSTY_MEMORY_ADDR");
    }
    assert_eq!(check.status, CheckStatus::Fail);
}

#[tokio::test]
async fn search_unreachable_is_fail() {
    unsafe {
        std::env::set_var("TRUSTY_SEARCH_ADDR", "127.0.0.1:0");
    }
    let tmp = tempfile::tempdir().unwrap();
    let check = check_search(tmp.path()).await;
    unsafe {
        std::env::remove_var("TRUSTY_SEARCH_ADDR");
    }
    assert_eq!(check.status, CheckStatus::Fail);
}

#[tokio::test]
async fn agents_check_scopes_to_managed_workspace_when_project_given() {
    // Issue #2149: a managed session deploys its roster under
    // `<workspace>/.claude/agents/` (issue #1931), NOT the operator's
    // `$HOME/.claude/agents/`. Before this fix `run_doctor` always probed
    // `FrameworkPaths::default()` (the home tier) even when a `project_dir`
    // was supplied, so a managed workspace with a completely empty/broken
    // roster could report a false `agents` `Ok` purely because the
    // OPERATOR's own `$HOME/.claude/agents/` happened to be populated —
    // silently missing the exact provisioning gap this issue is about.
    // A fresh, empty `project_dir` with NO `.claude/agents/` at all must
    // now `Fail`, regardless of whatever the real test-runner's home
    // directory holds.
    let project = tempfile::tempdir().unwrap();
    let report = run_doctor(Some(project.path()), None, &[]).await;
    let agents_check = report
        .checks
        .iter()
        .find(|c| c.name == "agents")
        .expect("agents check present");
    assert_eq!(agents_check.status, CheckStatus::Fail);
    // The message must name the WORKSPACE-scoped path, not the operator's
    // home directory, proving the probe was scoped to `project_dir`.
    let expected_dir = project.path().join(".claude").join("agents");
    assert!(
        agents_check
            .message
            .contains(&expected_dir.display().to_string()),
        "message must name the workspace-scoped agents dir: {}",
        agents_check.message
    );
}

#[tokio::test]
async fn agents_check_ok_when_managed_workspace_roster_populated() {
    // The positive counterpart: a project-scoped roster (with its manifest)
    // must report `Ok`, proving the probe reads the WORKSPACE tier rather
    // than always falling through to the home tier.
    let project = tempfile::tempdir().unwrap();
    let agents_dir = project.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("engineer.md"), "agent").unwrap();
    std::fs::write(
        agents_dir.join(crate::core::agent_manifest::MANIFEST_FILE),
        "{}",
    )
    .unwrap();

    let report = run_doctor(Some(project.path()), None, &[]).await;
    let agents_check = report
        .checks
        .iter()
        .find(|c| c.name == "agents")
        .expect("agents check present");
    assert_eq!(agents_check.status, CheckStatus::Ok);
}

#[tokio::test]
async fn run_doctor_produces_nineteen_checks() {
    // Issue #2158 added the `deployment` probe (nine → ten); issue #2246
    // adds `oauth_token` (ten → eleven); issue #2876 adds `skill_staleness`
    // and `legacy_sources` (eleven → thirteen); DOC-42 / issue #2889 adds
    // `agent_skills` (thirteen → fourteen); issue #2906 review splits that
    // into `agent_skills` + `agent_skills_prose_hints` (fourteen → fifteen);
    // issue #2940 adds `hooks_contamination` + `hooks_foreign_conflict`
    // (fifteen → seventeen); issue #2333 adds `output_style_staleness`
    // (seventeen → eighteen); issue #2997 adds `tcc_taint` (eighteen →
    // nineteen).
    // #1905's stale-skill cleanup is deliberately NOT a `run_doctor` probe
    // — see the `run_doctor` doc.
    let report = run_doctor(None, None, &[]).await;
    assert_eq!(report.checks.len(), 19);
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "instructions",
            "agents",
            "skills",
            "skill_source",
            "output_style",
            "output_style_staleness",
            "deployment",
            "skill_staleness",
            "legacy_sources",
            "agent_skills",
            "agent_skills_prose_hints",
            "memory",
            "search",
            "worktrees",
            "gh_account",
            "oauth_token",
            "hooks_contamination",
            "hooks_foreign_conflict",
            "tcc_taint"
        ]
    );
}

#[test]
fn build_gh_account_check_single_ok() {
    // A single clear active account is healthy (Ok), naming the login.
    let check =
        build_gh_account_check(Some("bobmatnyc".to_string()), vec!["bobmatnyc".to_string()]);
    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.name, "gh_account");
    assert!(check.message.contains("bobmatnyc"));
}

#[test]
fn build_gh_account_check_multi_warn() {
    // Multiple logged-in accounts is the ambiguity that hid the admin-merge
    // bug: Warn, name both accounts, and point at `gh auth switch`.
    let check = build_gh_account_check(
        Some("bob-duetto".to_string()),
        vec!["bob-duetto".to_string(), "bobmatnyc".to_string()],
    );
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("bob-duetto"));
    assert!(check.message.contains("bobmatnyc"));
    assert!(check.message.contains("gh auth switch"));
}

#[test]
fn build_gh_account_check_unauthenticated_warn() {
    // No account at all: Warn (advisory), not Fail, pointing at `gh auth login`.
    let check = build_gh_account_check(None, Vec::new());
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("not authenticated"));
    assert!(check.message.contains("gh auth login"));
}

#[test]
fn gh_account_check_is_advisory_only() {
    // Every branch must be advisory — never a hard Fail.
    for check in [
        build_gh_account_check(Some("a".to_string()), vec!["a".to_string()]),
        build_gh_account_check(
            Some("a".to_string()),
            vec!["a".to_string(), "b".to_string()],
        ),
        build_gh_account_check(None, Vec::new()),
        build_gh_account_check(None, vec!["a".to_string()]),
    ] {
        assert_ne!(
            check.status,
            CheckStatus::Fail,
            "gh_account must never Fail"
        );
    }
}

#[test]
fn oauth_token_check_warns_when_relocated_with_no_token_or_key() {
    // Issue #2246: the exact at-risk configuration — CLAUDE_CONFIG_DIR
    // relocated (as every managed spawn does), no stored token, no
    // resolvable OAuth env var — must Warn with an actionable hint.
    let check = build_oauth_token_check(true, false, false);
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("tm auth set-token"));
    assert!(check.message.contains("claude setup-token"));
}

#[test]
fn oauth_token_check_ok_when_token_stored() {
    // A stored token file resolves for a managed spawn → satisfied.
    let check = build_oauth_token_check(true, true, false);
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn oauth_token_check_ok_when_env_var_set() {
    // A CLAUDE_CODE_OAUTH_TOKEN env var is the higher-precedence source
    // resolve_oauth_token consults → also satisfies the managed-session
    // auth requirement even without a stored token file.
    let check = build_oauth_token_check(true, false, true);
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn oauth_token_check_warns_when_only_api_key_set() {
    // #2246 false-negative fix: ambient ANTHROPIC_API_KEY does NOT satisfy
    // the managed-session auth requirement — every managed spawn strips it
    // via `env -u ANTHROPIC_API_KEY`, so relying on it still triggers the
    // login loop. With only the API key set (no token file, no OAuth env
    // var), the check MUST still Warn. `build_oauth_token_check` no longer
    // even takes the API-key flag as an input; this test pins the behaviour
    // by driving the same relocated/no-token state the api-key-only
    // environment produces.
    let check = build_oauth_token_check(true, false, false);
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "an ambient ANTHROPIC_API_KEY must NOT count as managed-session auth"
    );
}

#[test]
fn oauth_token_check_ok_when_not_relocated() {
    // Home unresolved → no managed config dir → the #2246 divergence
    // cannot occur (there is nothing to relocate).
    let check = build_oauth_token_check(false, false, false);
    assert_eq!(check.status, CheckStatus::Ok);
}

#[tokio::test]
async fn worktrees_no_repos_root_is_ok() {
    // Fix 7 (#1840): absence of a managed workspace root is NORMAL — not every
    // operator uses in-project worktree sessions. Return Ok, not Warn.
    let check = check_worktrees(None, &[]).await;
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("no managed workspace"));
}

#[tokio::test]
async fn worktrees_no_orphans_is_ok() {
    // Why (#1845 item 2): on macOS /tmp is a symlink to /private/tmp. If we pass the
    // raw tempfile path as active (e.g. /tmp/…) but the walk canonicalises it to
    // /private/tmp/…, the active-set lookup misses and a live worktree is falsely
    // flagged as an orphan — causing a spurious CI failure. Canonicalize the active
    // path in the test so the set membership check in find_orphaned_worktrees matches.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Create owner/repo/.worktrees/session-abc/ and register it as active.
    let wt = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("session-abc");
    std::fs::create_dir_all(&wt).unwrap();
    // Canonicalize so /tmp vs /private/tmp symlink differences are resolved.
    let canonical_wt = std::fs::canonicalize(&wt).unwrap_or(wt);
    let active = vec![canonical_wt];
    let check = check_worktrees(Some(root), &active).await;
    assert_eq!(check.status, CheckStatus::Ok);
}

#[tokio::test]
async fn worktrees_with_orphan_is_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Create two worktrees; only one has an active session.
    let wt1 = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("session-live");
    let wt2 = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("session-dead");
    std::fs::create_dir_all(&wt1).unwrap();
    std::fs::create_dir_all(&wt2).unwrap();
    // wt2 is orphaned (no active session).
    let active = vec![wt1];
    let check = check_worktrees(Some(root), &active).await;
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("1 orphaned"));
    assert!(check.message.contains("tm session prune --worktrees"));
}
