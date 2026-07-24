//! Tests for the #3025 spawn-time `GH_TOKEN` minting resolver
//! (`resolve_gh_account_env_with`/`resolve_gh_account_env`/
//! `resolve_gh_account_env_for_registry`/`find_pinned_gh_account` in
//! `gh_account.rs`).
//!
//! Why: split into a companion `_tests.rs` file (rather than growing the
//! large inline `#[cfg(test)] mod tests` in `gh_account.rs` itself) purely to
//! keep `gh_account.rs` — a production file capped at 500 SLOC — clear of
//! further growth; this file is classified as a test file (1500 SLOC cap) by
//! its `_tests.rs` suffix. Every `resolve_gh_account_env_with` case exercises
//! a FAKE token resolver closure so no live `gh` login is ever required in
//! CI; every `find_pinned_gh_account`/`resolve_gh_account_env_for_registry`
//! case exercises a REAL, temp-dir-backed `ProjectRegistry` fixture (no live
//! `gh` or git repository needed either) — this is the review-follow-up
//! coverage proving the registry, not the static config file, is consulted.
//! What: the four `resolve_gh_account_env_with` outcomes (no account, blank
//! account, success, resolver failure — the fail-open spawn contract), plus
//! the registry-matching coverage: a registered project's pinned
//! `gh_account` IS picked up for its `repo_url`, an unregistered/mismatched
//! `repo_url` is NOT, and a registered project with no `gh_account` set
//! yields `None` — plus the end-to-end no-origin-workspace case for
//! `resolve_gh_account_env_for_registry` itself.
//! Test: itself (each `#[test]`/`#[tokio::test]` below is its own coverage unit).

use super::{
    GH_TOKEN_ENV_VAR, GH_USER_ENV_VAR, find_pinned_gh_account, resolve_gh_account_env_for_registry,
    resolve_gh_account_env_with,
};
use crate::project::Project;
use crate::project::ProjectRegistry;

/// Why: `gh_account: None` (no pinning configured) must resolve to `None` —
/// nothing to inject, no regression for every project that never sets the
/// field.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_unset_is_none() {
    let result = resolve_gh_account_env_with(None, |_| Ok("unused".to_string()));
    assert!(result.is_none());
}

/// Why: a blank/whitespace-only `gh_account` must be treated the same as
/// unset — never invokes the resolver, never fabricates env vars from
/// nothing.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_blank_is_none() {
    let result = resolve_gh_account_env_with(Some("   "), |_| Ok("unused".to_string()));
    assert!(result.is_none());
}

/// Why: a successful resolution must inject BOTH `GH_TOKEN` (the minted
/// token) and `GH_USER` (the pinned account name) — in that order — so a
/// caller applying `vars()` sequentially gets a deterministic env.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_success_returns_token_and_user() {
    let result = resolve_gh_account_env_with(Some("bobmatnyc"), |account| {
        assert_eq!(account, "bobmatnyc");
        Ok("ghp_fake_token".to_string())
    });
    let vars = result.expect("some").expect("ok");
    assert_eq!(
        vars,
        vec![
            (GH_TOKEN_ENV_VAR.to_string(), "ghp_fake_token".to_string()),
            (GH_USER_ENV_VAR.to_string(), "bobmatnyc".to_string()),
        ]
    );
}

/// Why: a resolver failure (account not logged in, `gh` missing, etc.) must
/// surface as `Some(Err(..))` — never panic, never silently fall through to
/// an empty/placeholder token — so the caller can log a clear warning and
/// proceed WITHOUT injecting `GH_TOKEN` (issue #3025's documented failure
/// mode).
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_failure_returns_err() {
    let result = resolve_gh_account_env_with(Some("bob-duetto"), |_| {
        Err("account 'bob-duetto' is not logged in".to_string())
    });
    let err = result.expect("some").expect_err("err");
    assert!(err.contains("bob-duetto"), "err: {err}");
}

/// Why: the account name passed to the resolver must be TRIMMED (leading/
/// trailing whitespace stripped) so a config value with incidental
/// whitespace still resolves correctly.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_trims_account_name() {
    let result = resolve_gh_account_env_with(Some("  bobmatnyc  "), |account| {
        assert_eq!(account, "bobmatnyc");
        Ok("tok".to_string())
    });
    let vars = result.expect("some").expect("ok");
    assert!(vars.contains(&(GH_USER_ENV_VAR.to_string(), "bobmatnyc".to_string())));
}

/// Build a minimal `Project` fixture, mirroring the register-route shape.
fn project(name: &str, repo_url: &str, gh_account: Option<&str>) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: gh_account.map(str::to_string),
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// Why (#3025 review follow-up, CRITICAL fix): this is the exact coverage
/// the review demanded — register a project (via the registry, the REAL
/// write target `tm projects register --gh-account`/the PATCH route/the MCP
/// tool all use) with a pinned `gh_account`, then assert the resolver picks
/// it up for that project's `repo_url`. Proves the registry — not the
/// static `TrustyToolsConfig` file the original implementation mistakenly
/// consulted — is the source of truth.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_picks_up_registered_gh_account() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "widget",
            "https://github.com/acme/widget",
            Some("bobmatnyc"),
        ))
        .await
        .expect("register");

    let found = find_pinned_gh_account(&registry, "https://github.com/acme/widget").await;
    assert_eq!(found.as_deref(), Some("bobmatnyc"));

    // Tolerates the `.git`-suffix/scheme variance `repo_url_matches` handles.
    let found_git_suffix =
        find_pinned_gh_account(&registry, "https://github.com/acme/widget.git").await;
    assert_eq!(found_git_suffix.as_deref(), Some("bobmatnyc"));
}

/// Why: a `repo_url` that matches NO registered project must yield `None` —
/// not a panic, not a false-positive match against an unrelated project.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_no_match_is_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "widget",
            "https://github.com/acme/widget",
            Some("bobmatnyc"),
        ))
        .await
        .expect("register");

    let found = find_pinned_gh_account(&registry, "https://github.com/acme/other-repo").await;
    assert_eq!(found, None);
}

/// Why: a registered project with NO `gh_account` set must yield `None` —
/// the no-regression case for every project that never pins one.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_registered_without_gh_account_is_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project("widget", "https://github.com/acme/widget", None))
        .await
        .expect("register");

    let found = find_pinned_gh_account(&registry, "https://github.com/acme/widget").await;
    assert_eq!(found, None);
}

/// Why: a workspace with no git origin (a bare, non-git temp dir) must
/// resolve to an EMPTY vec end-to-end via `resolve_gh_account_env_for_registry`
/// itself — no regression for every workspace that predates #3025, and no
/// panic/hang even with an empty registry.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_no_origin_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let vars = resolve_gh_account_env_for_registry(&registry, workspace.path()).await;
    assert!(vars.is_empty(), "vars: {vars:?}");
}
