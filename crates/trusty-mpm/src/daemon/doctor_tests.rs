//! Unit tests for `daemon::doctor` — split out of `doctor.rs` (test-file
//! budget: 1500 SLOC; the inline module pushed the production file over its
//! 500-SLOC cap once issue #2940 added the `hooks_contamination` /
//! `hooks_foreign_conflict` probes).
//! What: exercises `index_present`, the memory/search reachability probes,
//! the managed-workspace-scoped `agents` probe, the full `run_doctor` check
//! roster, the `oauth_token` advisory check, and the worktree-orphan scan.
//! The `gh_account` check has its own suite in `doctor_gh_account_tests.rs`
//! (#5032).
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

#[test]
fn expected_search_index_id_derives_from_project_dir_not_hardcoded() {
    // #4003: `tm doctor`'s search check used to hardcode the literal expected
    // index id "trusty-mpm" (the crate name), which diverges from a repo's
    // actual registered index id — this repo registers as "trusty-tools" —
    // so a healthy, fully-indexed project permanently warned "index
    // missing". Pin that the id is now DERIVED from the project root's
    // basename (the same rule `trusty_common::derive_index_id` applies
    // everywhere else an index id is resolved), not a fixed constant: a
    // project literally named "trusty-mpm" must resolve to "trusty-mpm", and
    // one named anything else must resolve to ITS OWN name, never the old
    // hardcoded literal.
    let tmp = tempfile::tempdir().unwrap();

    let other_repo = tmp.path().join("trusty-tools");
    std::fs::create_dir_all(other_repo.join(".git")).unwrap();
    let other_id = expected_search_index_id(Some(&other_repo));
    assert_eq!(other_id, "trusty-tools");
    assert_ne!(
        other_id, "trusty-mpm",
        "must not fall back to the old hardcoded literal for an unrelated project"
    );

    let mpm_repo = tmp.path().join("trusty-mpm");
    std::fs::create_dir_all(mpm_repo.join(".git")).unwrap();
    let mpm_id = expected_search_index_id(Some(&mpm_repo));
    assert_eq!(mpm_id, "trusty-mpm");

    // Nested working directory inside the repo still resolves to the git
    // root's basename, not the nested dir's own name.
    let nested = other_repo.join("crates").join("trusty-mpm");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(expected_search_index_id(Some(&nested)), "trusty-tools");
}

#[tokio::test]
async fn search_unreachable_is_fail() {
    unsafe {
        std::env::set_var("TRUSTY_SEARCH_ADDR", "127.0.0.1:0");
    }
    let tmp = tempfile::tempdir().unwrap();
    let check = check_search(tmp.path(), None).await;
    unsafe {
        std::env::remove_var("TRUSTY_SEARCH_ADDR");
    }
    assert_eq!(check.status, CheckStatus::Fail);
}

#[tokio::test]
async fn agents_check_probes_the_managed_config_tier_not_the_workspace() {
    // Issue #4409 supersedes #2149's workspace scoping for AGENTS: bundled
    // agents no longer deploy per-workspace at all, so probing
    // `<workspace>/.claude/agents/` would now report every healthy install as
    // broken. The probe must name the tm-managed `CLAUDE_CONFIG_DIR` tier —
    // the ONE place a bundled agent can legitimately be — even when a
    // `project_dir` is supplied.
    //
    // The status itself is deliberately not asserted: the deploy tier is
    // machine-global, so a provisioned workstation and a bare CI runner
    // legitimately disagree. `doctor_fs_checks`'s `agents_*` tests cover every
    // status branch hermetically.
    let project = tempfile::tempdir().unwrap();
    let report = run_doctor(Some(project.path()), None, &[], None).await;
    let agents_check = report
        .checks
        .iter()
        .find(|c| c.name == "agents")
        .expect("agents check present");

    let workspace_tier = project.path().join(".claude").join("agents");
    assert!(
        !agents_check
            .message
            .contains(&workspace_tier.display().to_string()),
        "the agents probe must not read the workspace tier any more: {}",
        agents_check.message
    );

    // Asserted as a relative suffix, not an absolute path: the deploy tier is
    // home-relative, and sibling `#[serial]` tests move `$HOME` around, so
    // resolving it a second time here would race them.
    let expected_suffix = std::path::Path::new(".trusty-tools")
        .join(crate::core::trusty_tools_config::CRATE_NAME)
        .join(crate::core::trusty_tools_config::MANAGED_CLAUDE_CONFIG_SUBDIR)
        .join("agents");
    assert!(
        agents_check
            .message
            .contains(&expected_suffix.display().to_string()),
        "message must name the tm-managed agent deploy tier (…/{}): {}",
        expected_suffix.display(),
        agents_check.message
    );
}

#[tokio::test]
async fn unmanaged_cwd_audits_the_operator_home_tier() {
    // #5867: `tm doctor` sends the process cwd as `project`, so `run_doctor`
    // took the `for_managed_workspace` arm for any directory at all. That
    // pointed `claude_skills_dir()` at `<cwd>/.claude/skills` — the same path
    // the "project" tier candidate builds — and `skill_deploy_tiers`' dedup
    // dropped the duplicate, leaving `~/.claude/skills` unaudited. The `skills`
    // probe reads the same root, so it names the directory it looked at and is
    // the observable half of that resolution.
    //
    // Before the fix this asserted false: the message read
    // "<tmp>/.claude/skills does not exist".
    let project = tempfile::tempdir().unwrap();
    let report = run_doctor(Some(project.path()), None, &[], None).await;
    let skills = report
        .checks
        .iter()
        .find(|c| c.name == "skills")
        .expect("skills check present");

    let workspace_tier = project.path().join(".claude").join("skills");
    assert!(
        !skills
            .message
            .contains(&workspace_tier.display().to_string()),
        "an unregistered directory is not a managed workspace and must not be \
         probed as one: {}",
        skills.message
    );
}

#[tokio::test]
async fn a_registered_workspace_still_gets_the_workspace_layout() {
    // The other arm of #5867: a directory a live session was provisioned into
    // MUST keep the #2149/#1931 workspace scoping, or a managed workspace with
    // an empty roster goes back to reporting a false `Ok` off the operator's
    // own populated `$HOME/.claude`.
    let project = tempfile::tempdir().unwrap();
    let active = vec![project.path().to_path_buf()];
    let report = run_doctor(Some(project.path()), None, &active, None).await;
    let skills = report
        .checks
        .iter()
        .find(|c| c.name == "skills")
        .expect("skills check present");

    let workspace_tier = project.path().join(".claude").join("skills");
    assert!(
        skills
            .message
            .contains(&workspace_tier.display().to_string()),
        "a registered workspace must still be probed at its own tier: {}",
        skills.message
    );
}

#[test]
fn is_managed_workspace_matches_only_a_registered_path() {
    let registered = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let active = vec![registered.path().to_path_buf()];

    assert!(is_managed_workspace(registered.path(), &active));
    assert!(!is_managed_workspace(other.path(), &active));
    assert!(
        !is_managed_workspace(registered.path(), &[]),
        "with no live sessions nothing is a managed workspace"
    );
}

#[test]
fn is_managed_workspace_sees_through_a_symlinked_path() {
    // On macOS a workspace under `/tmp` is reached through a `/private/tmp`
    // symlink, so the daemon's recorded path and the cwd the CLI sends can be
    // two spellings of one directory. A raw `==` would miss the match and drop
    // a real managed workspace back to the home layout.
    let real = tempfile::tempdir().unwrap();
    let link_parent = tempfile::tempdir().unwrap();
    let link = link_parent.path().join("workspace-link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    #[cfg(not(unix))]
    return;

    let active = vec![real.path().to_path_buf()];
    assert!(
        is_managed_workspace(&link, &active),
        "a symlink to a registered workspace is that workspace"
    );
}

/// The canonicalize-failure fallback must not promote an unmanaged directory.
///
/// Why (#5867): `resolve` falls back to the raw path on ANY `canonicalize`
/// error, and that branch had no coverage — the two tests above only use paths
/// that exist. It is the branch the doc comment makes its correctness claim
/// about, and a false `true` here is #5867's original bug: an arbitrary cwd
/// audited as though a session had been provisioned into it.
/// What: drives all three failure kinds — an absent directory (`NotFound`), a
/// broken symlink (`NotFound` on the target), and a directory behind an
/// unreadable parent (`PermissionDenied`) — on both sides of the comparison,
/// and asserts every one answers `false`.
/// Test: this function IS the test.
#[test]
fn an_uncanonicalizable_path_is_not_a_managed_workspace() {
    let registered = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let active = vec![registered.path().to_path_buf()];

    let absent = scratch.path().join("never-created");
    assert!(
        !is_managed_workspace(&absent, &active),
        "a directory that is not on disk is not a provisioned workspace"
    );

    #[cfg(unix)]
    {
        let broken = scratch.path().join("dangling-link");
        std::os::unix::fs::symlink(scratch.path().join("no-such-target"), &broken).unwrap();
        assert!(
            !is_managed_workspace(&broken, &active),
            "a symlink to nothing resolves to nothing, not to a workspace"
        );
    }

    // The failure can sit on the RECORDED side too: a session's workspace_path
    // that has since been deleted must not start matching arbitrary cwds.
    let stale = vec![scratch.path().join("reaped-workspace")];
    assert!(
        !is_managed_workspace(registered.path(), &stale),
        "a stale recorded path must not match a live, unrelated directory"
    );
}

/// A `PermissionDenied` canonicalize is the same answer as an absent path.
///
/// Why (#5867): `resolve` collapses every `canonicalize` error into the raw
/// path, so the doc comment's "safe answer" claim has to hold for the
/// permissions kind, not just `NotFound`.
/// What: puts a real directory behind a `0o000` parent so `canonicalize` fails
/// with `PermissionDenied`, and asserts it still does not match a registered
/// workspace.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn an_unreadable_directory_is_not_a_managed_workspace() {
    use std::os::unix::fs::PermissionsExt;
    let registered = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let locked = scratch.path().join("locked");
    let inner = locked.join("workspace");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::canonicalize(&inner).is_ok() {
        eprintln!("skipping: cannot deny traversal on this platform/privilege level");
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        return;
    }

    let managed = is_managed_workspace(&inner, &[registered.path().to_path_buf()]);
    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));

    assert!(
        !managed,
        "a directory tm cannot even resolve is not one to deploy into"
    );
}

/// The fallback still matches a recorded path by its exact spelling.
///
/// Why (#5867): the fallback is only safe because a raw-path comparison can
/// match nothing but a path already in `active_workspace_paths`. Stating that
/// as prose in the doc comment is what left the branch untested; this pins the
/// boundary from the other side, so a future "just return false on any
/// canonicalize error" edit has to face the case it would change.
/// What: passes a path that exists on neither side but is spelled identically
/// to a recorded workspace, and asserts it matches — a recorded workspace,
/// under the name it was recorded with, is never the arbitrary cwd #5867 is
/// about.
/// Test: this function IS the test.
#[test]
fn an_absent_path_still_matches_the_recorded_spelling_of_itself() {
    let scratch = tempfile::tempdir().unwrap();
    let reaped = scratch.path().join("reaped-workspace");
    assert!(
        is_managed_workspace(&reaped, std::slice::from_ref(&reaped)),
        "an unresolvable path is compared verbatim, so it matches only its own \
         recorded spelling — never an unregistered directory"
    );
}

#[tokio::test]
async fn run_doctor_produces_thirty_two_checks() {
    // Issue #2158 added the `deployment` probe (nine → ten); issue #2246
    // adds `oauth_token` (ten → eleven); issue #2876 adds `skill_staleness`
    // and `legacy_sources` (eleven → thirteen); DOC-42 / issue #2889 adds
    // `agent_skills` (thirteen → fourteen); issue #2906 review splits that
    // into `agent_skills` + `agent_skills_prose_hints` (fourteen → fifteen);
    // issue #2940 adds `hooks_contamination` + `hooks_foreign_conflict`
    // (fifteen → seventeen); issue #2333 adds `output_style_staleness`
    // (seventeen → eighteen); issue #2997 adds `tcc_taint` (eighteen →
    // nineteen); issue #3453 part 2 adds `output_style_legacy_ids` (nineteen
    // → twenty); issue #3427 adds `scaffold_tracking` (twenty →
    // twenty-one); issue #2867 adds `push_guard` (twenty-one →
    // twenty-two); issue #4451 adds `agent_reachability` (twenty-two →
    // twenty-three); issue #4467 adds `transcript_saving` (twenty-three →
    // twenty-four); issue #4442 adds `asset_tier` (twenty-four →
    // twenty-five); issue #2919 adds `worktree_disk` (twenty-five →
    // twenty-six); issue #4605 adds `skill_unmanaged` (twenty-six →
    // twenty-seven); issue #4033 adds `binary_provenance` (twenty-seven →
    // twenty-eight); issue #5045 adds `search_index_pin` (twenty-nine →
    // thirty); the stray-`.mcp.json` probe adds `stray_mcp_json` (thirty →
    // thirty-one).
    // #1905's stale-skill cleanup is deliberately NOT a `run_doctor` probe
    // — see the `run_doctor` doc.
    let report = run_doctor(None, None, &[], None).await;
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    let expected = [
        "instructions",
        "agents",
        "agent_reachability",
        "asset_tier",
        "transcript_saving",
        "skills",
        "skill_source",
        "output_style",
        "output_style_staleness",
        "output_style_legacy_ids",
        "deployment",
        "skill_staleness",
        "skill_unmanaged",
        "legacy_sources",
        "legacy_overrides",
        "agent_skills",
        "agent_skills_prose_hints",
        "memory",
        "search",
        "search_index_pin",
        "worktrees",
        "worktree_disk",
        "gh_account",
        "oauth_token",
        "hooks_contamination",
        "hooks_foreign_conflict",
        "tcc_taint",
        "scaffold_tracking",
        "push_guard",
        "binary_provenance",
        // #5007: `sessions.json` integrity — a corrupt store blocks every write.
        "session_store",
        "stray_mcp_json",
    ];
    assert_eq!(names, expected);
    // Count derived from the list above, never a standalone literal:
    // adding a check is then a one-line edit here (#4090 review LOW-1).
    assert_eq!(report.checks.len(), expected.len());
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

/// Counts for `repos_root`, taken from the SHARED reconciled inventory (#5947).
///
/// Why: the whole point of the fix is that doctor no longer has its own orphan
/// rule, so every worktree test here must feed it the same classification
/// `prune-worktrees` and `reconcile-worktrees` read. Computing the counts any
/// other way in a test would re-introduce the third implementation the fix
/// removed, in the one place nobody would look for it.
fn counts_for(repos_root: &std::path::Path) -> WorktreeOrphanCounts {
    let report = crate::session_manager::worktree_reconcile::reconcile_worktrees(
        repos_root,
        &[],
        &[],
        chrono::Utc::now(),
    );
    WorktreeOrphanCounts::from_reconcile(&report)
}

#[tokio::test]
async fn worktrees_no_repos_root_is_ok() {
    // Fix 7 (#1840): absence of a managed workspace root is NORMAL — not every
    // operator uses in-project worktree sessions. Return Ok, not Warn.
    let check = check_worktrees(None, None).await;
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("no managed workspace"));
}

#[tokio::test]
async fn worktrees_no_orphans_is_ok() {
    // #4207: a REAL `git worktree add`, since discovery is derived from git's
    // registry — a `mkdir` is not a candidate and would make this pass for the
    // wrong reason. The fixture canonicalizes its own root, so the /tmp vs
    // /private/tmp symlink hazard (#1845 item 2) is handled there.
    let fx = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let _wt = fx.add_worktree("session-abc");
    let check = check_worktrees(Some(&fx.repos_root), Some(counts_for(&fx.repos_root))).await;
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[tokio::test]
async fn worktrees_unowned_worktree_is_not_an_orphan() {
    // #5947 (fault 2): THE false positive. Two real worktrees, neither carrying
    // an ownership sentinel. The old probe counted every git-registered
    // worktree no live session claimed, so it warned "1 orphaned" here — and on
    // the dogfood fleet, "198 orphaned" where `prune-worktrees` and
    // `reconcile-worktrees` both found zero. A worktree with no sentinel is
    // never auto-reclaimable, so it is not an orphan.
    let fx = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let _a = fx.add_worktree("session-live");
    let _b = fx.add_worktree("session-dead");
    let counts = counts_for(&fx.repos_root);
    assert_eq!(counts.orphaned, 0, "reconcile must report zero orphans");
    let check = check_worktrees(Some(&fx.repos_root), Some(counts)).await;
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(check.message.contains("no orphaned worktrees found"));
}

#[tokio::test]
async fn worktrees_with_orphan_is_warn() {
    // A GENUINE orphan: admitted by git, sentinel names a provably-gone owner,
    // tree clean — the same shape `clean_ownerless_admitted_worktree_is_orphaned`
    // pins on the reconcile side.
    let fx = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let _live = fx.add_worktree("session-live");
    let dead = fx.add_worktree("session-dead");
    crate::session_manager::worktree_git_fixture::GitWorktreeFixture::stamp_reclaimable_sentinel(
        &dead,
    );
    let check = check_worktrees(Some(&fx.repos_root), Some(counts_for(&fx.repos_root))).await;
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(check.message.contains("1 orphaned"));
    // #5947 (fault 1): the hint must name the verb that exists.
    assert!(check.message.contains(WORKTREE_REMEDIATION_COMMAND));
    assert!(
        !check.message.contains("prune --worktrees"),
        "the nonexistent flag must not come back: {}",
        check.message
    );
}

#[tokio::test]
async fn worktrees_without_a_reconciled_inventory_is_unknown() {
    // #5947: no inventory means no count. Reporting Ok would hide real orphans
    // and Warn would invent them, so the probe says it could not observe.
    let fx = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let check = check_worktrees(Some(&fx.repos_root), None).await;
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    assert!(check.message.contains("not established"));
}

// ---- Issues #4005 / #4001: doctor must observe, not infer ----

/// Spawn a throwaway HTTP listener that answers `GET /health` with `body`
/// after waiting `delay`.
///
/// Why: the existing probe tests bind port 0 (which only ever refuses), so
/// nothing in this suite could express "a daemon that IS serving". Both #4005
/// (a serving daemon reported unreachable) and #4001 (a wedged daemon reported
/// healthy) are about what doctor concludes from a REAL response, so the tests
/// need a real socket. Hand-rolled over `TcpListener` to avoid pulling an HTTP
/// server dependency into the test graph.
/// What: binds an ephemeral port, returns its `host:port`, and serves the
/// canned JSON to every connection until the test ends.
/// Test: used by the #4005/#4001 tests below.
async fn spawn_health_listener(body: serde_json::Value, delay: std::time::Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let body = body.to_string();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                tokio::time::sleep(delay).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    addr
}

/// A `/health` body from a healthy, fully-ready daemon.
fn healthy_body() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "daemon_state": "ready",
        "worker": {"in_flight": 0, "wedged": false},
    })
}

/// Why (issue #4001): THE false positive. During the #3992 incident six
/// trusty-memory threads were parked in `concurrent_open::backoff_sleep_ms`
/// with a `memory_remember` hung ~1800 s, and `tm doctor` reported HEALTHY the
/// entire time — because it only ever asked "did the socket answer?". Here the
/// listener is fully live and returns a clean HTTP 200; only the body reveals
/// the wedge. Before this change doctor returned `Ok` for exactly this input.
/// What: asserts a wedge-reporting daemon is NOT `Ok`, and that the message
/// names the wedge so an operator is not sent looking elsewhere.
/// Test: itself.
#[tokio::test]
async fn memory_wedged_worker_pool_is_not_ok() {
    let body = serde_json::json!({
        "status": "ok",
        "daemon_state": "ready",
        "worker": {"in_flight": 6, "oldest_age_secs": 1800, "wedged": true},
    });
    let addr = spawn_health_listener(body, std::time::Duration::ZERO).await;
    let check = probe_health(
        "memory",
        "trusty-memory",
        Transport::Http(&format!("http://{addr}/health")),
        &addr,
    )
    .await;

    assert_ne!(
        check.status,
        CheckStatus::Ok,
        "a live listener over a wedged worker pool must NOT report healthy: {}",
        check.message
    );
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("WEDGED"),
        "message must name the wedge: {}",
        check.message
    );
    assert!(
        check.message.contains("1800"),
        "message must carry the observed age: {}",
        check.message
    );
}

/// Why (issue #4005): THE false negative. A daemon that is genuinely serving
/// must not be called unreachable just because `/health` is slower than the
/// probe's budget — `/health` samples RSS/CPU and enumerates file descriptors,
/// work the MCP path never does. This listener answers correctly but takes
/// 2.5 s, which the old 2 s single-shot budget turned into
/// "trusty-memory unreachable at ...".
/// What: asserts a slow-but-serving daemon reports `Ok`.
/// Test: itself.
#[tokio::test]
async fn memory_slow_but_serving_daemon_is_ok() {
    let addr = spawn_health_listener(healthy_body(), std::time::Duration::from_millis(2500)).await;
    let check = probe_health(
        "memory",
        "trusty-memory",
        Transport::Http(&format!("http://{addr}/health")),
        &addr,
    )
    .await;

    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "a serving daemon that is merely slow must not be reported unreachable: {}",
        check.message
    );
    assert!(
        !check.message.contains("unreachable"),
        "message must not claim unreachable: {}",
        check.message
    );
}

/// Why (issue #4005, warm-up case): the issue explicitly notes the failure may
/// only reproduce right after a `tm` restart, and asks that the fix handle
/// warm-up specifically rather than reporting a hard failure. A warming daemon
/// is serving (recall falls back to the non-embedder path) so it is not a
/// failure; it is not fully ready either, so it is not `Ok`.
/// What: asserts warm-up is `Warn` — neither `Fail` nor `Ok`.
/// Test: itself.
#[tokio::test]
async fn memory_warming_is_warn_not_fail() {
    let body = serde_json::json!({
        "status": "ok",
        "daemon_state": "warming",
        "worker": {"in_flight": 1, "oldest_age_secs": 2, "wedged": false},
    });
    let addr = spawn_health_listener(body, std::time::Duration::ZERO).await;
    let check = probe_health(
        "memory",
        "trusty-memory",
        Transport::Http(&format!("http://{addr}/health")),
        &addr,
    )
    .await;

    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "post-restart warm-up must be a warning, not a failure: {}",
        check.message
    );
    assert!(
        check.message.contains("WARMING"),
        "message must name the warm-up: {}",
        check.message
    );
}

/// Why (the unifying principle): a probe that times out has learned NOTHING.
/// Rendering that as `Fail` is the #4005 false negative; rendering it as `Ok`
/// would be a false positive. It must render as its own state, and that state
/// must never be healthy.
/// What: points the probe at a listener that accepts the connection and then
/// never answers, and asserts `Unknown` — explicitly neither `Ok` nor `Fail`.
/// Test: itself.
#[tokio::test]
async fn memory_timeout_is_unknown_not_fail() {
    // Accept connections but never respond, so the probe times out rather
    // than being refused.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock); // hold the socket open, answer nothing
        }
    });

    let check = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        probe_health(
            "memory",
            "trusty-memory",
            Transport::Http(&format!("http://{addr}/health")),
            &addr,
        ),
    )
    .await
    .expect("probe must stay bounded");

    assert_eq!(
        check.status,
        CheckStatus::Unknown,
        "a timed-out probe must be UNKNOWN, not a verdict: {}",
        check.message
    );
    assert!(
        check.message.contains("could not be determined"),
        "message must say so plainly: {}",
        check.message
    );
}

/// Why (issue #4001, forward-compatibility): a daemon too old to report worker
/// occupancy cannot support a healthy verdict — doctor has no observation to
/// base one on. Claiming `Ok` there would reintroduce exactly the inference
/// this fix removes.
/// What: asserts a 2xx body with no `worker` block is `Unknown`.
/// Test: itself.
#[tokio::test]
async fn health_body_without_worker_block_is_unknown() {
    let body = serde_json::json!({"status": "ok", "daemon_state": "ready"});
    let addr = spawn_health_listener(body, std::time::Duration::ZERO).await;
    let check = probe_health(
        "memory",
        "trusty-memory",
        Transport::Http(&format!("http://{addr}/health")),
        &addr,
    )
    .await;

    assert_eq!(
        check.status,
        CheckStatus::Unknown,
        "no worker observation means health is undetermined: {}",
        check.message
    );
}

/// Why: the aggregate verdict is what an operator actually reads. If a single
/// `Unknown` could still fold into an `Ok` report, the third state would be
/// cosmetic.
/// What: asserts `Unknown` outranks `Ok` and `Warn` but not `Fail`.
/// Test: itself.
#[test]
fn unknown_never_aggregates_to_ok() {
    let report = DoctorReport::from_checks(vec![
        DoctorCheck::new("a", CheckStatus::Ok, "fine"),
        DoctorCheck::new("b", CheckStatus::Unknown, "no idea"),
    ]);
    assert_eq!(report.overall, CheckStatus::Unknown);

    let with_warn = DoctorReport::from_checks(vec![
        DoctorCheck::new("a", CheckStatus::Warn, "meh"),
        DoctorCheck::new("b", CheckStatus::Unknown, "no idea"),
    ]);
    assert_eq!(with_warn.overall, CheckStatus::Unknown);

    // A real failure still outranks an unknown — a known problem is more
    // actionable than an undetermined one.
    let with_fail = DoctorReport::from_checks(vec![
        DoctorCheck::new("a", CheckStatus::Unknown, "no idea"),
        DoctorCheck::new("b", CheckStatus::Fail, "broken"),
    ]);
    assert_eq!(with_fail.overall, CheckStatus::Fail);
}
