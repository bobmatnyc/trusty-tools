//! Per-project `gh` account enforcement (#2081) — the non-mutating mechanism
//! that defaults `gh` operations for a project to its configured
//! [`crate::project::record::Project::gh_user`], split out of
//! `gh_account.rs` to keep both files under the workspace's 500-SLOC
//! production cap (issue #3070; the #3025 spawn-env feature is unrelated to
//! this enforcement path and stays in `gh_account.rs`).
//!
//! Why: `tm`'s daemon manages many projects concurrently, and a bare `gh auth
//! switch` rewrites the single SHARED `~/.config/gh/hosts.yml` "active"
//! pointer, so switching for project A would silently redirect project B's
//! concurrent `gh` calls to the wrong identity. This module deliberately does
//! NOT run an unscoped `gh auth switch` for that reason.
//!
//! What: [`ensure_gh_account_for_project`] requires an already-isolated
//! `GH_CONFIG_DIR` (the convention this codebase's `gh_identity` module and
//! the operator's own shell tooling already use to scope `gh` state per
//! project/context) and only ever mutates the "active" pointer INSIDE that
//! one directory, verifying the result with a fresh `gh auth status` before
//! returning success. No `GH_CONFIG_DIR` in the environment → refusal, never
//! a fall-through to the shared global store.
//!
//! Test: the pure decision logic (`parse_active_account_from_status`,
//! `verify_active_account`) is unit-tested inline; the public entry point's
//! refusal path is covered without a live `gh` by
//! `ensure_gh_account_for_project_refuses_without_config_dir`.

use std::path::PathBuf;

use anyhow::Context;

use super::{GH_ENFORCE_TIMEOUT, extract_login_token, run_bounded};

/// Environment variable that scopes every `gh` invocation to a private config
/// home — the project-isolation primitive [`ensure_gh_account_for_project`]
/// requires rather than reinvents (already used by `gh_identity::GithubConfig`
/// and, on at least one operator's host, by a shell `chpwd` hook that sets it
/// per working directory).
pub const GH_CONFIG_DIR_ENV: &str = "GH_CONFIG_DIR";

/// Env vars that can override `gh`'s config-dir-resolved identity; must be
/// neutralised before enforcing/verifying the active account so a stray token
/// cannot silently outrank the isolated config dir.
const GH_TOKEN_ENV_VARS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// Ensure `gh` is authenticated as `expected_user` for `config_dir`, WITHOUT
/// mutating any other gh config store — the #2081 per-project mechanism.
///
/// Why: `tm`'s daemon runs many projects concurrently; a bare `gh auth switch`
/// rewrites the single shared `~/.config/gh/hosts.yml` "active" pointer, so
/// switching for one project would silently redirect a concurrently-running
/// project's `gh` calls to the wrong identity. Scoping every read AND the
/// switch itself to an already-isolated `config_dir` (via `GH_CONFIG_DIR`)
/// confines the mutation to that one project's store — two projects with
/// distinct config dirs can run this concurrently without interfering. The
/// motivating incident (#2081) was NOT fixed by `GH_CONFIG_DIR` isolation
/// alone: the isolated store itself still had the wrong account marked
/// active, so this also corrects (and then re-verifies) the active-user
/// pointer INSIDE that store rather than assuming a config-dir switch is
/// sufficient.
/// What: runs `gh auth status` scoped to `config_dir` (with `GH_TOKEN` /
/// `GITHUB_TOKEN` stripped from the child env so a stray token cannot outrank
/// the config-dir identity); if `expected_user` is already active, returns
/// immediately with no mutation. Otherwise runs `gh auth switch --user
/// <expected_user>` under the same scoped, token-stripped env, then re-runs
/// `gh auth status` and fails loudly (never silently proceeds) unless the
/// re-check confirms `expected_user` is now active.
/// Test: `verify_active_account_*`, `parse_active_account_from_status_*`
/// cover the pure decision logic; `ensure_gh_account_for_project_*` cover the
/// public entry point's refusal path without a live `gh`.
fn ensure_gh_account_in_dir(
    expected_user: &str,
    config_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let dir = config_dir.to_string_lossy().to_string();
    let host = crate::core::trusty_tools_config::DEFAULT_GITHUB_HOST;

    let status_text = run_gh_scoped(&dir, ["auth", "status"])
        .context("failed to run `gh auth status` scoped to the project's GH_CONFIG_DIR")?;
    if verify_active_account(&status_text, expected_user).is_ok() {
        return Ok(());
    }

    let switch = std::process::Command::new("gh")
        .args([
            "auth",
            "switch",
            "--hostname",
            host,
            "--user",
            expected_user,
        ])
        .env(GH_CONFIG_DIR_ENV, &dir)
        .env_remove(GH_TOKEN_ENV_VARS[0])
        .env_remove(GH_TOKEN_ENV_VARS[1])
        .output()
        .with_context(|| format!("failed to spawn `gh auth switch --user {expected_user}`"))?;
    if !switch.status.success() {
        let stderr = String::from_utf8_lossy(&switch.stderr);
        anyhow::bail!(
            "`gh auth switch --user {expected_user}` failed inside {dir}: {} \
             (is '{expected_user}' logged in under this GH_CONFIG_DIR? run \
             `GH_CONFIG_DIR={dir} gh auth login` first)",
            stderr.trim()
        );
    }

    let recheck = run_gh_scoped(&dir, ["auth", "status"])
        .context("failed to re-verify `gh auth status` after switching")?;
    verify_active_account(&recheck, expected_user).map_err(|msg| {
        anyhow::anyhow!(
            "gh account switch to '{expected_user}' did not take effect inside {dir}: {msg}"
        )
    })
}

/// Run a bounded `gh` subprocess scoped to `config_dir`, with any env-token
/// override stripped, returning the combined stdout+stderr text.
///
/// Why: both the pre-check and the post-switch re-verification in
/// [`ensure_gh_account_in_dir`] need the identical scoped-subprocess
/// invocation; a shared helper keeps the env-stripping and timeout bound in
/// one place.
/// What: bounded by [`GH_ENFORCE_TIMEOUT`]; returns an error on a spawn
/// failure or timeout.
/// Test: exercised indirectly via `ensure_gh_account_for_project_*` (no live
/// `gh` in CI, so only the refusal path — which never reaches this
/// function — is asserted there).
fn run_gh_scoped(config_dir: &str, args: [&'static str; 2]) -> anyhow::Result<String> {
    let config_dir = config_dir.to_string();
    run_bounded(GH_ENFORCE_TIMEOUT, move || {
        let out = std::process::Command::new("gh")
            .args(args)
            .env(GH_CONFIG_DIR_ENV, &config_dir)
            .env_remove(GH_TOKEN_ENV_VARS[0])
            .env_remove(GH_TOKEN_ENV_VARS[1])
            .output()
            .ok()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Some(text)
    })
    .ok_or_else(|| anyhow::anyhow!("`gh` did not respond within {GH_ENFORCE_TIMEOUT:?}"))
}

/// Parse the login marked `Active account: true` from a `gh auth status` transcript.
///
/// Why: [`verify_active_account`] needs the SPECIFIC active login (not just
/// "who is logged in", which `parse_logged_in_accounts` already answers) to
/// confirm a switch actually took effect.
/// What: scans line-by-line, tracking the login introduced by the most recent
/// "Logged in to ..." line, and returns it the first time a following
/// "Active account: true" line is seen. Returns `None` when no login is ever
/// marked active.
/// Test: `parse_active_account_from_status_multi`,
/// `parse_active_account_from_status_single`,
/// `parse_active_account_from_status_none_active`.
pub(crate) fn parse_active_account_from_status(auth_status: &str) -> Option<String> {
    let mut current: Option<String> = None;
    for line in auth_status.lines() {
        if let Some((_, rest)) = line.split_once("Logged in to") {
            current = extract_login_token(rest);
            continue;
        }
        if let Some((_, value)) = line.split_once("Active account:")
            && value.trim().eq_ignore_ascii_case("true")
        {
            return current.clone();
        }
    }
    None
}

/// Decide whether a `gh auth status` transcript confirms `expected` is active.
///
/// Why: isolates the verification decision — including the defensive check
/// for an env-token override that would make the config-dir identity
/// unverifiable — so it is unit-testable without a live `gh` and so
/// [`ensure_gh_account_in_dir`] never has to guess at the meaning of raw
/// status text.
/// What: `Err` when the transcript mentions an environment-variable token
/// override (defence in depth: `GH_TOKEN`/`GITHUB_TOKEN` should already be
/// stripped from the child env, but this catches the case where `gh` reports
/// one anyway); `Err` when no account or a DIFFERENT account is active;
/// `Ok(())` only when [`parse_active_account_from_status`] returns exactly
/// `expected`.
/// Test: `verify_active_account_matches`, `verify_active_account_mismatch`,
/// `verify_active_account_rejects_env_token_override`.
pub(crate) fn verify_active_account(status_text: &str, expected: &str) -> Result<(), String> {
    let lower = status_text.to_lowercase();
    if lower.contains("environment variable")
        && (lower.contains("gh_token") || lower.contains("github_token"))
    {
        return Err(format!(
            "gh is authenticating via a GH_TOKEN/GITHUB_TOKEN environment variable override, \
             so the active account inside this GH_CONFIG_DIR cannot be verified as '{expected}'"
        ));
    }
    match parse_active_account_from_status(status_text) {
        Some(active) if active == expected => Ok(()),
        Some(active) => Err(format!(
            "active account is '{active}', expected '{expected}'"
        )),
        None => Err(format!(
            "could not determine an active gh account (expected '{expected}')"
        )),
    }
}

/// Ensure `gh` operations for a project default to `gh_user` — the #2081
/// public entry point.
///
/// Why: `Project::gh_user` (#2081) declares a project's preferred `gh`
/// account, but honouring it safely requires an already-isolated
/// `GH_CONFIG_DIR` (see the module-level doc for why a global `gh auth
/// switch` is refused). Requiring [`GH_CONFIG_DIR_ENV`] to already be set —
/// rather than inventing a project-specific directory here — aligns with the
/// isolation convention this codebase (`gh_identity::GithubConfig::config_dir`)
/// and at least one operator's own shell tooling already use.
/// What: reads [`GH_CONFIG_DIR_ENV`] from the process environment; `None`/empty
/// → a loud, actionable error (never falls through to mutating the shared
/// default gh config). When present, delegates to
/// [`ensure_gh_account_in_dir`], which is itself a no-op when `gh_user` is
/// already the active account inside that directory.
/// Test: `ensure_gh_account_for_project_refuses_without_config_dir` proves the
/// refusal path never attempts any `gh` call (so it can never perform a
/// global switch) when no isolated config dir is available.
pub fn ensure_gh_account_for_project(gh_user: &str) -> anyhow::Result<()> {
    let gh_user = gh_user.trim();
    anyhow::ensure!(!gh_user.is_empty(), "gh_user must not be empty");

    let config_dir = std::env::var_os(GH_CONFIG_DIR_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no isolated {GH_CONFIG_DIR_ENV} is set in this environment; refusing to run \
                 `gh auth switch` against the shared ~/.config/gh store (that would leak \
                 identity across concurrently-running projects). Export {GH_CONFIG_DIR_ENV} \
                 to a per-project gh config directory (see `github.config_dir` in \
                 TrustyToolsConfig, or the equivalent per-directory shell convention) before \
                 retrying."
            )
        })?;

    ensure_gh_account_in_dir(gh_user, &config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `gh auth status` output with two github.com accounts logged in.
    const MULTI_AUTH_STATUS: &str = "\
github.com
  ✓ Logged in to github.com account bob-duetto (keyring)
  - Active account: true
  - Git operations protocol: https
  - Token: gho_************************************
  - Token scopes: 'admin:org', 'gist', 'project', 'repo', 'workflow'

  ✓ Logged in to github.com account bobmatnyc (keyring)
  - Active account: false
  - Git operations protocol: https
  - Token: gho_************************************
  - Token scopes: 'gist', 'read:org', 'repo', 'workflow'
";

    /// Single-account `gh auth status` output.
    const SINGLE_AUTH_STATUS: &str = "\
github.com
  ✓ Logged in to github.com account bobmatnyc (keyring)
  - Active account: true
  - Git operations protocol: https
  - Token: gho_************************************
";

    /// Why: the active-account parser must pick the login marked
    /// `Active account: true`, not just the first login seen.
    /// Test: itself.
    #[test]
    fn parse_active_account_from_status_multi() {
        assert_eq!(
            parse_active_account_from_status(MULTI_AUTH_STATUS).as_deref(),
            Some("bob-duetto")
        );
    }

    /// Why: a single-account transcript must resolve to that one login.
    /// Test: itself.
    #[test]
    fn parse_active_account_from_status_single() {
        assert_eq!(
            parse_active_account_from_status(SINGLE_AUTH_STATUS).as_deref(),
            Some("bobmatnyc")
        );
    }

    /// Why: a transcript with no `Active account: true` line (or no logins at
    /// all) must yield `None`, never panic.
    /// Test: itself.
    #[test]
    fn parse_active_account_from_status_none_active() {
        assert_eq!(parse_active_account_from_status(""), None);
        assert_eq!(
            parse_active_account_from_status(
                "github.com\n  ✓ Logged in to github.com account x\n  - Active account: false\n"
            ),
            None
        );
    }

    /// Why: [`verify_active_account`] must succeed only when the expected
    /// login is the one marked active.
    /// Test: itself.
    #[test]
    fn verify_active_account_matches() {
        assert!(verify_active_account(SINGLE_AUTH_STATUS, "bobmatnyc").is_ok());
    }

    /// Why: a mismatched active account (the exact #2081 incident shape —
    /// `bob-duetto` active when `bobmatnyc` was expected) must be a loud `Err`,
    /// never a silent pass.
    /// Test: itself.
    #[test]
    fn verify_active_account_mismatch() {
        let err = verify_active_account(MULTI_AUTH_STATUS, "bobmatnyc").unwrap_err();
        assert!(err.contains("bob-duetto"), "err: {err}");
        assert!(err.contains("bobmatnyc"), "err: {err}");
    }

    /// Why: if `gh` reports it is authenticating via an environment-variable
    /// token override, the config-dir identity cannot be trusted even if some
    /// login happens to also be marked active — this must be caught and
    /// refused explicitly (defence in depth alongside stripping `GH_TOKEN` /
    /// `GITHUB_TOKEN` from the child env before the call).
    /// Test: itself.
    #[test]
    fn verify_active_account_rejects_env_token_override() {
        let text = "The value of the GH_TOKEN environment variable is being used for authentication.\ngithub.com\n  ✓ Logged in to github.com account bobmatnyc\n  - Active account: true\n";
        let err = verify_active_account(text, "bobmatnyc").unwrap_err();
        assert!(err.contains("environment variable"), "err: {err}");
    }

    /// Why (#2081): the public entry point must REFUSE (never fall through to
    /// mutating the shared default `~/.config/gh` store) when no isolated
    /// `GH_CONFIG_DIR` is present in the environment. Because this returns
    /// before spawning any `gh` subprocess, it structurally proves the
    /// refusal path can never perform a global `gh auth switch`.
    /// Test: itself.
    #[test]
    fn ensure_gh_account_for_project_refuses_without_config_dir() {
        let _g = crate::core::trusty_tools_config::env_test_lock();
        // SAFETY: guarded by the crate-wide env test lock; restored below.
        let prior = std::env::var_os(GH_CONFIG_DIR_ENV);
        unsafe { std::env::remove_var(GH_CONFIG_DIR_ENV) };
        let err = ensure_gh_account_for_project("bobmatnyc").unwrap_err();
        // SAFETY: restoring whatever was present before the test ran.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(GH_CONFIG_DIR_ENV, v),
                None => std::env::remove_var(GH_CONFIG_DIR_ENV),
            }
        }
        let msg = err.to_string();
        assert!(msg.contains(GH_CONFIG_DIR_ENV), "msg: {msg}");
        assert!(
            msg.contains("shared"),
            "expected the shared-store refusal rationale: {msg}"
        );
    }

    /// Why (#2081): an empty `gh_user` is a caller bug, not a valid
    /// "no preference" signal (that is `Option::None` at the `Project` layer)
    /// — must be rejected before even checking `GH_CONFIG_DIR`.
    /// Test: itself.
    #[test]
    fn ensure_gh_account_for_project_rejects_empty_login() {
        let err = ensure_gh_account_for_project("   ").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }
}
