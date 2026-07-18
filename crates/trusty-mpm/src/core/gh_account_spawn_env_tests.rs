//! Tests for the #3025 spawn-time `GH_TOKEN` minting resolver
//! (`resolve_gh_account_env_with`/`resolve_gh_account_env` in `gh_account.rs`).
//!
//! Why: split into a companion `_tests.rs` file (rather than growing the
//! inline `#[cfg(test)] mod tests` in `gh_account.rs` itself) purely to keep
//! `gh_account.rs` — a production file capped at 500 SLOC — well clear of
//! its cap; this file is classified as a test file (1500 SLOC cap) by its
//! `_tests.rs` suffix. Every case here exercises
//! [`resolve_gh_account_env_with`] with a FAKE token resolver closure so no
//! live `gh` login is ever required in CI (per the #3025 task brief).
//! What: the four documented outcomes — no account configured, a blank
//! account, a successful resolution, and a resolver failure (the fail-open
//! spawn contract: a failure must never propagate as an `Err` the caller
//! cannot recover from — it is surfaced as `Some(Err(..))` for the caller to
//! log and continue past).
//! Test: itself (each `#[test]` below is its own coverage unit).

use super::{
    GH_TOKEN_ENV_VAR, GH_USER_ENV_VAR, resolve_gh_account_env_for_workspace,
    resolve_gh_account_env_with,
};

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

/// Why: a workspace with no git origin (or that matches no registered
/// project) must resolve to an EMPTY vec — no regression for every workspace
/// that predates #3025 / never pins a `gh_account`. Uses a bare temp dir (not
/// a git repo at all) so `get_origin_url` deterministically returns `None`
/// without depending on any real project config on the test host.
/// Test: itself.
#[test]
fn resolve_gh_account_env_for_workspace_no_origin_is_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let vars = resolve_gh_account_env_for_workspace(tmp.path());
    assert!(vars.is_empty(), "vars: {vars:?}");
}
