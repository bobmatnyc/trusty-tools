//! Account-selected clone credentials for the daemon's `inproject` base clone (#7166).
//!
//! Why: split out of `inproject.rs` (issue #7166 review follow-up) to keep that
//! file under the 500-SLOC production cap — the token-resolution,
//! identity-verification, and shadow-then-set env logic this module holds is a
//! self-contained concern with its own doc, its own tests, and only one call
//! site (`ensure_base_clone`) in the parent.
//! What: [`account_clone_env`] is the `pub(super)` entry point `ensure_base_clone`
//! calls; everything else here is private to this module (or `pub(super)` only
//! where the sibling `tests` module — a descendant of `inproject`, and so still
//! able to see anything `pub(super)` here exposes — needs direct access for
//! pure-value assertions).
//! Test: `inproject/tests.rs` — see each item's own doc for the exact test names.

/// The env overrides an account-selected clone applies to the `git` child (#7166).
///
/// Why: split out of [`ensure_base_clone`] so the shadow-then-set shape can
/// be asserted directly against a plain `std::process::Command`, without
/// spawning a real `git clone` — mirrors how [`crate::core::gh_identity::
/// GhEnv::apply_to`] is unit-tested at the pure-value layer.
/// What: `remove` — every [`crate::core::gh_identity::
/// GH_INHERITED_IDENTITY_ENV`] entry, so an exported `GH_TOKEN`/`GITHUB_TOKEN`
/// (or a `GH_CONFIG_DIR` naming a different identity) cannot outrank the
/// selection; `set` — whatever [`crate::core::gh_account::GhSpawnEnv::vars`]
/// resolved (`GH_CONFIG_DIR`[+`GH_USER`], or `GH_TOKEN`+`GH_USER`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AccountCloneEnv {
    remove: Vec<&'static str>,
    set: Vec<(String, String)>,
}

impl AccountCloneEnv {
    /// Apply `remove` then `set` to `cmd` — removal MUST precede the set, the
    /// same ordering [`crate::core::gh_identity::GhEnv::apply_to`] requires.
    pub(super) fn apply(&self, cmd: &mut std::process::Command) {
        for key in &self.remove {
            cmd.env_remove(key);
        }
        for (key, value) in &self.set {
            cmd.env(key, value);
        }
    }
}

/// Resolve the [`AccountCloneEnv`] for `account` (#7166, tightened after a
/// `code-critic` BLOCK on the initial version).
///
/// Why: the one place this module decides HOW an explicit account selection
/// becomes credentials for a raw `git clone` subprocess — as opposed to a
/// `gh` subcommand, which [`trusty_common::gh::GhCommand`] already handles.
/// `git clone` never goes through `gh`, so the token has to land where
/// `git`'s own already-configured credential resolution (this workspace's
/// convention: `credential.helper = !gh auth git-credential`) will find
/// it — the environment, never argv or the URL.
///
/// 🔴 The first version of this function called
/// [`crate::core::gh_account::gh_token_via_cli`] (`gh auth token -u
/// <account>`) directly and unconditionally. That selector is documented, in
/// this same crate, as NOT discriminating between logged-in accounts on a
/// keyring-backed `gh` install (macOS's default, and the exact `#7166`
/// scenario) — `-u bobmatnyc` and `-u bob-duetto` can return the identical
/// value, so the clone would silently authenticate as whichever account
/// happened to be globally active, not the one requested. Two changes close
/// that:
///
/// 1. Token resolution now runs through
///    [`crate::core::gh_account::resolve_gh_account_env`] — the SAME
///    config_dir-first, `-u`-fallback precedence
///    [`crate::daemon::managed_routes::lifecycle::resolve_gh_env`] already
///    uses for the session's own spawn-time `GH_TOKEN`, rather than a second,
///    weaker call path. This cold-start clone has no already-registered
///    project to read a pinned `github.config_dir` from (the project does
///    not exist in the registry yet), so `config_dir` is always `None` here
///    today — the `-u` fallback is what actually runs — but routing through
///    the shared resolver rather than calling the demoted selector directly
///    means a future config_dir-aware caller costs nothing to add, and this
///    call can never independently drift from the other two `gh_token_via_cli`
///    call sites' precedence.
/// 2. [`verify_token_login`] is run against the resolved token BEFORE it is ever
///    handed to `git`, and a mismatch is a loud, named refusal — this is what
///    actually closes the vulnerability for the always-taken `-u` arm above.
///    The config_dir arm is skipped: it selects by an isolated `gh` config
///    home, which discriminates by construction (`gh_account.rs`'s own
///    rationale for demoting `-u`), so there is no bare token to verify.
///
/// What: `Err` on an unresolvable/failed identity (already names the account;
/// see [`crate::core::gh_account::gh_token_via_cli`]'s doc) or on a
/// login mismatch (names BOTH the requested and the actual login). `Ok`
/// returns the shadow-then-set pair described on [`AccountCloneEnv`].
/// Test: `account_clone_env_shadows_inherited_identity_and_sets_gh_token`,
/// `account_clone_env_propagates_a_resolver_failure_naming_the_account`,
/// `account_clone_env_refuses_a_token_minted_for_a_different_account`,
/// `account_clone_env_propagates_a_verification_failure`,
/// `account_clone_env_accepts_a_token_that_matches`.
pub(super) fn account_clone_env(account: &str) -> Result<AccountCloneEnv, String> {
    account_clone_env_with(
        account,
        crate::core::gh_account::gh_token_via_cli,
        verify_token_login,
    )
}

/// [`account_clone_env`] with injectable token-resolution and
/// identity-verification steps.
///
/// Why: two closures — this codebase's established seam for `gh` I/O that
/// cannot run hermetically in CI (see `gh_account::resolve_gh_account_env_with`,
/// which `resolve_token` mirrors) — let the shadow-then-set shape, the
/// not-logged-in refusal, AND the mismatch refusal all be asserted with no
/// real `gh` process, no PATH mutation, and no network.
/// What: delegates identity SELECTION to
/// [`crate::core::gh_account::resolve_gh_account_env_with`] (`config_dir:
/// None` — see [`account_clone_env`]'s doc for why); when the resolved
/// [`crate::core::gh_account::GhSpawnEnv::vars`] carries a `GH_TOKEN` (the
/// `-u`-fallback arm — the config_dir arm never sets one), `verify_login` is
/// called with that token and its result compared, case-insensitively
/// (GitHub logins are case-insensitive; matches
/// [`crate::core::gh_account::GhAccountStatus::canonical_logged_in_login`]'s
/// convention), against `account`. A `resolve_gh_account_env_with` `warning`
/// (a pinned-but-credential-less `config_dir`) is logged, not swallowed.
/// Test: `account_clone_env_shadows_inherited_identity_and_sets_gh_token`,
/// `account_clone_env_propagates_a_resolver_failure_naming_the_account`,
/// `account_clone_env_refuses_a_token_minted_for_a_different_account`,
/// `account_clone_env_propagates_a_verification_failure`,
/// `account_clone_env_accepts_a_token_that_matches`.
fn account_clone_env_with(
    account: &str,
    resolve_token: impl FnOnce(&str) -> Result<String, String>,
    verify_login: impl FnOnce(&str) -> Result<String, String>,
) -> Result<AccountCloneEnv, String> {
    // `config_dir: None` — see the doc above for why this call site never has
    // one to offer yet. `account` is always `Some`, so `resolve_gh_account_env_with`
    // never returns its `None` ("nothing to inject") outcome here.
    let spawn_env =
        crate::core::gh_account::resolve_gh_account_env_with(Some(account), None, resolve_token)
            .ok_or_else(|| {
            format!(
                "internal error: resolve_gh_account_env_with returned None for account '{account}'"
            )
        })??;

    if let Some(warning) = &spawn_env.warning {
        tracing::warn!("{warning}");
    }

    // #7166 critic BLOCK: only the `-u`-fallback arm sets a bare `GH_TOKEN` —
    // the config_dir arm discriminates by construction and carries none, so
    // there is nothing to verify there.
    if let Some((_, token)) = spawn_env.vars.iter().find(|(k, _)| k == "GH_TOKEN") {
        let actual_login = verify_login(token).map_err(|e| {
            format!("could not verify the minted token for account '{account}': {e}")
        })?;
        if !actual_login.eq_ignore_ascii_case(account) {
            return Err(format!(
                "gh minted a token for '{actual_login}', not the requested account \
                 '{account}' — `gh auth token -u` does not discriminate between logged-in \
                 accounts on a keyring-backed host. Pin a scoped `github.config_dir` for \
                 this project (`tm projects register --gh-account {account} --gh-config-dir \
                 <dir>`) to select reliably, or make '{account}' the machine-global active \
                 account with `gh auth switch --user {account}`."
            ));
        }
    }

    Ok(AccountCloneEnv {
        remove: clone_env_removal_set(),
        set: spawn_env.vars,
    })
}

/// The full removal set for an account-selected clone's child env (#7166
/// review follow-up).
///
/// Why: [`crate::core::gh_identity::GH_INHERITED_IDENTITY_ENV`] deliberately
/// does NOT include `GH_HOST` — two existing consumers
/// ([`crate::core::gh_identity::resolve_gh_env`]'s `inherited_identity_to_clear`
/// and, transitively, `runtime::claude_code_gh_env`) have an existing test
/// (`binding_removes_the_inherited_identity_vars`) pinning an EXACT unset
/// list, so widening the shared const would be a silent behavior change for
/// both of them, not a scoped fix for this one. This clone path is
/// different: the clone URL's host is hardcoded to `github.com`
/// (`register_args::GITHUB_HOST`) — never read from config — so an ambient
/// `GH_HOST` here can only ever be a stale mismatch (e.g. a GHE host left
/// over from a previous invocation) that would misdirect `gh auth token -u`
/// / `gh api user` while `git clone` still targets `github.com`. Removing it
/// LOCALLY, for this one child env only, closes that without touching the
/// shared const or its consumers' behavior.
/// What: [`crate::core::gh_identity::GH_INHERITED_IDENTITY_ENV`] plus
/// `"GH_HOST"`.
/// Test: `account_clone_env_shadows_inherited_identity_and_sets_gh_token`.
fn clone_env_removal_set() -> Vec<&'static str> {
    let mut set = crate::core::gh_identity::GH_INHERITED_IDENTITY_ENV.to_vec();
    set.push("GH_HOST");
    set
}

/// Confirm a minted token actually belongs to the account it claims to,
/// before it is ever used to authenticate a clone (#7166 critic BLOCK).
///
/// Why: `gh auth token -u <account>` can return a DIFFERENT account's token
/// on a keyring-backed host (`gh_account.rs`'s own documented caveat) — this
/// is the check that turns "silently wrong identity" into a loud, named
/// refusal, by asking GitHub itself who the token belongs to.
/// What: runs `gh api user --jq .login`, bounded by
/// [`crate::core::gh_account::GH_ENFORCE_TIMEOUT`] (see
/// [`verify_token_login_with`]), with EVERY inherited identity var removed
/// and `token` set as `GH_TOKEN` — so the answer reflects `token` alone,
/// never an ambient credential — and returns the trimmed login on a
/// non-empty, zero-exit response.
/// Test: the `gh`-invoking closure itself (a real, deliberate network call)
/// has no pure branch left to unit test hermetically; the timeout wrapper it
/// delegates to is covered directly — see [`verify_token_login_with`].
fn verify_token_login(token: &str) -> Result<String, String> {
    let token = token.to_string();
    verify_token_login_with(crate::core::gh_account::GH_ENFORCE_TIMEOUT, move || {
        let mut cmd = trusty_common::gh::GhCommand::new(["api", "user", "--jq", ".login"]);
        // Includes `GH_HOST` — see `clone_env_removal_set`'s doc — so a stale
        // ambient GHE host cannot make this verify against the wrong host
        // while the clone itself still targets `github.com`.
        for key in clone_env_removal_set() {
            cmd = cmd.env_remove(key);
        }
        // `.env_remove(k)` then `.env(k, v)` on the SAME key leaves it SET —
        // see `GhCommand::env`'s "later call wins" doc — so `GH_TOKEN` here
        // is shadowed by `token` regardless of removal order.
        cmd = cmd.env("GH_TOKEN", &token);
        cmd.nonempty_stdout_blocking()
            .map_err(|e| format!("`gh api user` failed: {e}"))
    })
}

/// [`verify_token_login`] with an explicit, injectable timeout and runner
/// (#7166 review follow-up HIGH).
///
/// Why: the FIRST version of this function called
/// `GhCommand::nonempty_stdout_blocking()` with no bound at all — a network
/// stall (or a hung `gh`) blocked the whole clone indefinitely, unlike every
/// other `gh`-subprocess call this crate makes for account selection
/// ([`crate::core::gh_account::gh_token_via_cli`], `gh_account_enforce`'s
/// `api_login_scoped`), which all run under
/// [`crate::core::gh_account::GH_ENFORCE_TIMEOUT`] via
/// [`crate::core::gh_account::run_bounded`]. Splitting the timeout wrapper
/// from the `gh`-invoking closure — the SAME seam
/// [`crate::core::gh_account::probe_gh_auth_with`] uses for its own
/// hermetically-tested timeout arm — lets the timeout branch be asserted
/// with a `std::thread::sleep`, no live `gh`, no network, no PATH mutation.
/// What: runs `run` on `run_bounded`'s detached thread; a stall past
/// `timeout` returns `Err` naming the timeout explicitly (distinct from a
/// `run` failure, which propagates verbatim) — [`account_clone_env_with`]
/// wraps whichever `Err` reaches it with the account name, so either shape
/// still names the account by the time it surfaces.
/// Test: `verify_token_login_with_times_out_and_names_the_bound`,
/// `verify_token_login_with_returns_the_inner_result_when_fast`.
fn verify_token_login_with<F>(timeout: std::time::Duration, run: F) -> Result<String, String>
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
{
    crate::core::gh_account::run_bounded(timeout, move || Some(run())).unwrap_or_else(|| {
        Err(format!(
            "`gh api user` verification timed out after {timeout:?}"
        ))
    })
}

#[cfg(test)]
#[path = "account_clone_tests.rs"]
mod tests;
