//! Hand a launched session a `github.token_env` binding through the #9750 gh
//! shim, never as a `GH_TOKEN` value (#10487).
//!
//! Why: a process environment is readable by every same-uid process (`ps -E`),
//! so a `GH_TOKEN` copied from `github.token_env` into a launched `claude` is
//! readable by every hook, MCP server and tool child. The #9750 shim already
//! resolves a pinned account's token per gh call from the keyring; this routes
//! the `token_env` binding through it (Architect ruling, 2026-11-10 21:48Z).
//! What: [`shim_token_env_with`] proves the binding's token with `GET /user`,
//! stores it in gh's keyring entry for that login under tm's per-account dir
//! ([`crate::core::gh_session_account::store_proven_token`], #10037), then
//! verifies the keyring answer and writes the shim
//! ([`crate::core::gh_session_shim::launch_pinned_login`]). The session env
//! names the shim and clears `GH_TOKEN`, its spellings and the `token_env`
//! variable itself. Every failure is an `Err`; nothing falls back to a token
//! in the env.
//! Test: `gh_token_env_shim_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::gh_account::GH_USER_ENV_VAR;
use crate::core::gh_account_dir::{AccountDirSources, tm_account_dir};
use crate::core::gh_account_proof::{GhTokenProbe, GhUserCheck, api_base_url, normalize_gh_host};
use crate::core::gh_identity::GhEnv;
use crate::core::gh_keyring_token::GhKeyringWriter;
use crate::core::gh_session_shim::LaunchSeams;
use crate::core::trusty_tools_config::DEFAULT_GITHUB_HOST;

/// The token variable a `token_env` binding sets.
const GH_TOKEN: &str = "GH_TOKEN";
/// The host variable a binding may set.
const GH_HOST: &str = "GH_HOST";

/// What [`shim_token_env_with`] needs to prove, store and shim a token.
pub(crate) struct TokenEnvSeams<'a> {
    /// Asks GitHub whose token it is.
    pub(crate) check: &'a dyn GhUserCheck,
    /// Asks gh for a stored token (the launch's keyring read-back).
    pub(crate) probe: &'a dyn GhTokenProbe,
    /// Writes gh's keyring entry.
    pub(crate) keyring: &'a dyn GhKeyringWriter,
    /// The real gh, or why there is none.
    pub(crate) real_gh: Result<PathBuf, String>,
    /// tm's state root: the account dirs and the shims.
    pub(crate) state_root: &'a Path,
}

/// `env` as a launched session must receive it: a `GH_TOKEN` it carries goes
/// through the gh shim (#10487).
///
/// Why: see the module docs. A one-shot `tm` gh call keeps `env` as it is;
/// only a session launch calls this.
/// What: an `env` with no `GH_TOKEN`, or one not from `github.token_env`
/// ([`GhEnv::token_source`] is `None`, e.g. [`GhEnv::local_only`]), is
/// returned unchanged. Otherwise the host
/// is `GH_HOST`, else `github.com`; `GET /user` names the login; the token is
/// stored for that login ([`crate::core::gh_session_account::store_proven_token`]);
/// [`crate::core::gh_session_shim::launch_pinned_login`] reads it back and
/// writes the shim. The result sets the shim dir, `GH_USER`, the git
/// credential pin to the shim and any `GH_HOST`, sets no token variable, and
/// clears every inherited token spelling plus [`GhEnv::token_source`]. Each
/// failure is an `Err` naming the step, with the token redacted.
/// Test: `a_token_env_binding_reaches_the_launch_env_as_the_shim_not_a_token_10487`,
/// `a_binding_with_no_token_passes_through_unchanged_10487`,
/// `a_local_only_env_passes_through_with_no_get_user_10487`,
/// `a_token_env_keyring_failure_refuses_and_injects_no_token_10487`,
/// `an_unproven_token_env_token_refuses_and_stores_nothing_10487`,
/// `a_token_env_launch_with_no_gh_refuses_10487`.
pub(crate) fn shim_token_env_with(env: &GhEnv, seams: TokenEnvSeams<'_>) -> Result<GhEnv, String> {
    // #10487 review: only a `github.token_env` binding is shimmed; the
    // local-only clamp's empty `GH_TOKEN` must make no network call.
    let (Some(token), Some(_)) = (env.var(GH_TOKEN), env.token_source()) else {
        return Ok(env.clone());
    };
    let refuse = |step: &str, reason: String| {
        // #10487: a reason quoting the token must not carry it into a message.
        let reason = if token.is_empty() {
            reason
        } else {
            reason.replace(token, "<redacted>")
        };
        format!(
            "tm: cannot hand this project's github.token_env token to the session's gh shim: \
             {step}: {reason}. No token was injected into the session's environment."
        )
    };
    let host = normalize_gh_host(env.var(GH_HOST).unwrap_or(DEFAULT_GITHUB_HOST))
        .map_err(|e| refuse("the gh host", e))?;
    let login = seams
        .check
        .login(&api_base_url(&host), token)
        .map_err(|e| refuse("GET /user", e))?;
    let dir = tm_account_dir(seams.state_root, &login).map_err(|e| refuse("the account dir", e))?;
    crate::core::gh_session_account::store_proven_token(&dir, &host, &login, token, seams.keyring)
        .map_err(|e| refuse("the gh keyring", e))?;
    let sources = AccountDirSources {
        static_config_dir: None,
        state_root: Some(seams.state_root.to_path_buf()),
        own_config_dir: None,
    };
    let cred = crate::core::gh_session_shim::launch_pinned_login(
        &login,
        &host,
        LaunchSeams {
            sources: &sources,
            probe: seams.probe,
            check: seams.check,
            real_gh: seams.real_gh,
            state_root: seams.state_root,
        },
    )
    .map_err(|e| refuse("the gh shim", e))?;
    let mut vars = cred.vars;
    vars.push((GH_USER_ENV_VAR.to_string(), login));
    vars.extend(crate::core::gh_session_account::git_credential_pin(
        &host,
        &cred.helper,
    ));
    if let Some(bound) = env.var(GH_HOST) {
        vars.push((GH_HOST.to_string(), bound.to_string()));
    }
    let shimmed = GhEnv::from_identity_vars(vars);
    Ok(match env.token_source() {
        Some(name) => shimmed.clearing(name),
        None => shimmed,
    })
}

/// Production entry point: [`shim_token_env_with`] with the real `GET /user`,
/// gh, keyring, this process's `PATH` and tm's state root (#10487).
///
/// What: blocking (HTTPS, gh and keyring calls). A unit-test build refuses at
/// `GET /user`, so it never touches a real keyring.
/// Test: none directly (real gh, keyring and network); the decisions are
/// [`shim_token_env_with`]'s.
pub fn shim_token_env(env: &GhEnv) -> Result<GhEnv, String> {
    use crate::core::gh_account_proof::{CliTokenProbe, HttpUserCheck};
    use crate::core::gh_session_shim::{find_real_gh, shim_root};
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    let path = std::env::var_os("PATH");
    shim_token_env_with(
        env,
        TokenEnvSeams {
            check: &HttpUserCheck,
            probe: &CliTokenProbe,
            keyring: &crate::core::gh_keyring_token::SystemGhKeyring,
            real_gh: find_real_gh(path.as_deref(), &shim_root(&state_root)),
            state_root: &state_root,
        },
    )
}

#[cfg(test)]
#[path = "gh_token_env_shim_tests.rs"]
mod gh_token_env_shim_tests;
