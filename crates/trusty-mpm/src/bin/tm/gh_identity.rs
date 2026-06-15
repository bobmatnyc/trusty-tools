//! Per-project GitHub identity resolution for every `gh` subprocess `tm` spawns.
//!
//! Why: a host may carry several GitHub identities — a personal account, a work
//! account, a bot, or a GitHub Enterprise host — yet `tm`'s many `gh` calls
//! (issue list/view/comment, label/assignee edits, repo view, clone-URL
//! synthesis, managed spawn) must use the RIGHT one for the active project, not
//! whichever identity happens to be ambient. #1265 binds that identity per
//! project via the `github:` config section. This module turns the declarative
//! [`GithubConfig`] into the concrete set of environment overrides applied to
//! each `gh` `Command` before spawn, honouring a precedence chain and — for the
//! `config_dir`/`token_env` strategies — WITHOUT mutating the user's global gh
//! state.
//!
//! What: [`GhEnv`] is the resolved, ordered list of `(VAR, value)` overrides;
//! [`resolve_gh_env`] applies the precedence **`config_dir` > `token_env` >
//! `account`** for identity selection and ALWAYS applies `host` (as `GH_HOST`)
//! when set; [`clone_url`] synthesises an HTTPS clone URL honouring the host.
//! [`GhIdentityError`] is the typed failure (today only the `account`-strategy
//! refusal). An absent `github:` config resolves to an EMPTY [`GhEnv`], so gh
//! inherits the ambient identity (no regression).
//!
//! ## Precedence chain (identity)
//!
//! 1. `config_dir` → `GH_CONFIG_DIR=<dir>`. Highest precedence and least
//!    invasive: gh reads its entire auth/config from a private directory, fully
//!    isolating the identity without touching `~/.config/gh` or global state.
//! 2. else `token_env` (when the named env var is PRESENT) → `GH_TOKEN=<value>`.
//!    The config stores only the env-var NAME; the secret is resolved from the
//!    environment at call time and never persisted. If the named var is absent,
//!    this strategy is skipped and precedence falls through.
//! 3. else `account` → see the caveat below.
//!
//! `host` is applied independently of the identity strategy whenever set.
//!
//! ## `account`-strategy caveat (deliberate decision)
//!
//! `gh` has NO universal per-invocation `--user`/`--account` flag, and
//! `gh auth switch` mutates GLOBAL state (it rewrites the active account in the
//! shared `~/.config/gh/hosts.yml`). Silently switching the user's global gh
//! account as a side effect of a `tm` command — and racing every other `gh`
//! consumer on the box — violates the "do not mutate global state" requirement.
//! Rather than do that, when `account` is the ONLY identity field set we return
//! a typed [`GhIdentityError::AccountStrategyUnsupported`] that instructs the
//! operator to bind via `config_dir` (a per-account gh config home, the gh
//! convention for multiple accounts) or `token_env` instead. `account` is still
//! accepted in config (it documents intent and pairs naturally with a
//! `config_dir`), but on its own it cannot safely select an identity. This keeps
//! every supported strategy side-effect-free on global gh state.
//!
//! Test: `resolve_*`, `precedence_*`, `host_*`, `account_*`, `clone_url_*` in
//! the inline `tests` module.

use trusty_mpm::core::trusty_tools_config::{DEFAULT_GITHUB_HOST, GithubConfig};

/// `GH_CONFIG_DIR` — points gh at a private config/auth home.
const ENV_GH_CONFIG_DIR: &str = "GH_CONFIG_DIR";
/// `GH_TOKEN` — the auth token gh uses for API calls.
const ENV_GH_TOKEN: &str = "GH_TOKEN";
/// `GH_HOST` — the default host gh targets.
const ENV_GH_HOST: &str = "GH_HOST";

/// Typed failures from resolving a [`GithubConfig`] into a [`GhEnv`].
///
/// Why: the `account`-only strategy cannot be honoured without mutating global
/// gh state (see the module docs); a typed error lets the caller surface an
/// actionable message and lets tests assert the refusal precisely instead of
/// string-matching an `anyhow` chain.
/// What: today the single variant flags the unsupported `account`-only case,
/// carrying the offending account name for the message.
/// Test: `account_only_is_refused`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum GhIdentityError {
    /// `account` was the only identity field set; selecting an account alone
    /// requires mutating global gh state, which is refused.
    #[error(
        "github.account = '{0}' cannot select a gh identity on its own without \
         mutating global gh state (`gh auth switch`). Bind this project's \
         identity with `github.config_dir` (a per-account gh config home) or \
         `github.token_env` (the NAME of an env var holding a token) instead."
    )]
    AccountStrategyUnsupported(String),
}

/// The resolved set of environment overrides to apply to a `gh` subprocess.
///
/// Why: callers (the `CommandRunner` boundary) need ONE value that lists exactly
/// which env vars to set on a `gh` `Command`, derived once from the active
/// project's config, so every `gh` invocation is bound identically and no call
/// site re-implements the precedence. An EMPTY `GhEnv` means "apply nothing" —
/// the ambient gh identity is used, preserving pre-#1265 behaviour.
/// What: an ordered list of `(name, value)` pairs. Order is deterministic
/// (identity var first when present, then `GH_HOST`) so tests can assert it.
/// Test: every `resolve_*`/`precedence_*`/`host_*` test inspects `vars()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GhEnv {
    vars: Vec<(String, String)>,
}

impl GhEnv {
    /// Borrow the resolved `(name, value)` overrides.
    ///
    /// Why: the `CommandRunner` boundary iterates these to call `Command::env`;
    /// tests iterate them to assert the resolved set and ordering.
    /// What: returns the override slice (possibly empty).
    /// Test: read by every resolution test.
    pub(crate) fn vars(&self) -> &[(String, String)] {
        &self.vars
    }

    /// Whether any override is set (i.e. an identity binding is active).
    ///
    /// Why: lets callers cheaply distinguish "bound" from "ambient" without
    /// allocating; useful for diagnostics.
    /// What: true when at least one override is present.
    /// Test: `resolve_absent_config_is_empty` asserts `is_empty()` for None.
    pub(crate) fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

/// Resolve an optional [`GithubConfig`] into the [`GhEnv`] for `gh` subprocesses.
///
/// Why: the single, tested place the #1265 precedence chain is applied so every
/// `gh` call `tm` makes is bound to the same per-project identity and no call
/// site diverges. Keeping `token_env` resolution here (via `std::env::var` on
/// the configured NAME) guarantees the plaintext secret never has to live in the
/// config or be threaded through the call graph.
/// What: returns an EMPTY `GhEnv` when `config` is `None`. Otherwise applies
/// **`config_dir` > `token_env`(present) > `account`** for the identity var and
/// ALWAYS appends `GH_HOST` when `host` is set. The `account`-only case returns
/// [`GhIdentityError::AccountStrategyUnsupported`] (see module docs). A
/// `token_env` whose named var is ABSENT is skipped, falling through to the next
/// strategy.
/// Test: `resolve_absent_config_is_empty`, `resolve_config_dir`,
/// `resolve_token_env_present`, `resolve_token_env_absent_falls_through`,
/// `precedence_config_dir_beats_token_env`,
/// `precedence_token_env_beats_account`, `account_only_is_refused`,
/// `host_always_applied`, `host_applied_with_identity`.
pub(crate) fn resolve_gh_env(config: Option<&GithubConfig>) -> Result<GhEnv, GhIdentityError> {
    let Some(cfg) = config else {
        return Ok(GhEnv::default());
    };

    let mut vars: Vec<(String, String)> = Vec::new();

    // --- Identity selection (precedence: config_dir > token_env > account) ---
    if let Some(dir) = cfg
        .config_dir
        .as_deref()
        .map(|p| p.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
    {
        vars.push((ENV_GH_CONFIG_DIR.to_string(), dir));
    } else if let Some(token) = resolve_token_env(cfg.token_env.as_deref()) {
        vars.push((ENV_GH_TOKEN.to_string(), token));
    } else if let Some(account) = cfg
        .account
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // `account` alone cannot safely select an identity (see module docs).
        return Err(GhIdentityError::AccountStrategyUnsupported(
            account.to_string(),
        ));
    }

    // --- Host is applied independently of the identity strategy. ------------
    if let Some(host) = cfg.host.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        vars.push((ENV_GH_HOST.to_string(), host.to_string()));
    }

    Ok(GhEnv { vars })
}

/// Resolve the token NAMEd by `token_env` from the process environment.
///
/// Why: isolates the `std::env::var` lookup so the precedence logic stays
/// readable and so the "named var absent → skip" rule is a single tested
/// function. The plaintext token is read here and nowhere else.
/// What: given `Some(name)`, returns `Some(value)` when the env var `name` is
/// set to a non-empty value; returns `None` for an unset/empty var or a `None`
/// name (trimming whitespace around the configured name).
/// Test: covered via `resolve_token_env_present` /
/// `resolve_token_env_absent_falls_through`.
fn resolve_token_env(token_env: Option<&str>) -> Option<String> {
    let name = token_env.map(str::trim).filter(|s| !s.is_empty())?;
    let value = std::env::var(name).ok()?;
    if value.is_empty() { None } else { Some(value) }
}

/// Resolve the effective gh host, honouring config with a `github.com` default.
///
/// Why: both `GH_HOST` and clone-URL synthesis need the same host answer; a
/// single resolver keeps them aligned and applies the default in one place.
/// What: returns the trimmed `config.host` when set and non-empty, else
/// [`DEFAULT_GITHUB_HOST`].
/// Test: `effective_host_default`, `effective_host_override`.
pub(crate) fn effective_host(config: Option<&GithubConfig>) -> String {
    config
        .and_then(|c| c.host.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_GITHUB_HOST)
        .to_string()
}

/// Synthesise an HTTPS clone URL for `owner/repo`, honouring the configured host.
///
/// Why: watch/ticket synthesise a clone URL for a board that may live on GitHub
/// Enterprise; hard-coding `github.com` breaks GHE (#1261). Routing through the
/// resolved host fixes that while defaulting to the public host.
/// What: returns `https://<effective-host>/<repo>` where `repo` is the
/// `owner/repo` slug and the host comes from [`effective_host`].
/// Test: `clone_url_default_host`, `clone_url_enterprise_host`.
pub(crate) fn clone_url(config: Option<&GithubConfig>, repo: &str) -> String {
    format!("https://{}/{}", effective_host(config), repo)
}

/// Convenience: resolve the [`GhEnv`] from an optional config, or surface the
/// typed error as an `anyhow` error for binary call sites.
///
/// Why: production call sites use `anyhow::Result`; folding the
/// [`GhIdentityError`] into `anyhow` here keeps those sites a one-liner while the
/// typed error stays available for tests.
/// What: delegates to [`resolve_gh_env`] and maps the error into `anyhow`.
/// Test: success path covered by the `resolve_*` tests; the error mapping is
/// thin glue exercised by `account_only_is_refused` on the typed function.
pub(crate) fn resolve_gh_env_anyhow(config: Option<&GithubConfig>) -> anyhow::Result<GhEnv> {
    resolve_gh_env(config).map_err(|e| anyhow::anyhow!(e))
}

/// Convenience wrapper: load trusty-mpm config and resolve the active [`GhEnv`].
///
/// Why: every `tm` entry point that spawns `gh` needs the same "load config →
/// resolve github section → GhEnv" sequence; centralising it keeps the call
/// sites to one line and the behaviour identical across `ticket`/`watch`/`issue`.
/// What: loads [`trusty_mpm::core::trusty_tools_config::TrustyToolsConfig`], then
/// resolves its `github` section into a [`GhEnv`] (empty when absent), surfacing
/// the `account`-strategy refusal as an `anyhow` error.
/// Test: the resolution itself is unit-tested; this is thin wiring over `load`.
pub(crate) fn load_gh_env() -> anyhow::Result<GhEnv> {
    let config = trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load();
    let env = resolve_gh_env_anyhow(config.github.as_ref())?;
    if !env.is_empty() {
        // Names only — never the resolved token VALUE (which `vars()` may hold).
        let names: Vec<&str> = env.vars().iter().map(|(k, _)| k.as_str()).collect();
        tracing::debug!(overrides = ?names, "applying per-project GitHub identity binding to gh calls");
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Serialise env mutation so the token-env tests cannot race each other or
    /// the workspace-root tests across the shared test process.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cfg() -> GithubConfig {
        GithubConfig::default()
    }

    /// Why: an absent `github:` section must yield NO overrides so gh inherits
    /// the ambient identity — the no-regression guarantee.
    /// Test: itself.
    #[test]
    fn resolve_absent_config_is_empty() {
        let env = resolve_gh_env(None).expect("none is ok");
        assert!(env.is_empty());
        assert_eq!(env.vars(), &[]);
    }

    /// Why: `config_dir` is the highest-precedence, least-invasive strategy and
    /// must map to `GH_CONFIG_DIR`.
    /// Test: itself.
    #[test]
    fn resolve_config_dir() {
        let c = GithubConfig {
            config_dir: Some(PathBuf::from("/home/bob/.config/gh-work")),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        assert_eq!(
            env.vars(),
            &[(
                ENV_GH_CONFIG_DIR.to_string(),
                "/home/bob/.config/gh-work".to_string()
            )]
        );
    }

    /// Why: `token_env` resolves the token from the NAMEd env var at call time
    /// (the secret is never in config) → `GH_TOKEN`.
    /// Test: itself.
    #[test]
    fn resolve_token_env_present() {
        let _g = env_lock();
        let name = "TM_TEST_GH_TOKEN_PRESENT";
        // SAFETY: guarded by env_lock; removed below.
        unsafe { std::env::set_var(name, "ghp_secret_value") };
        let c = GithubConfig {
            token_env: Some(name.to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        unsafe { std::env::remove_var(name) };
        assert_eq!(
            env.vars(),
            &[(ENV_GH_TOKEN.to_string(), "ghp_secret_value".to_string())]
        );
    }

    /// Why: a `token_env` whose named var is ABSENT must be skipped (precedence
    /// falls through), NOT export an empty `GH_TOKEN`.
    /// Test: itself.
    #[test]
    fn resolve_token_env_absent_falls_through() {
        let _g = env_lock();
        let name = "TM_TEST_GH_TOKEN_DEFINITELY_UNSET";
        // SAFETY: guarded by env_lock.
        unsafe { std::env::remove_var(name) };
        let c = GithubConfig {
            token_env: Some(name.to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        // No identity var set (and no host configured) → empty.
        assert!(env.is_empty(), "expected empty, got {:?}", env.vars());
    }

    /// Why: `config_dir` must win over `token_env` even when the token var IS
    /// present (precedence ordering).
    /// Test: itself.
    #[test]
    fn precedence_config_dir_beats_token_env() {
        let _g = env_lock();
        let name = "TM_TEST_GH_TOKEN_PRECEDENCE";
        // SAFETY: guarded by env_lock.
        unsafe { std::env::set_var(name, "tok") };
        let c = GithubConfig {
            config_dir: Some(PathBuf::from("/cfg/dir")),
            token_env: Some(name.to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        unsafe { std::env::remove_var(name) };
        assert_eq!(
            env.vars(),
            &[(ENV_GH_CONFIG_DIR.to_string(), "/cfg/dir".to_string())]
        );
    }

    /// Why: with no `config_dir`, a present `token_env` must win over `account`
    /// (so the account caveat never triggers when a usable token exists).
    /// Test: itself.
    #[test]
    fn precedence_token_env_beats_account() {
        let _g = env_lock();
        let name = "TM_TEST_GH_TOKEN_BEATS_ACCOUNT";
        // SAFETY: guarded by env_lock.
        unsafe { std::env::set_var(name, "tok2") };
        let c = GithubConfig {
            token_env: Some(name.to_string()),
            account: Some("bob-work".to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        unsafe { std::env::remove_var(name) };
        assert_eq!(
            env.vars(),
            &[(ENV_GH_TOKEN.to_string(), "tok2".to_string())]
        );
    }

    /// Why: `account` alone cannot safely select an identity; the documented
    /// decision is to refuse with a typed, actionable error.
    /// Test: itself.
    #[test]
    fn account_only_is_refused() {
        let c = GithubConfig {
            account: Some("bob-work".to_string()),
            ..cfg()
        };
        let err = resolve_gh_env(Some(&c)).unwrap_err();
        assert_eq!(
            err,
            GhIdentityError::AccountStrategyUnsupported("bob-work".to_string())
        );
        let msg = err.to_string();
        assert!(msg.contains("config_dir"), "msg: {msg}");
        assert!(msg.contains("token_env"), "msg: {msg}");
    }

    /// Why: `account` paired with a usable `config_dir` must NOT error — the
    /// account documents intent while config_dir does the actual selection.
    /// Test: itself.
    #[test]
    fn account_with_config_dir_is_ok() {
        let c = GithubConfig {
            config_dir: Some(PathBuf::from("/cfg/acct")),
            account: Some("bob-work".to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        assert_eq!(
            env.vars(),
            &[(ENV_GH_CONFIG_DIR.to_string(), "/cfg/acct".to_string())]
        );
    }

    /// Why: `host` must always be exported (as `GH_HOST`) when set, even with no
    /// identity strategy configured.
    /// Test: itself.
    #[test]
    fn host_always_applied() {
        let c = GithubConfig {
            host: Some("github.example.com".to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        assert_eq!(
            env.vars(),
            &[(ENV_GH_HOST.to_string(), "github.example.com".to_string())]
        );
    }

    /// Why: host applies independently of (and in addition to) the identity var;
    /// ordering is identity-first then host.
    /// Test: itself.
    #[test]
    fn host_applied_with_identity() {
        let c = GithubConfig {
            config_dir: Some(PathBuf::from("/cfg")),
            host: Some("ghe.corp".to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        assert_eq!(
            env.vars(),
            &[
                (ENV_GH_CONFIG_DIR.to_string(), "/cfg".to_string()),
                (ENV_GH_HOST.to_string(), "ghe.corp".to_string()),
            ]
        );
    }

    /// Why: the host resolver must default to `github.com` when unset.
    /// Test: itself.
    #[test]
    fn effective_host_default() {
        assert_eq!(effective_host(None), "github.com");
        assert_eq!(effective_host(Some(&cfg())), "github.com");
    }

    /// Why: a configured host must override the default.
    /// Test: itself.
    #[test]
    fn effective_host_override() {
        let c = GithubConfig {
            host: Some("github.example.com".to_string()),
            ..cfg()
        };
        assert_eq!(effective_host(Some(&c)), "github.example.com");
    }

    /// Why: clone-URL synthesis defaults to the public host (no config).
    /// Test: itself.
    #[test]
    fn clone_url_default_host() {
        assert_eq!(
            clone_url(None, "bobmatnyc/trusty-tools"),
            "https://github.com/bobmatnyc/trusty-tools"
        );
    }

    /// Why: clone-URL synthesis must honour a GHE host (closes #1261's GHE part).
    /// Test: itself.
    #[test]
    fn clone_url_enterprise_host() {
        let c = GithubConfig {
            host: Some("github.example.com".to_string()),
            ..cfg()
        };
        assert_eq!(
            clone_url(Some(&c), "acme/widget"),
            "https://github.example.com/acme/widget"
        );
    }
}
