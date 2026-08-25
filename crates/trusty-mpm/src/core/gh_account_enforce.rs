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
//! one directory, verifying the result with a fresh `gh api user` probe before
//! returning success. No `GH_CONFIG_DIR` in the environment → refusal, never
//! a fall-through to the shared global store.
//!
//! Test: the pure decision logic (`parse_active_account_from_status`,
//! `verify_active_account`) is unit-tested inline; the public entry point's
//! refusal path is covered without a live `gh` by
//! `ensure_gh_account_for_project_refuses_without_config_dir`.
//!
//! ## What verification establishes (#5849)
//!
//! Verification asks `gh api user --jq .login` — the login GitHub itself
//! attributes to the credential `gh` resolves inside this `GH_CONFIG_DIR` —
//! and compares THAT to the expected account. It previously parsed the text of
//! a `gh auth status` transcript, which only echoes what the scoped `hosts.yml`
//! claims: a hand-written `hosts.yml` naming an account that exists nowhere on
//! the machine printed a green checkmark, live token scopes and `Active
//! account: true`, and enforcement passed it.
//!
//! So the identity this module confirms is the one an API call runs as, not the
//! one the config file names. When the two disagree — the #5656 divergence,
//! where a credential can resolve from the macOS keyring rather than the scoped
//! config dir — the API answer decides and the disagreement is logged at WARN.
//! That is the right side to take here: every downstream `gh` call is an API
//! call, so the API answer is what "operating as the wrong account" means.
//! Whether `GH_CONFIG_DIR` can isolate a credential at all stays #5656's
//! question; this module reports the divergence rather than papering over it.
//!
//! Every probe failure — `gh` missing, a non-zero exit, blank output, or the
//! `GH_ENFORCE_TIMEOUT` bound — is a verification FAILURE, never a pass.
//!
//! ## Production wiring (#3312)
//!
//! This enforcement had zero production call sites until #3312: the function
//! existed and was tested, but nothing on a real `gh`/git operation path ever
//! ran it. It is now wired at every point a wrong-account operation would
//! actually do damage (project registration / workspace provisioning in the
//! daemon's managed-spawn path, and issue/PR creation in the `tm
//! ticket`/`tm issue`/`tm watch` CLI paths) via [`configured_account_pair`]
//! (the shared decision of WHETHER there is anything to enforce) plus
//! [`ensure_gh_account_in_dir`] (the explicit-directory core every wired call
//! site — CLI and daemon alike — now calls directly, in place of
//! [`ensure_gh_account_for_project`]'s env-var-reading wrapper). The daemon is
//! a single long-lived process serving MANY projects concurrently, so
//! mutating `std::env` process-globally to satisfy the env-based wrapper's
//! contract is unsafe there; calling the explicit-directory core avoids that
//! entirely for BOTH callers (keeping their behaviour identical rather than
//! having the CLI and daemon diverge on how they invoke enforcement).
//! [`ensure_gh_account_for_project`] itself is left unchanged and still
//! `pub`/tested — it remains the documented entry point for any FUTURE caller
//! that already owns an isolated `GH_CONFIG_DIR` in its own process env (e.g.
//! a single-shot script).
//!
//! See `crates::core::gh_identity::resolve_project_aware` (CLI) and
//! `crates::core::git_identity::resolve_for_config_enforced` (daemon) for the
//! two wired call sites; `configured_account_pair` decides per-project
//! whether enforcement applies at all (never for a project that only set
//! `config_dir`, or set neither field).

use std::path::PathBuf;

use anyhow::Context;

use crate::core::trusty_tools_config::GithubConfig;

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
/// What: probes the identity scoped to `config_dir` (with `GH_TOKEN` /
/// `GITHUB_TOKEN` stripped from the child env so a stray token cannot outrank
/// the config-dir identity) with BOTH `gh auth status` and `gh api user --jq
/// .login`; `gh api user` decides (#5849), the transcript only supplies the
/// env-token-override guard and the diagnostic. If `expected_user` is the
/// authenticated identity, returns immediately with no mutation. Otherwise
/// runs `gh auth switch --user <expected_user>` under the same scoped,
/// token-stripped env, re-probes, and fails loudly (never silently proceeds)
/// unless the re-check confirms `expected_user`.
/// Test: `verify_active_account_*`, `parse_active_account_from_status_*`
/// cover the pure decision logic; `ensure_gh_account_for_project_*` cover the
/// public entry point's refusal path without a live `gh`;
/// `ensure_gh_account_in_dir_*` (#3312, #5849) exercise this function directly
/// against a fake `gh` on `PATH`, covering the self-heal, switch-failure,
/// already-active no-op, contradicted-transcript, and failed-probe outcomes.
///
/// Public (#3312) so every wired production call site — the CLI's
/// `gh_identity::resolve_project_aware` and the daemon's
/// `git_identity::resolve_for_config_enforced` — can call it directly with an
/// already-resolved directory, instead of round-tripping through
/// [`ensure_gh_account_for_project`]'s process-env contract (see the module
/// docs for why: the daemon serves many projects concurrently, so mutating
/// process-global env to satisfy that contract is unsafe there, and the CLI
/// path matches it for consistency rather than diverging on how the two
/// callers invoke the same enforcement).
pub fn ensure_gh_account_in_dir(
    expected_user: &str,
    config_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let dir = config_dir.to_string_lossy().to_string();
    let host = crate::core::trusty_tools_config::DEFAULT_GITHUB_HOST;

    let status_text = run_gh_scoped(&dir, ["auth", "status"])
        .context("failed to run `gh auth status` scoped to the project's GH_CONFIG_DIR")?;
    // #5849: the transcript echoes hosts.yml; `gh api user` is the identity the
    // API calls actually run as, and it decides.
    let probe = api_login_scoped(&dir);
    if verify_active_account(
        &status_text,
        probe.as_deref().map_err(String::as_str),
        expected_user,
    )
    .is_ok()
    {
        warn_on_identity_divergence(&status_text, &probe);
        return Ok(());
    }

    // #5475: single `gh` entry point; the scoping env is unchanged.
    let switch = trusty_common::gh::GhCommand::new([
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
    .output_blocking()
    .with_context(|| format!("failed to spawn `gh auth switch --user {expected_user}`"))?;
    if !switch.success {
        let stderr = &switch.stderr;
        anyhow::bail!(
            "`gh auth switch --user {expected_user}` failed inside {dir}: {} \
             (is '{expected_user}' logged in under this GH_CONFIG_DIR? run \
             `GH_CONFIG_DIR={dir} gh auth login` first)",
            stderr.trim()
        );
    }

    let recheck = run_gh_scoped(&dir, ["auth", "status"])
        .context("failed to re-verify `gh auth status` after switching")?;
    let recheck_probe = api_login_scoped(&dir);
    let verdict = verify_active_account(
        &recheck,
        recheck_probe.as_deref().map_err(String::as_str),
        expected_user,
    );
    if verdict.is_ok() {
        warn_on_identity_divergence(&recheck, &recheck_probe);
    }
    verdict.map_err(|msg| {
        anyhow::anyhow!(
            "gh account switch to '{expected_user}' did not take effect inside {dir}: {msg}"
        )
    })
}

/// Ask GitHub which login the credential scoped to `config_dir` belongs to.
///
/// Why (#5849): `gh auth status` reports what the scoped `hosts.yml` CLAIMS —
/// it prints a green checkmark and live token scopes for an account that
/// exists nowhere on the machine. `gh api user` is answered by GitHub itself
/// using the credential `gh` resolves, so it is the identity every subsequent
/// `gh` call will actually operate as. That is the only fact worth checking
/// before enforcement reports success.
/// What: `gh api user --jq .login`, scoped to `config_dir` and with the
/// env-token overrides stripped exactly as [`run_gh_scoped`] does, bounded by
/// [`GH_ENFORCE_TIMEOUT`]. Every failure mode — `gh` missing, a non-zero exit
/// (unauthenticated, network, rate limit), blank output, or the timeout —
/// returns `Err`, which [`verify_active_account`] treats as verification
/// failure. It never yields a login it did not read from GitHub.
/// Test: `ensure_gh_account_in_dir_fails_closed_when_the_api_probe_fails` and
/// `ensure_gh_account_in_dir_rejects_a_transcript_the_api_contradicts` drive
/// it through a fake `gh` on `PATH`.
fn api_login_scoped(config_dir: &str) -> Result<String, String> {
    let dir = config_dir.to_string();
    run_bounded(GH_ENFORCE_TIMEOUT, move || {
        // #5475: single `gh` entry point. `nonempty_stdout_blocking` folds a
        // non-zero exit and a blank login into the same `Err`.
        Some(
            trusty_common::gh::GhCommand::new(["api", "user", "--jq", ".login"])
                .env(GH_CONFIG_DIR_ENV, &dir)
                .env_remove(GH_TOKEN_ENV_VARS[0])
                .env_remove(GH_TOKEN_ENV_VARS[1])
                .nonempty_stdout_blocking()
                .map_err(|e| format!("`gh api user --jq .login` failed: {e}")),
        )
    })
    .unwrap_or_else(|| {
        Err(format!(
            "`gh api user --jq .login` did not respond within {GH_ENFORCE_TIMEOUT:?}"
        ))
    })
}

/// Log when the scoped `hosts.yml` and the credential name different logins.
///
/// Why (#5656, #5849): verification now passes on the API answer alone, so a
/// config dir whose transcript claims a DIFFERENT account would otherwise go
/// unremarked — the credential resolved from somewhere other than the config
/// dir (a machine keyring, per #5656), and the isolation the operator believes
/// they configured is not the isolation they have. Enforcement is still
/// correct to proceed (the effective identity is the expected one), but the
/// divergence is worth a loud line in the log rather than silence.
/// What: no-op unless the probe succeeded AND the transcript names an active
/// login that differs from it; emits one WARN naming both.
/// Test: side-effect logging only; the decision it guards is covered by
/// `ensure_gh_account_in_dir_accepts_the_api_answer_over_a_stale_transcript`.
fn warn_on_identity_divergence(status_text: &str, probe: &Result<String, String>) {
    let Ok(api_login) = probe else { return };
    if let Some(claimed) = parse_active_account_from_status(status_text)
        && &claimed != api_login
    {
        tracing::warn!(
            claimed_by_config_dir = %claimed,
            authenticated_as = %api_login,
            "gh auth status and gh api user name different accounts; \
             the credential did not come from this GH_CONFIG_DIR (see #5656)"
        );
    }
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
        // #5475: single `gh` entry point.
        let out = trusty_common::gh::GhCommand::new(args)
            .env(GH_CONFIG_DIR_ENV, &config_dir)
            .env_remove(GH_TOKEN_ENV_VARS[0])
            .env_remove(GH_TOKEN_ENV_VARS[1])
            .output_blocking()
            .ok()?;
        Some(out.combined())
    })
    .ok_or_else(|| anyhow::anyhow!("`gh` did not respond within {GH_ENFORCE_TIMEOUT:?}"))
}

/// Parse the login marked `Active account: true` from a `gh auth status` transcript.
///
/// Why: what the scoped config dir CLAIMS is no longer the verification
/// itself (#5849 moved that to `gh api user`), but it is still the specific
/// fact worth reporting when the claim and the credential disagree — the
/// #5656 divergence. `parse_logged_in_accounts` answers "who is logged in",
/// which is a different question.
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

/// Decide whether the scoped `gh` really authenticates as `expected`.
///
/// Why (#5849): this decision used to rest on `status_text` alone, and a
/// `gh auth status` transcript is not evidence of an identity — it echoes the
/// scoped `hosts.yml`, so a `hosts.yml` naming an account that exists nowhere
/// on the machine produced `Active account: true` and passed. `api_login` is
/// GitHub's own answer for the credential `gh` resolved, so it is what
/// downstream `gh` calls will operate as. Keeping the decision in one pure
/// function keeps it unit-testable without a live `gh`.
/// What: `Err` when the transcript mentions an environment-variable token
/// override (defence in depth: `GH_TOKEN`/`GITHUB_TOKEN` are already stripped
/// from the child env, but this catches `gh` reporting one anyway); `Err` when
/// `api_login` is `Err`, i.e. the probe could not establish an identity at all
/// (fail closed — an unverifiable identity is never a pass); `Err` when the
/// probed login differs from `expected`, naming what the transcript claimed
/// when the two disagree. `Ok(())` only when the probe returned exactly
/// `expected`. The transcript never overrides the probe in either direction.
/// Test: `verify_active_account_matches`, `verify_active_account_mismatch`,
/// `verify_active_account_rejects_env_token_override`,
/// `verify_active_account_follows_the_api_over_the_transcript`,
/// `verify_active_account_fails_closed_when_the_probe_failed`.
pub(crate) fn verify_active_account(
    status_text: &str,
    api_login: Result<&str, &str>,
    expected: &str,
) -> Result<(), String> {
    let lower = status_text.to_lowercase();
    if lower.contains("environment variable")
        && (lower.contains("gh_token") || lower.contains("github_token"))
    {
        return Err(format!(
            "gh is authenticating via a GH_TOKEN/GITHUB_TOKEN environment variable override, \
             so the active account inside this GH_CONFIG_DIR cannot be verified as '{expected}'"
        ));
    }
    let active = api_login.map_err(|why| {
        format!(
            "could not establish which account gh authenticates as \
             (expected '{expected}'): {why}"
        )
    })?;
    if active == expected {
        return Ok(());
    }
    // #5849: name the transcript's claim too when it disagrees — that gap is
    // the #5656 divergence, and it is the operator's next lead.
    let claimed = parse_active_account_from_status(status_text)
        .filter(|c| c != active)
        .map(|c| format!(" (`gh auth status` claims '{c}' is active — see #5656)"))
        .unwrap_or_default();
    Err(format!(
        "gh authenticates as '{active}', expected '{expected}'{claimed}"
    ))
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
/// already the account `gh` authenticates as inside that directory. `Ok(())`
/// means `gh api user` reported `gh_user` (#5849) — not merely that the
/// directory's `hosts.yml` names it.
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

/// Decide WHETHER `#2081` account enforcement applies to a resolved
/// [`GithubConfig`], and extract the `(config_dir, account)` pair to enforce
/// when it does — the single decision point every wired call site (#3312)
/// shares, so the CLI and the daemon can never diverge on when enforcement
/// runs.
///
/// Why: `account` alone documents intent (see `gh_identity`'s module docs and
/// its `account_with_config_dir_is_ok` test) and `config_dir` alone is just an
/// isolation directory with no stated expectation of WHO should be active
/// inside it — enforcement only has both a target (`config_dir`) and an
/// expectation (`account`) to check when a project configures BOTH together.
/// A project that only sets `config_dir` (isolation with no stated
/// preference) or only sets `account` (a hint the `gh_identity` precedence
/// chain already refuses to select alone) must see NO behaviour change: this
/// returns `None` and the caller skips enforcement entirely, never spawning a
/// `gh` subprocess and never failing a project that never opted in.
/// What: `Some((dir, account))` only when `config.config_dir` is set AND
/// `config.account` is set to a non-blank value (trimmed); `None` for a
/// `None` `config`, a blank/whitespace-only `account`, or either field
/// missing.
/// Test: `configured_account_pair_both_set`,
/// `configured_account_pair_config_dir_only`,
/// `configured_account_pair_account_only`,
/// `configured_account_pair_blank_account`, `configured_account_pair_none`.
pub fn configured_account_pair(config: Option<&GithubConfig>) -> Option<(PathBuf, String)> {
    let cfg = config?;
    let dir = cfg.config_dir.clone()?;
    let account = cfg
        .account
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some((dir, account.to_string()))
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

    /// Why: [`verify_active_account`] must succeed when the probed identity is
    /// the expected login.
    /// Test: itself.
    #[test]
    fn verify_active_account_matches() {
        assert!(verify_active_account(SINGLE_AUTH_STATUS, Ok("bobmatnyc"), "bobmatnyc").is_ok());
    }

    /// Why: a mismatched account (the exact #2081 incident shape —
    /// `bob-duetto` authenticated when `bobmatnyc` was expected) must be a loud
    /// `Err`, never a silent pass.
    /// Test: itself.
    #[test]
    fn verify_active_account_mismatch() {
        let err =
            verify_active_account(MULTI_AUTH_STATUS, Ok("bob-duetto"), "bobmatnyc").unwrap_err();
        assert!(err.contains("bob-duetto"), "err: {err}");
        assert!(err.contains("bobmatnyc"), "err: {err}");
    }

    /// Why (#5849): the transcript is not evidence of an identity. When
    /// `hosts.yml` claims the expected account is active but the credential
    /// resolves to someone else, the API answer must decide and the error must
    /// name BOTH logins — that gap is the #5656 divergence and the operator's
    /// next lead.
    /// Test: itself.
    #[test]
    fn verify_active_account_follows_the_api_over_the_transcript() {
        let claims_ghost = "\
github.com
  ✓ Logged in to github.com account nonexistent-user-xyz (keyring)
  - Active account: true
";
        let err = verify_active_account(claims_ghost, Ok("bobmatnyc"), "nonexistent-user-xyz")
            .unwrap_err();
        assert!(err.contains("bobmatnyc"), "err: {err}");
        assert!(err.contains("nonexistent-user-xyz"), "err: {err}");
        assert!(err.contains("5656"), "err: {err}");
    }

    /// Why (#5849): a probe that could not answer — `gh` missing, a non-zero
    /// exit, a timeout — leaves the identity unverified, which must fail
    /// CLOSED even when the transcript looks perfect.
    /// Test: itself.
    #[test]
    fn verify_active_account_fails_closed_when_the_probe_failed() {
        let err = verify_active_account(SINGLE_AUTH_STATUS, Err("gh: not found"), "bobmatnyc")
            .unwrap_err();
        assert!(err.contains("could not establish"), "err: {err}");
        assert!(err.contains("gh: not found"), "err: {err}");
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
        let err = verify_active_account(text, Ok("bobmatnyc"), "bobmatnyc").unwrap_err();
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

    // ── configured_account_pair (#3312) ──────────────────────────────────

    fn github_cfg(config_dir: Option<&str>, account: Option<&str>) -> GithubConfig {
        GithubConfig {
            config_dir: config_dir.map(PathBuf::from),
            token_env: None,
            account: account.map(str::to_string),
            host: None,
        }
    }

    /// Why: the intentional `#2081` pairing — both `config_dir` AND `account`
    /// configured together — is exactly when enforcement must apply.
    /// Test: itself.
    #[test]
    fn configured_account_pair_both_set() {
        let cfg = github_cfg(Some("/cfg/acct"), Some("bobmatnyc"));
        assert_eq!(
            configured_account_pair(Some(&cfg)),
            Some((PathBuf::from("/cfg/acct"), "bobmatnyc".to_string()))
        );
    }

    /// Why: `config_dir` alone states isolation with no expectation of WHICH
    /// account should be active inside it — nothing to enforce, must be
    /// `None` so a project that never opted into `account` sees no new
    /// behaviour.
    /// Test: itself.
    #[test]
    fn configured_account_pair_config_dir_only() {
        let cfg = github_cfg(Some("/cfg/acct"), None);
        assert_eq!(configured_account_pair(Some(&cfg)), None);
    }

    /// Why: `account` alone (no `config_dir`) has no directory to enforce
    /// inside — `gh_identity::resolve_gh_env` already refuses to select an
    /// identity from `account` alone; enforcement must likewise be a no-op.
    /// Test: itself.
    #[test]
    fn configured_account_pair_account_only() {
        let cfg = github_cfg(None, Some("bobmatnyc"));
        assert_eq!(configured_account_pair(Some(&cfg)), None);
    }

    /// Why: a blank/whitespace-only `account` is not a real expectation —
    /// must be treated identically to `None`, never enforced against.
    /// Test: itself.
    #[test]
    fn configured_account_pair_blank_account() {
        let cfg = github_cfg(Some("/cfg/acct"), Some("   "));
        assert_eq!(configured_account_pair(Some(&cfg)), None);
    }

    /// Why: no `github:` binding at all (the common, unconfigured case) must
    /// yield `None` — the sensible default so enforcement never fires for a
    /// project that never configured anything.
    /// Test: itself.
    #[test]
    fn configured_account_pair_none() {
        assert_eq!(configured_account_pair(None), None);
    }

    // ── ensure_gh_account_in_dir against a fake `gh` on PATH (#3312) ──────
    //
    // These prove the enforcement actually runs a verify/correct/re-verify
    // cycle against a real subprocess, not just the pure parsers above. A
    // fake `gh` script (stateful via a file inside the isolated config_dir)
    // stands in for the real binary, following this workspace's established
    // fake-binary-via-PATH test convention (see
    // `trusty-common::update::tests::write_fake_binary`).

    /// Serialises PATH/env mutation across the tests in this section so they
    /// cannot race each other or any other test in the shared test binary.
    fn fake_gh_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::core::trusty_tools_config::env_test_lock()
    }

    /// Write a fake `gh` that tracks the "active account" as a file inside
    /// `config_dir` (mirroring how real `gh` persists active-account state
    /// inside its config home): `auth status` reports whatever the state file
    /// holds (or `initial` if absent); `auth switch --user X` writes `X` to
    /// the state file and exits 0, unless `FAKE_GH_SWITCH_FAILS=1` is set in
    /// the environment, in which case it exits 1 without writing anything.
    ///
    /// `api user --jq .login` (#5849) answers with the same state by default —
    /// the honest machine, where the config dir and the credential agree.
    /// `FAKE_GH_API_LOGIN=X` makes it answer `X` regardless, which is how the
    /// transcript and the credential are made to DISAGREE; `FAKE_GH_API_FAILS=1`
    /// makes it exit 1, standing in for an unreachable/unauthenticated probe.
    #[cfg(unix)]
    fn write_fake_gh(bin_dir: &std::path::Path, initial: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            r#"#!/bin/sh
STATE="$GH_CONFIG_DIR/.fake_active"
if [ -f "$STATE" ]; then
  ACTIVE=$(cat "$STATE")
else
  ACTIVE="{initial}"
fi
if [ "$1" = "api" ] && [ "$2" = "user" ]; then
  if [ "$FAKE_GH_API_FAILS" = "1" ]; then
    echo "fake gh: api refused" >&2
    exit 1
  fi
  if [ -n "$FAKE_GH_API_LOGIN" ]; then
    echo "$FAKE_GH_API_LOGIN"
  else
    echo "$ACTIVE"
  fi
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  echo "github.com"
  echo "  - Logged in to github.com account $ACTIVE (keyring)"
  echo "  - Active account: true"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "switch" ]; then
  if [ "$FAKE_GH_SWITCH_FAILS" = "1" ]; then
    echo "fake gh: switch refused" >&2
    exit 1
  fi
  target=""
  prev=""
  for a in "$@"; do
    if [ "$prev" = "--user" ]; then target="$a"; fi
    prev="$a"
  done
  echo "$target" > "$STATE"
  exit 0
fi
exit 1
"#
        );
        let path = bin_dir.join("gh");
        std::fs::write(&path, script).expect("write fake gh");
        let mut perms = std::fs::metadata(&path)
            .expect("stat fake gh")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake gh");
    }

    /// Prepend `dir` to `PATH`, returning the prior value to restore.
    #[cfg(unix)]
    fn prepend_path(dir: &std::path::Path) -> Option<String> {
        let prior = std::env::var("PATH").ok();
        let new_path = match &prior {
            Some(p) => format!("{}:{p}", dir.display()),
            None => dir.display().to_string(),
        };
        // SAFETY: guarded by `fake_gh_lock` in every caller.
        unsafe { std::env::set_var("PATH", new_path) };
        prior
    }

    /// Reset every `FAKE_GH_*` knob so one test's env cannot leak into the next.
    #[cfg(unix)]
    fn clear_fake_gh_env() {
        // SAFETY: guarded by `fake_gh_lock` in every caller.
        unsafe {
            std::env::remove_var("FAKE_GH_SWITCH_FAILS");
            std::env::remove_var("FAKE_GH_API_LOGIN");
            std::env::remove_var("FAKE_GH_API_FAILS");
        }
    }

    #[cfg(unix)]
    fn restore_path(prior: Option<String>) {
        // SAFETY: guarded by `fake_gh_lock` in every caller.
        unsafe {
            match prior {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    /// Why (#3312): the exact #2081 incident shape — the isolated
    /// `config_dir` itself has the WRONG account active — must be corrected:
    /// `ensure_gh_account_in_dir` detects the mismatch, runs `gh auth switch`,
    /// and re-verifies before returning `Ok`.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn ensure_gh_account_in_dir_self_heals_mismatch() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "wrong-account");
        let prior_path = prepend_path(bin_dir.path());
        clear_fake_gh_env();

        let result = ensure_gh_account_in_dir("bobmatnyc", config_dir.path());

        restore_path(prior_path);
        assert!(result.is_ok(), "expected self-heal to succeed: {result:?}");
        let state = std::fs::read_to_string(config_dir.path().join(".fake_active"))
            .expect("state file written by switch");
        assert_eq!(state.trim(), "bobmatnyc");
    }

    /// Why (#3312): when the isolated directory has the wrong account active
    /// AND the switch itself fails (e.g. the expected account was never
    /// logged in under that `GH_CONFIG_DIR`), enforcement must hard-fail —
    /// never silently proceed with the wrong identity active.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn ensure_gh_account_in_dir_switch_failure_is_err() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "wrong-account");
        let prior_path = prepend_path(bin_dir.path());
        clear_fake_gh_env();
        // SAFETY: guarded by fake_gh_lock; cleared below.
        unsafe { std::env::set_var("FAKE_GH_SWITCH_FAILS", "1") };

        let result = ensure_gh_account_in_dir("bobmatnyc", config_dir.path());

        clear_fake_gh_env();
        restore_path(prior_path);
        let err = result.expect_err("switch failure must be a hard error");
        assert!(err.to_string().contains("bobmatnyc"), "err: {err}");
    }

    /// Why (#3312): when the isolated directory's active account ALREADY
    /// matches, `ensure_gh_account_in_dir` must be a pure no-op — it must not
    /// even attempt a switch. Proven here by making a switch attempt fail
    /// (`FAKE_GH_SWITCH_FAILS=1`) and asserting `Ok` is still returned, which
    /// is only possible if no switch was ever invoked.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn ensure_gh_account_in_dir_noop_when_already_active() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "bobmatnyc");
        let prior_path = prepend_path(bin_dir.path());
        clear_fake_gh_env();
        // SAFETY: guarded by fake_gh_lock; cleared below. A switch attempt
        // would fail if (incorrectly) invoked, proving the no-op path.
        unsafe { std::env::set_var("FAKE_GH_SWITCH_FAILS", "1") };

        let result = ensure_gh_account_in_dir("bobmatnyc", config_dir.path());

        clear_fake_gh_env();
        restore_path(prior_path);
        assert!(
            result.is_ok(),
            "already-active must be a no-op Ok: {result:?}"
        );
        assert!(
            !config_dir.path().join(".fake_active").exists(),
            "no switch should have been attempted"
        );
    }

    // ── the API answer, not the transcript, decides (#5849) ──────────────

    /// Why (#5849): the reported bug. A `hosts.yml` naming an account that
    /// exists nowhere on the machine still prints `Active account: true`, and
    /// enforcement used to pass on that text alone. Here the transcript claims
    /// `nonexistent-user-xyz` — exactly what is expected — while the credential
    /// resolves to `bobmatnyc`, and enforcement must REFUSE, naming both
    /// logins. Before this fix the same fixture returned `Ok(())`.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn ensure_gh_account_in_dir_rejects_a_transcript_the_api_contradicts() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "nonexistent-user-xyz");
        let prior_path = prepend_path(bin_dir.path());
        clear_fake_gh_env();
        // SAFETY: guarded by fake_gh_lock; cleared below.
        unsafe { std::env::set_var("FAKE_GH_API_LOGIN", "bobmatnyc") };

        let result = ensure_gh_account_in_dir("nonexistent-user-xyz", config_dir.path());

        clear_fake_gh_env();
        restore_path(prior_path);
        let err = result.expect_err("a transcript the credential contradicts must not pass");
        let msg = err.to_string();
        assert!(msg.contains("bobmatnyc"), "err: {msg}");
        assert!(msg.contains("nonexistent-user-xyz"), "err: {msg}");
    }

    /// Why (#5849): a probe that cannot answer leaves the identity unverified,
    /// which must fail CLOSED — including when the transcript looks perfect
    /// and names exactly the expected account.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn ensure_gh_account_in_dir_fails_closed_when_the_api_probe_fails() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "bobmatnyc");
        let prior_path = prepend_path(bin_dir.path());
        clear_fake_gh_env();
        // SAFETY: guarded by fake_gh_lock; cleared below.
        unsafe { std::env::set_var("FAKE_GH_API_FAILS", "1") };

        let result = ensure_gh_account_in_dir("bobmatnyc", config_dir.path());

        clear_fake_gh_env();
        restore_path(prior_path);
        let err = result.expect_err("an unverifiable identity must never pass");
        assert!(
            err.to_string().contains("could not establish"),
            "err: {err}"
        );
    }

    /// Why (#5849): the API answer is authoritative in BOTH directions. A
    /// stale transcript naming the wrong account must not force a switch when
    /// the credential already authenticates as the expected user. Proven by
    /// making a switch attempt fail: `Ok` is only reachable without one.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn ensure_gh_account_in_dir_accepts_the_api_answer_over_a_stale_transcript() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "stale-account");
        let prior_path = prepend_path(bin_dir.path());
        clear_fake_gh_env();
        // SAFETY: guarded by fake_gh_lock; cleared below.
        unsafe {
            std::env::set_var("FAKE_GH_API_LOGIN", "bobmatnyc");
            std::env::set_var("FAKE_GH_SWITCH_FAILS", "1");
        }

        let result = ensure_gh_account_in_dir("bobmatnyc", config_dir.path());

        clear_fake_gh_env();
        restore_path(prior_path);
        assert!(
            result.is_ok(),
            "the credential already matches; no switch is owed: {result:?}"
        );
        assert!(
            !config_dir.path().join(".fake_active").exists(),
            "no switch should have been attempted"
        );
    }
}
