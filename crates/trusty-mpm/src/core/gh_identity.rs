//! GitHub identity resolution shared by every `gh`/git-auth-aware surface in
//! `tm` — the CLI's `gh` subprocess sites AND the daemon's managed-session
//! spawn/provisioner path (#2184).
//!
//! Why: #1265 introduced a single GLOBAL `github:` config section and the
//! `config_dir` > `token_env` > `account` precedence chain that turns it into
//! concrete env overrides for a `gh` subprocess, but that logic originally
//! lived only in the `tm` binary (`bin/tm/gh_identity.rs`), unreachable from
//! library code (the daemon's `spawn_managed`/`WorkspaceProvisioner`). #2184
//! adds a PER-PROJECT `github:` binding (`ProjectConfig::github` /
//! `Project::github`) that must resolve identically for both the CLI and the
//! daemon, so the core precedence engine — everything that does NOT depend on
//! `anyhow` or a specific caller's project-lookup strategy — now lives here in
//! the library, with `bin/tm/gh_identity.rs` reduced to a thin,
//! project-aware, `anyhow`-flavoured wrapper over it.
//!
//! What: [`GhEnv`] is the resolved, ordered list of `(VAR, value)` overrides;
//! [`resolve_gh_env`] applies the precedence **`config_dir` > `token_env` >
//! `account`** for identity selection and ALWAYS applies `host` (as `GH_HOST`)
//! when set; [`select_github_config`] applies the #2184 project-over-global
//! precedence (project binding wins outright when present; otherwise the
//! global binding is used); [`clone_url`] synthesises an HTTPS clone URL
//! honouring the host. [`GhIdentityError`] is the typed failure (today only
//! the `account`-strategy refusal). An absent `github:` config resolves to an
//! EMPTY [`GhEnv`], so `gh` inherits the ambient identity (no regression).
//!
//! ## Precedence chain (identity, within one resolved [`GithubConfig`])
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
//! ## Clearing the inherited identity (#6668)
//!
//! Selecting an identity is only half the job. `gh` reads `GH_TOKEN` (and its
//! `GITHUB_TOKEN` / `*_ENTERPRISE_TOKEN` spellings) BEFORE it reads
//! `GH_CONFIG_DIR`, so a `tm` invoked from a shell that exports one account's
//! token authenticated as that account even with a per-project `config_dir`
//! binding applied. Whenever an identity strategy resolves, every OTHER
//! [`GH_INHERITED_IDENTITY_ENV`] var is therefore listed in
//! [`GhEnv::unset_vars`] and removed from the child by
//! [`GhEnv::apply_to`]. An unbound (`None`) config, and a binding that sets
//! only `host`, remove nothing — an operator whose sole credential is an
//! ambient `GH_TOKEN` is unaffected.
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
//! ## Project-over-global precedence (#2184)
//!
//! [`select_github_config`] is the single tested place the new tier is
//! applied: a project's OWN `github:` binding, when present, is used WHOLESALE
//! in place of the global binding (no field-level merge between the two tiers
//! — this matches the existing "a config section is either fully bound or
//! absent" shape `GithubConfig` already has at the global level). The global
//! `github:` section remains the fallback for projects with no binding of
//! their own, and an ambient (unconfigured) identity remains the final
//! fallback via [`resolve_gh_env`]'s existing `None` handling.
//!
//! Test: `resolve_*`, `precedence_*`, `host_*`, `account_*`, `clone_url_*`,
//! `select_github_config_*` in the inline `tests` module.
//!
//! ## Directory-based resolution (#6623)
//!
//! [`select_config_for_origin`] promotes the CLI's project-matching glue
//! (`bin/tm/gh_identity::resolve_project_aware`, which detects the active
//! project from `std::env::current_dir()`'s git origin remote) to the
//! library, so a daemon-side `gh` spawn — which has a WORKING DIRECTORY but no
//! already-detected project — can apply the identical #2184 precedence. It is
//! the pure half only: matching an already-resolved `origin_url` against
//! `config.projects`. The impure half (reading the directory's origin remote,
//! loading [`TrustyToolsConfig`] from disk) is production wiring left to each
//! caller — see `session_manager::worktree_reclaim_gh::resolve_daemon_gh_env`.

use crate::core::trusty_tools_config::{DEFAULT_GITHUB_HOST, GithubConfig, TrustyToolsConfig};

/// `GH_CONFIG_DIR` — points gh at a private config/auth home.
const ENV_GH_CONFIG_DIR: &str = "GH_CONFIG_DIR";
/// `GH_TOKEN` — the auth token gh uses for API calls.
const ENV_GH_TOKEN: &str = "GH_TOKEN";
/// `GH_HOST` — the default host gh targets.
const ENV_GH_HOST: &str = "GH_HOST";

/// Every env var that can select a gh identity on its own (#6668).
///
/// Why: gh resolves auth from the environment BEFORE it reads a scoped config
/// dir, so an inherited `GH_TOKEN` silently outranks the `GH_CONFIG_DIR` this
/// resolver emits — the binding is applied and still loses. Naming the full
/// set once is what keeps the removal complete: `GITHUB_TOKEN` is gh's
/// documented fallback for `GH_TOKEN`, and the two `*_ENTERPRISE_TOKEN`
/// spellings are the same pair for a GHE host.
/// What: the removal candidates [`resolve_gh_env`] clears from a bound child.
/// A var this resolution SETS is never also removed.
/// Test: `binding_removes_the_inherited_identity_vars`,
/// `token_env_binding_removes_the_config_dir`,
/// `absent_config_and_host_only_binding_remove_nothing`.
const GH_INHERITED_IDENTITY_ENV: &[&str] = &[
    ENV_GH_TOKEN,
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    ENV_GH_CONFIG_DIR,
    "GH_USER",
];

/// Typed failures from resolving a [`GithubConfig`] into a [`GhEnv`].
///
/// Why: the `account`-only strategy cannot be honoured without mutating global
/// gh state (see the module docs); a typed error lets the caller surface an
/// actionable message and lets tests assert the refusal precisely instead of
/// string-matching an error chain.
/// What: today the single variant flags the unsupported `account`-only case,
/// carrying the offending account name for the message.
/// Test: `account_only_is_refused`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GhIdentityError {
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

/// The resolved set of environment overrides to apply to a `gh`/git subprocess.
///
/// Why: callers need ONE value that lists exactly which env vars to set on a
/// `Command`, derived once from the active project's config, so every call is
/// bound identically and no call site re-implements the precedence. An EMPTY
/// `GhEnv` means "apply nothing" — the ambient identity is used, preserving
/// pre-#1265 behaviour.
/// What: an ordered list of `(name, value)` pairs. Order is deterministic
/// (identity var first when present, then `GH_HOST`) so tests can assert it.
/// Test: every `resolve_*`/`precedence_*`/`host_*` test inspects `vars()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhEnv {
    vars: Vec<(String, String)>,
    /// #6668: identity vars to REMOVE from the child so an inherited one
    /// cannot outrank the binding in `vars`. Empty when nothing binds.
    unset: Vec<String>,
}

impl GhEnv {
    /// Borrow the resolved `(name, value)` overrides.
    ///
    /// Why: callers iterate these to call `Command::env`; tests iterate them
    /// to assert the resolved set and ordering.
    /// What: returns the override slice (possibly empty).
    /// Test: read by every resolution test.
    pub fn vars(&self) -> &[(String, String)] {
        &self.vars
    }

    /// The env vars to REMOVE from the child before applying [`vars`](Self::vars) (#6668).
    ///
    /// Why: setting `GH_CONFIG_DIR` is not enough to bind an identity. gh reads
    /// `GH_TOKEN` (and its `GITHUB_TOKEN`/enterprise spellings) ahead of any
    /// scoped config dir, so a shell that exports another account's token wins
    /// over the project binding — the #6668 symptom was `tm session
    /// prune-worktrees --merged-prs` reporting "Could not resolve to a
    /// Repository" for every repo the shell token could not see. Every spawn
    /// site must clear these before setting the binding's own vars.
    /// What: the [`GH_INHERITED_IDENTITY_ENV`] entries this resolution does not
    /// itself set, or EMPTY when no identity strategy resolved (ambient stays
    /// ambient — an unbound `gh` call still inherits the operator's token).
    /// Test: `binding_removes_the_inherited_identity_vars`,
    /// `token_env_binding_removes_the_config_dir`,
    /// `absent_config_and_host_only_binding_remove_nothing`.
    pub fn unset_vars(&self) -> &[String] {
        &self.unset
    }

    /// Apply this binding to `cmd`: remove the inherited identity, then set the
    /// resolved overrides (#6668).
    ///
    /// Why: the removal must precede the set, and getting that order wrong at
    /// one call site reintroduces the bug there alone. One applier is what
    /// keeps every `std::process::Command` spawn site identical.
    /// What: `env_remove` for each [`unset_vars`](Self::unset_vars) entry, then
    /// `env` for each [`vars`](Self::vars) pair. A default (`is_empty`) `GhEnv`
    /// touches nothing.
    /// Test: `apply_to_removes_then_sets`.
    pub fn apply_to(&self, cmd: &mut std::process::Command) {
        for key in &self.unset {
            cmd.env_remove(key);
        }
        for (key, value) in &self.vars {
            cmd.env(key, value);
        }
    }

    /// Whether any override is set (i.e. an identity binding is active).
    ///
    /// Why: lets callers cheaply distinguish "bound" from "ambient" without
    /// allocating; useful for diagnostics.
    /// What: true when at least one override is present.
    /// Test: `resolve_absent_config_is_empty` asserts `is_empty()` for None.
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty() && self.unset.is_empty()
    }

    /// One-line, secret-free description of the resolved overrides (#6623).
    ///
    /// Why: a daemon `gh` lookup that fails needs to say WHICH identity it
    /// tried — the #6561 failure was invisible for so long precisely because
    /// nothing distinguished "used no config dir at all" from "used the wrong
    /// one". `GH_TOKEN`'s VALUE must never appear in a diagnostic string.
    /// What: `"no github: binding resolved …"` for an empty `GhEnv`, else the
    /// resolved `VAR=value` pairs joined by `, `, with `GH_TOKEN`'s value
    /// redacted.
    /// Test: `describe_empty_env`, `describe_config_dir`, `describe_redacts_token`.
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "no github: binding resolved — gh inherits the daemon's ambient \
                    environment"
                .to_string();
        }
        // #6668: the removals are named too — "GH_CONFIG_DIR=… " alone did not
        // say whether an inherited token was still in play.
        self.vars
            .iter()
            .map(|(k, v)| {
                if k == ENV_GH_TOKEN {
                    format!("{k}=<redacted>")
                } else {
                    format!("{k}={v}")
                }
            })
            .chain(self.unset.iter().map(|k| format!("-{k}")))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Apply the #2184 project-over-global precedence for a `github:` binding.
///
/// Why: a project's own `github:` section, when present, must win OUTRIGHT
/// over the global section — not merge field-by-field — matching the existing
/// "bound or absent" shape of `GithubConfig`. Centralising the `.or(...)` here
/// (rather than inlining it at each call site) keeps the precedence rule
/// documented and independently testable.
/// What: returns `project` when `Some`, else `global`, else `None` (ambient).
/// Test: `select_github_config_project_wins`,
/// `select_github_config_falls_back_to_global`,
/// `select_github_config_none_when_neither_set`.
pub fn select_github_config<'a>(
    project: Option<&'a GithubConfig>,
    global: Option<&'a GithubConfig>,
) -> Option<&'a GithubConfig> {
    project.or(global)
}

/// Select the `GithubConfig` that governs `gh` spawns for an already-detected
/// git origin remote (#6623).
///
/// Why: every daemon-side `gh` spawn has a WORKING DIRECTORY but no
/// already-resolved project — `bin/tm/gh_identity::resolve_project_aware`
/// performs this same match from `std::env::current_dir()`'s origin remote for
/// an interactive `tm` invocation, but that helper is CLI-only (`bin/tm`),
/// unreachable from library/daemon code. Promoting the pure "which config
/// applies" step here lets both call sites share one implementation.
/// What: matches `origin_url` (when `Some`) against `config.projects` via
/// [`crate::project::record::repo_url_matches`]; the matched project's own
/// `github:` binding, when it has one, wins outright over the global
/// `config.github` section via [`select_github_config`]. `origin_url: None`,
/// or a URL matching no registered project, resolves purely from
/// `config.github` — matching pre-#2184 behaviour exactly.
/// Test: `select_config_for_origin_project_wins`,
/// `select_config_for_origin_falls_back_to_global`,
/// `select_config_for_origin_no_match_uses_global`,
/// `select_config_for_origin_no_origin_uses_global`.
pub fn select_config_for_origin<'a>(
    config: &'a TrustyToolsConfig,
    origin_url: Option<&str>,
) -> Option<&'a GithubConfig> {
    let project_github = origin_url
        .and_then(|url| {
            config
                .projects
                .iter()
                .find(|p| crate::project::record::repo_url_matches(&p.repo_url, url))
        })
        .and_then(|p| p.github.as_ref());
    select_github_config(project_github, config.github.as_ref())
}

/// Resolve an optional [`GithubConfig`] into the [`GhEnv`] for `gh` subprocesses.
///
/// Why: the single, tested place the #1265 precedence chain is applied so every
/// `gh` call `tm` makes is bound to the same identity and no call site diverges.
/// Keeping `token_env` resolution here (via `std::env::var` on the configured
/// NAME) guarantees the plaintext secret never has to live in the config or be
/// threaded through the call graph.
/// What: returns an EMPTY `GhEnv` when `config` is `None`. Otherwise applies
/// **`config_dir` > `token_env`(present) > `account`** for the identity var and
/// ALWAYS appends `GH_HOST` when `host` is set. The `account`-only case returns
/// [`GhIdentityError::AccountStrategyUnsupported`] (see module docs). A
/// `token_env` whose named var is ABSENT is skipped, falling through to the next
/// strategy. #6668: when an identity strategy DOES resolve, every other
/// [`GH_INHERITED_IDENTITY_ENV`] var is listed in
/// [`GhEnv::unset_vars`] for the spawn site to remove — otherwise a shell
/// exporting `GH_TOKEN` outranks the `GH_CONFIG_DIR` this emits and the binding
/// loses. A host-only binding selects no identity and removes nothing.
/// Test: `resolve_absent_config_is_empty`, `resolve_config_dir`,
/// `resolve_token_env_present`, `resolve_token_env_absent_falls_through`,
/// `precedence_config_dir_beats_token_env`,
/// `precedence_token_env_beats_account`, `account_only_is_refused`,
/// `host_always_applied`, `host_applied_with_identity`.
pub fn resolve_gh_env(config: Option<&GithubConfig>) -> Result<GhEnv, GhIdentityError> {
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

    // #6668: an identity strategy that resolved must also CLEAR every other
    // identity var the child would otherwise inherit. Computed before `GH_HOST`
    // is appended, though `inherited_identity_to_clear` would ignore it anyway.
    let unset = inherited_identity_to_clear(&vars);

    // --- Host is applied independently of the identity strategy. ------------
    if let Some(host) = cfg.host.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        vars.push((ENV_GH_HOST.to_string(), host.to_string()));
    }

    Ok(GhEnv { vars, unset })
}

/// Which inherited identity vars a child must NOT keep, given what is set on it.
///
/// Why (#6668): two places bind a gh identity onto a child and neither may
/// decide this for itself — [`resolve_gh_env`] for a `gh`/git subprocess, and
/// `runtime::claude_code_gh_env::write_gh_env_file` for the sourced env file
/// every managed Claude Code / tmux session starts from. The second carries
/// only a `Vec<(String, String)>` of vars to SET, so before this function it
/// could not express a removal at all: a daemon started from a shell that
/// exports `GH_TOKEN` handed that token to every spawned session, outranking
/// the project's pinned `GH_CONFIG_DIR` for the session's whole lifetime.
/// Deriving the answer from the set-vars, in one function both callers ask,
/// keeps the precedence rule single-sourced rather than restated per site.
/// What: EMPTY unless `set_vars` selects an identity (`GH_CONFIG_DIR` or
/// `GH_TOKEN`) — a host-only or informational-only binding changes nothing.
/// Otherwise every [`GH_INHERITED_IDENTITY_ENV`] entry `set_vars` does not
/// itself set. `GH_USER` alone does not count as selecting an identity: it is
/// `trusty-mpm`'s own informational var, not one gh reads.
/// Test: `binding_removes_the_inherited_identity_vars`,
/// `absent_config_and_host_only_binding_remove_nothing`,
/// `inherited_identity_to_clear_ignores_an_informational_only_binding`,
/// and `gh_env_file_unsets_an_inherited_token` in
/// `runtime::claude_code_gh_env_tests`.
pub fn inherited_identity_to_clear(set_vars: &[(String, String)]) -> Vec<String> {
    let selects_identity = set_vars
        .iter()
        .any(|(k, _)| k == ENV_GH_CONFIG_DIR || k == ENV_GH_TOKEN);
    if !selects_identity {
        return Vec::new();
    }
    GH_INHERITED_IDENTITY_ENV
        .iter()
        .filter(|name| !set_vars.iter().any(|(k, _)| k == *name))
        .map(|name| (*name).to_string())
        .collect()
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
pub fn effective_host(config: Option<&GithubConfig>) -> String {
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
pub fn clone_url(config: Option<&GithubConfig>, repo: &str) -> String {
    format!("https://{}/{}", effective_host(config), repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Serialise env mutation so the token-env tests cannot race each other
    /// or the workspace-root tests across the shared test process.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::core::trusty_tools_config::env_test_lock()
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

    /// Why (#6668): a `config_dir` binding that leaves an inherited `GH_TOKEN`
    /// in the child is not a binding — gh reads the env token first, so the
    /// scoped config dir is decorative and the wrong account answers. The
    /// removal list must name every identity var this resolution did not set.
    /// Test: itself.
    #[test]
    fn binding_removes_the_inherited_identity_vars() {
        let c = GithubConfig {
            config_dir: Some(PathBuf::from("/cfg/duetto")),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        let unset: Vec<&str> = env.unset_vars().iter().map(String::as_str).collect();
        assert_eq!(
            unset,
            vec![
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_ENTERPRISE_TOKEN",
                "GITHUB_ENTERPRISE_TOKEN",
                "GH_USER",
            ]
        );
        // The var this resolution SETS is never also removed.
        assert!(!unset.contains(&ENV_GH_CONFIG_DIR));
    }

    /// Why (#6668, review HIGH): `GH_USER` is trusty-mpm's own informational
    /// var, not one gh reads, so a binding that emits only it selects no
    /// identity and must leave the operator's ambient token alone. This is the
    /// case that keeps the managed-session spawn file from clearing a token it
    /// has nothing to replace with.
    /// Test: itself.
    #[test]
    fn inherited_identity_to_clear_ignores_an_informational_only_binding() {
        assert!(
            inherited_identity_to_clear(&[("GH_USER".to_string(), "bobmatnyc".to_string())])
                .is_empty()
        );
        assert!(inherited_identity_to_clear(&[]).is_empty());
        // A config_dir pin alongside it DOES select an identity.
        let cleared = inherited_identity_to_clear(&[
            (ENV_GH_CONFIG_DIR.to_string(), "/cfg".to_string()),
            ("GH_USER".to_string(), "bobmatnyc".to_string()),
        ]);
        assert!(cleared.contains(&ENV_GH_TOKEN.to_string()), "{cleared:?}");
        assert!(!cleared.contains(&"GH_USER".to_string()), "{cleared:?}");
    }

    /// Why (#6668): the mirror case — a `token_env` binding sets `GH_TOKEN`, so
    /// the var it must clear is the inherited `GH_CONFIG_DIR` (and the other
    /// token spellings), not the one it just set.
    /// Test: itself.
    #[test]
    fn token_env_binding_removes_the_config_dir() {
        // SAFETY: single-threaded test setup for a name no other test reads.
        unsafe { std::env::set_var("TM_TEST_6668_TOKEN", "t") };
        let c = GithubConfig {
            token_env: Some("TM_TEST_6668_TOKEN".to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        let unset: Vec<&str> = env.unset_vars().iter().map(String::as_str).collect();
        assert!(unset.contains(&ENV_GH_CONFIG_DIR), "{unset:?}");
        assert!(unset.contains(&"GITHUB_TOKEN"), "{unset:?}");
        assert!(!unset.contains(&ENV_GH_TOKEN), "{unset:?}");
        unsafe { std::env::remove_var("TM_TEST_6668_TOKEN") };
    }

    /// Why (#6668): removal is scoped to a resolved IDENTITY. An absent config
    /// — and a binding that sets only `host` — must stay exactly as ambient as
    /// before, or an operator whose only credential is `GH_TOKEN` loses it.
    /// Test: itself.
    #[test]
    fn absent_config_and_host_only_binding_remove_nothing() {
        assert!(resolve_gh_env(None).expect("ok").unset_vars().is_empty());
        let c = GithubConfig {
            host: Some("github.example.com".to_string()),
            ..cfg()
        };
        assert!(
            resolve_gh_env(Some(&c))
                .expect("ok")
                .unset_vars()
                .is_empty()
        );
    }

    /// Why (#6668): the removal must reach the spawned command, and it must run
    /// BEFORE the set — the reverse order would clear the var just bound.
    /// Test: itself.
    #[test]
    fn apply_to_removes_then_sets() {
        let c = GithubConfig {
            config_dir: Some(PathBuf::from("/cfg/duetto")),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        let mut cmd = std::process::Command::new("true");
        env.apply_to(&mut cmd);
        let seen: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(seen.contains(&("GH_TOKEN".to_string(), None)), "{seen:?}");
        assert!(
            seen.contains(&(
                ENV_GH_CONFIG_DIR.to_string(),
                Some("/cfg/duetto".to_string())
            )),
            "{seen:?}"
        );
    }

    /// Why: `token_env` resolves the token from the NAMEd env var at call time
    /// (the secret is never in config) → `GH_TOKEN`.
    /// Test: itself.
    #[test]
    fn resolve_token_env_present() {
        let _g = env_lock();
        let name = "TM_TEST_GH_TOKEN_PRESENT_LIB";
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
        let name = "TM_TEST_GH_TOKEN_DEFINITELY_UNSET_LIB";
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
        let name = "TM_TEST_GH_TOKEN_PRECEDENCE_LIB";
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
        let name = "TM_TEST_GH_TOKEN_BEATS_ACCOUNT_LIB";
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

    // ── #2184: project-over-global precedence ────────────────────────────────

    /// Why: a project's own `github:` binding must win outright over the
    /// global one when both are present.
    /// Test: itself.
    #[test]
    fn select_github_config_project_wins() {
        let project = GithubConfig {
            account: Some("project-account".into()),
            ..cfg()
        };
        let global = GithubConfig {
            account: Some("global-account".into()),
            ..cfg()
        };
        let selected = select_github_config(Some(&project), Some(&global));
        assert_eq!(selected, Some(&project));
    }

    /// Why: with no project binding, the global one must be used.
    /// Test: itself.
    #[test]
    fn select_github_config_falls_back_to_global() {
        let global = GithubConfig {
            account: Some("global-account".into()),
            ..cfg()
        };
        let selected = select_github_config(None, Some(&global));
        assert_eq!(selected, Some(&global));
    }

    /// Why: with neither tier set, resolution must fall through to the ambient
    /// gh identity (represented here by `None`) — no regression.
    /// Test: itself.
    #[test]
    fn select_github_config_none_when_neither_set() {
        assert_eq!(select_github_config(None, None), None);
    }

    // ── #6623: directory-based resolution ─────────────────────────────────

    use crate::core::trusty_tools_config::ProjectConfig;

    fn project_cfg(repo_url: &str, github: Option<GithubConfig>) -> ProjectConfig {
        ProjectConfig {
            name: "p".into(),
            repo_url: repo_url.into(),
            github,
            ..ProjectConfig::default()
        }
    }

    /// Why: a matched project's own `github:` binding must win outright over
    /// the global one — the same #2184 precedence `select_github_config`
    /// applies, now reachable from a bare origin URL rather than a `cwd`.
    /// Test: itself.
    #[test]
    fn select_config_for_origin_project_wins() {
        let config = TrustyToolsConfig {
            github: Some(gh("/cfg/global")),
            projects: vec![project_cfg(
                "https://github.com/acme/widget",
                Some(gh("/cfg/project")),
            )],
            ..TrustyToolsConfig::default()
        };
        let selected =
            select_config_for_origin(&config, Some("https://github.com/acme/widget.git"));
        assert_eq!(
            selected.and_then(|c| c.config_dir.as_deref()),
            Some(std::path::Path::new("/cfg/project"))
        );
    }

    /// Why: a project match with no `github:` binding of its own must fall
    /// back to the global tier.
    /// Test: itself.
    #[test]
    fn select_config_for_origin_falls_back_to_global() {
        let config = TrustyToolsConfig {
            github: Some(gh("/cfg/global")),
            projects: vec![project_cfg("https://github.com/acme/widget", None)],
            ..TrustyToolsConfig::default()
        };
        let selected = select_config_for_origin(&config, Some("https://github.com/acme/widget"));
        assert_eq!(
            selected.and_then(|c| c.config_dir.as_deref()),
            Some(std::path::Path::new("/cfg/global"))
        );
    }

    /// Why: an origin that matches NO registered project must resolve purely
    /// from the global tier, not fall through to ambient.
    /// Test: itself.
    #[test]
    fn select_config_for_origin_no_match_uses_global() {
        let config = TrustyToolsConfig {
            github: Some(gh("/cfg/global")),
            ..TrustyToolsConfig::default()
        };
        let selected = select_config_for_origin(&config, Some("https://github.com/someone/else"));
        assert_eq!(
            selected.and_then(|c| c.config_dir.as_deref()),
            Some(std::path::Path::new("/cfg/global"))
        );
    }

    /// Why: `origin_url: None` (a directory whose remote could not be read)
    /// must resolve purely from the global tier — matching pre-#2184
    /// behaviour exactly, the no-regression guarantee.
    /// Test: itself.
    #[test]
    fn select_config_for_origin_no_origin_uses_global() {
        let config = TrustyToolsConfig {
            github: Some(gh("/cfg/global")),
            ..TrustyToolsConfig::default()
        };
        let selected = select_config_for_origin(&config, None);
        assert_eq!(
            selected.and_then(|c| c.config_dir.as_deref()),
            Some(std::path::Path::new("/cfg/global"))
        );
    }

    fn gh(config_dir: &str) -> GithubConfig {
        GithubConfig {
            config_dir: Some(PathBuf::from(config_dir)),
            ..GithubConfig::default()
        }
    }

    /// Why: an empty `GhEnv` must describe itself as "no binding resolved"
    /// rather than an empty string, which would render as a blank diagnostic.
    /// Test: itself.
    #[test]
    fn describe_empty_env() {
        assert!(GhEnv::default().describe().contains("no github: binding"));
    }

    /// Why: a resolved `GH_CONFIG_DIR` must appear verbatim — it names the
    /// exact fix an operator applies.
    /// Test: itself.
    #[test]
    fn describe_config_dir() {
        let env = resolve_gh_env(Some(&gh("/cfg/dir"))).expect("ok");
        // #6668: a `-VAR` entry names each identity var the spawn removes, so
        // the diagnostic says whether an inherited token was still in play.
        assert_eq!(
            env.describe(),
            "GH_CONFIG_DIR=/cfg/dir, -GH_TOKEN, -GITHUB_TOKEN, -GH_ENTERPRISE_TOKEN, \
             -GITHUB_ENTERPRISE_TOKEN, -GH_USER"
        );
    }

    /// Why: `GH_TOKEN`'s VALUE must never appear in a diagnostic string.
    /// Test: itself.
    #[test]
    fn describe_redacts_token() {
        let _g = env_lock();
        let name = "TM_TEST_GH_TOKEN_DESCRIBE";
        // SAFETY: guarded by env_lock; removed below.
        unsafe { std::env::set_var(name, "ghp_super_secret") };
        let c = GithubConfig {
            token_env: Some(name.to_string()),
            ..cfg()
        };
        let env = resolve_gh_env(Some(&c)).expect("ok");
        unsafe { std::env::remove_var(name) };
        let described = env.describe();
        assert!(
            described.contains("GH_TOKEN=<redacted>"),
            "described: {described}"
        );
        assert!(
            !described.contains("ghp_super_secret"),
            "described: {described}"
        );
    }
}
