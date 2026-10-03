//! CLI-side, project-aware GitHub identity resolution for every `gh`
//! subprocess `tm` spawns (#1265, project-aware since #2184).
//!
//! Why: the precedence engine (`config_dir` > `token_env` > `account`, plus
//! `GH_HOST`) now lives in the library (`trusty_mpm::core::gh_identity`) so
//! the daemon's managed-spawn path can share it (#2184). This module is the
//! remaining CLI-specific glue: it detects the ACTIVE project (by matching
//! the current directory's git origin remote against `config.projects`),
//! applies the #2184 project-over-global precedence via
//! [`trusty_mpm::core::gh_identity::select_github_config`], and folds the
//! typed [`GhIdentityError`] into `anyhow` (binary code uses `anyhow`, per
//! this workspace's error-handling convention).
//!
//! What: [`load_gh_env`] is the one-line entry point every `tm issue`/`tm
//! ticket`/`tm watch` call site already uses; it is now project-aware with NO
//! call-site changes required. [`resolve_project_aware`] is the pure,
//! directly-testable core (config + a pre-detected origin URL in, `GhEnv`
//! out) that `load_gh_env` wraps with real cwd/git detection.
//!
//! Test: `resolve_project_aware_*` in the inline `tests` module exercise the
//! project-match precedence hermetically (no real `gh`/git needed); the
//! underlying precedence chain itself is tested in
//! `trusty_mpm::core::gh_identity`.
//!
//! #2081/#3312: when the resolved `GithubConfig` pairs a `config_dir` with an
//! `account` (documented intent — see `account_with_config_dir_is_ok` in the
//! library's `gh_identity` tests), [`resolve_project_aware`] also verifies
//! (and self-heals) the ACTIVE account inside that isolated directory via
//! [`trusty_mpm::core::gh_account::ensure_gh_account_in_dir`] before
//! returning — this is the enforcement mechanism added in #2081 that had no
//! production call site until #3312. A project with no `account` paired with
//! its `config_dir` sees no behaviour change (enforcement never fires).

use trusty_mpm::core::gh_identity::{GhEnv, GhIdentityError, select_github_config};
use trusty_mpm::core::trusty_tools_config::TrustyToolsConfig;
use trusty_mpm::project::record::repo_url_matches;

// Re-exported for the existing CLI call sites (`clone_url` is used by
// `commands::watch`; `effective_host`/`resolve_gh_env` are exercised via the
// library's own test suite but kept reachable here for any future CLI need).
pub(crate) use trusty_mpm::core::gh_identity::clone_url;

/// Resolve the [`GhEnv`] for `origin_url` against a loaded [`TrustyToolsConfig`]
/// (#2184) — the pure, hermetically-testable core of [`load_gh_env`].
///
/// Why: separating the "which config wins" decision from the real cwd/git
/// detection lets the project-over-global precedence be asserted without a
/// real git repo on disk.
/// What: when `origin_url` is `Some` and matches (via [`repo_url_matches`]) a
/// `config.projects` entry with its own `github:` binding, that binding wins
/// outright; otherwise falls back to the global `config.github`; with no
/// match (or no `origin_url`) resolution is purely global — matching
/// pre-#2184 behaviour exactly. #8383: the global fallback keeps the caller's
/// inherited `GH_TOKEN`; only a project binding clears it.
/// Test: `resolve_project_aware_project_binding_wins`,
/// `resolve_project_aware_unbound_repo_keeps_the_shell_token_8383`,
/// `resolve_project_aware_falls_back_to_global`,
/// `resolve_project_aware_no_origin_uses_global`,
/// `resolve_project_aware_enforces_paired_account`,
/// `resolve_project_aware_skips_enforcement_when_account_unset`.
pub(crate) fn resolve_project_aware(
    config: &TrustyToolsConfig,
    origin_url: Option<&str>,
) -> anyhow::Result<GhEnv> {
    let project_github = origin_url
        .and_then(|url| {
            config
                .projects
                .iter()
                .find(|p| repo_url_matches(&p.repo_url, url))
        })
        .and_then(|p| p.github.as_ref());
    let selected = select_github_config(project_github, config.github.as_ref());

    // #2081/#3312: verify (and self-heal) the active account inside the
    // resolved `config_dir` BEFORE this identity is used for any real `gh`
    // call, when the project pairs `config_dir` with an explicit `account`.
    // A project that never configures `account` alongside `config_dir` sees
    // no behaviour change here.
    if let Some(target) = trusty_mpm::core::gh_account::configured_account_pair(selected) {
        // #5849: enforce on the project's configured host, not an assumed one.
        trusty_mpm::core::gh_account::ensure_gh_account_in_dir(
            &target.account,
            &target.config_dir,
            &target.host,
        )?;
    }

    // #8383: only a project's own binding clears the caller's token.
    trusty_mpm::core::gh_identity::resolve_tiered_gh_env(project_github, config.github.as_ref())
        .map_err(|e: GhIdentityError| anyhow::anyhow!(e))
}

/// Convenience wrapper: load trusty-mpm config, detect the active project from
/// the current directory's git origin remote, and resolve the active [`GhEnv`].
///
/// Why: every `tm` entry point that spawns `gh` needs the same "load config →
/// detect project → resolve `github:` section → GhEnv" sequence; centralising
/// it keeps the call sites to one line and the behaviour identical across
/// `ticket`/`watch`/`issue`.
/// What: [`load_gh_env_for`] against the process's current directory.
/// Test: `resolve_project_aware_*` cover the resolution logic; this function
/// is thin wiring over real cwd detection.
pub(crate) fn load_gh_env() -> anyhow::Result<GhEnv> {
    match std::env::current_dir() {
        Ok(cwd) => load_gh_env_for(&cwd),
        Err(_) => load_gh_env_for(std::path::Path::new(".")),
    }
}

/// [`load_gh_env`] against an EXPLICIT directory (#8233 review, HIGH).
///
/// Why: the bare-`tm` in-place relaunch runs inside a managed pane whose session
/// has its own workspace, and that workspace — not the shell's current
/// directory — is the project whose `github:` binding the relaunched `claude`
/// must inherit. Before #8233 it inherited the identity from the pane's shell
/// environment, which the spawn had exported there by sourcing a temp file; the
/// launch spec now delivers the environment to `claude` alone, so the pane shell
/// no longer holds it and this path has to resolve it again from config.
/// What: [`load_gh_env`]'s body, with `dir` in place of the process cwd. A
/// local-only pin (#8934, see [`local_only_pin`]) returns before any config
/// read.
/// Test: `resolve_project_aware_*` cover the resolution; the in-place
/// application is covered by
/// `inplace_exec_command_carries_the_pinned_gh_identity`;
/// `load_gh_env_for_a_local_only_repo_disables_gh`.
pub(crate) fn load_gh_env_for(dir: &std::path::Path) -> anyhow::Result<GhEnv> {
    use trusty_mpm::core::remote_mode::{RemoteMode, remote_mode};
    // #4734: still best-effort, but a git failure is logged instead of passing
    // silently as "this directory has no origin remote".
    let mode = remote_mode(dir)
        .inspect_err(|e| tracing::warn!("cannot read git origin remote for gh identity: {e}"))
        .ok();
    let inherited = std::env::var("GH_CONFIG_DIR").ok();
    let mpm = trusty_mpm::core::config::MpmConfig::load_default();
    if let Some(pinned) = local_only_pin(mode.as_ref(), &mpm, inherited.as_deref()) {
        return Ok(pinned);
    }
    let config = TrustyToolsConfig::load();
    let origin_url = match mode {
        Some(RemoteMode::Origin(url)) => Some(url),
        _ => None,
    };
    let env = resolve_project_aware(&config, origin_url.as_deref())?;
    if !env.is_empty() {
        // Names only — never the resolved token VALUE (which `vars()` may hold).
        let names: Vec<&str> = env.vars().iter().map(|(k, _)| k.as_str()).collect();
        tracing::debug!(overrides = ?names, "applying per-project GitHub identity binding to gh calls");
    }
    Ok(env)
}

/// The #8934 "no remote, no gh" binding a `tm` gh call gets, if any.
///
/// Why: a `tm` gh call from a local-only repository must not fall back to the
/// global `github:` binding or the machine's active account, and one run from
/// ANOTHER directory inside a pinned local-only session (`cd /tmp && tm issue`)
/// must keep the session's pin rather than resolve a fresh identity whose
/// `inherited_identity_to_clear` would strip the pin's dummy tokens. The
/// allow-listed supervisor keeps gh (07:47Z ruling).
/// What: `Some(GhEnv::local_only())` when the inherited `GH_CONFIG_DIR` is the
/// local-only pin, or when `mode` is local-only and
/// [`trusty_mpm::core::remote_mode::gh_disabled_for`] its root under `mpm`;
/// otherwise `None`.
/// Test: `load_gh_env_for_a_local_only_repo_disables_gh`,
/// `an_allow_listed_supervisor_dir_keeps_gh_in_the_cli`,
/// `a_pinned_session_keeps_its_pin_outside_the_repo`.
pub(crate) fn local_only_pin(
    mode: Option<&trusty_mpm::core::remote_mode::RemoteMode>,
    mpm: &trusty_mpm::core::config::MpmConfig,
    inherited_config_dir: Option<&str>,
) -> Option<GhEnv> {
    use trusty_mpm::core::remote_mode::{LOCAL_ONLY_GH_CONFIG_DIR, RemoteMode, gh_disabled_for};
    if inherited_config_dir == Some(LOCAL_ONLY_GH_CONFIG_DIR) {
        return Some(GhEnv::local_only());
    }
    match mode {
        Some(RemoteMode::LocalOnly { root }) if gh_disabled_for(root, mpm) => {
            Some(GhEnv::local_only())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::trusty_tools_config::{GithubConfig, ProjectConfig};

    fn project(repo_url: &str, github: Option<GithubConfig>) -> ProjectConfig {
        ProjectConfig {
            name: "p".into(),
            repo_url: repo_url.into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github,
            commit_name: None,
            commit_email: None,
            untracked_sync: None,
            worktree: None,
        }
    }

    fn gh(config_dir: &str) -> GithubConfig {
        GithubConfig {
            config_dir: Some(config_dir.into()),
            token_env: None,
            account: None,
            host: None,
        }
    }

    /// Build a `TrustyToolsConfig` carrying only the fields these tests set.
    ///
    /// Why (#5204): `TrustyToolsConfig` is `#[non_exhaustive]` so that adding a
    /// settings field stays non-breaking. A binary target is a separate crate
    /// from the lib, so it cannot use a struct literal — this is the
    /// build-then-assign shape every out-of-crate consumer must use.
    /// What: `Default` with `github` and `projects` overridden.
    /// Test: used by every `resolve_project_aware_*` test below.
    fn cfg_of(github: Option<GithubConfig>, projects: Vec<ProjectConfig>) -> TrustyToolsConfig {
        let mut c = TrustyToolsConfig::default();
        c.github = github;
        c.projects = projects;
        c
    }

    /// Why: a matched project's own `github:` binding must win over the
    /// global one — the #2184 precedence this module adds.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_project_binding_wins() {
        let config = cfg_of(
            Some(gh("/cfg/global")),
            vec![project(
                "https://github.com/acme/widget",
                Some(gh("/cfg/project")),
            )],
        );
        let env =
            resolve_project_aware(&config, Some("https://github.com/acme/widget.git")).expect("ok");
        assert_eq!(
            env.vars(),
            &[("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())]
        );
    }

    /// Why: a project match with no `github:` binding of its own must fall
    /// back to the global tier.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_falls_back_to_global() {
        let config = cfg_of(
            Some(gh("/cfg/global")),
            vec![project("https://github.com/acme/widget", None)],
        );
        let env =
            resolve_project_aware(&config, Some("https://github.com/acme/widget")).expect("ok");
        assert_eq!(
            env.vars(),
            &[("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// FAILS BEFORE #8383: an origin no project binds fell back to the global
    /// `config_dir` AND removed the shell's `GH_TOKEN`, so `tm issue
    /// seed-labels` asked GitHub as an account that could not see the repo
    /// while `gh -R` from the same shell, using the token, could.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_unbound_repo_keeps_the_shell_token_8383() {
        let config = cfg_of(
            Some(gh("/cfg/global")),
            vec![project(
                "https://github.com/acme/widget",
                Some(gh("/cfg/project")),
            )],
        );
        let unbound = resolve_project_aware(&config, Some("https://github.com/hotstats/reporting"))
            .expect("ok");
        assert_eq!(
            unbound.vars(),
            &[("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
        assert!(unbound.unset_vars().is_empty(), "{unbound:?}");

        // #6668 still holds for a bound repo: its binding beats the shell token.
        let bound =
            resolve_project_aware(&config, Some("https://github.com/acme/widget")).expect("ok");
        assert!(
            bound.unset_vars().iter().any(|k| k == "GH_TOKEN"),
            "{bound:?}"
        );
    }

    /// Why: with no detected origin (e.g. `tm` run outside a git repo), and no
    /// declared project therefore matches — resolution must be purely global,
    /// matching pre-#2184 behaviour exactly.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_no_origin_uses_global() {
        let config = cfg_of(Some(gh("/cfg/global")), Vec::new());
        let env = resolve_project_aware(&config, None).expect("ok");
        assert_eq!(
            env.vars(),
            &[("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// Why: a detected origin that matches NO declared project, with no
    /// global binding either, must resolve to a fully ambient (empty) env —
    /// the no-regression guarantee for projects with no #2184/#1265 config.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_no_match_no_global_is_ambient() {
        let config = TrustyToolsConfig::default();
        let env =
            resolve_project_aware(&config, Some("https://github.com/someone/else")).expect("ok");
        assert!(env.is_empty());
    }

    // ── #2081/#3312: enforcement wiring ───────────────────────────────────
    //
    // #7059: `trusty_mpm::core::gh_scoped_stub` answers the scoped `gh` calls
    // in-process. These tests used a fake `gh` shell script on the
    // process-global `PATH`, which a sibling test's `PATH` restore could remove
    // mid-run — the real `gh` then answered and the 5 s enforcement ceiling
    // turned into a flake.

    use trusty_mpm::core::gh_scoped_stub::GhScopedStub;

    // Serialises PATH mutation across the tests below — cargo runs unit tests
    // in parallel and env vars are process-global. #7996: this used to be a
    // module-local `static ENV_LOCK`, which serialised only these three tests
    // while `test_support::tmux_session` execs a bare `tmux` the OS resolves
    // through the very `$PATH` they rewrite. The binary target's one PATH
    // regime now lives in `test_support`, where both sides can reach it.
    fn fake_gh_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_support::lock_path_env()
    }

    /// #7996, inverted into an assertion: the PATH override these tests install
    /// and the tmux fixture's spawn seam must contend on ONE mutex, or the
    /// fixture can still exec while `$PATH` is mid-rewrite.
    /// Test: itself.
    #[test]
    fn gh_path_override_and_the_tmux_fixture_share_one_lock() {
        let _g = fake_gh_lock();
        assert!(
            crate::test_support::path_env_mutex().try_lock().is_err(),
            "`fake_gh_lock` must hold the same mutex `test_support::lock_path_env` hands \
             the tmux fixture (#7996)"
        );
    }

    /// Remove any ambient `GH_TOKEN`/`GITHUB_TOKEN`, returning them to restore.
    ///
    /// #5849: an ambient token makes `ensure_gh_account_in_dir` refuse, so a
    /// shell or CI job that exports one would otherwise decide these outcomes.
    /// Keys are spelled as literals (never a loop variable) so
    /// `env_isolation_tests`' rule-1 source scan can read what these write.
    type AmbientTokens = (Option<std::ffi::OsString>, Option<std::ffi::OsString>);

    fn strip_ambient_tokens() -> AmbientTokens {
        let prior = (
            std::env::var_os("GH_TOKEN"),
            std::env::var_os("GITHUB_TOKEN"),
        );
        // SAFETY: guarded by `fake_gh_lock` in every caller.
        unsafe {
            std::env::remove_var("GH_TOKEN");
            std::env::remove_var("GITHUB_TOKEN");
        }
        prior
    }

    fn restore_ambient_tokens(prior: AmbientTokens) {
        // SAFETY: guarded by `fake_gh_lock` in every caller.
        unsafe {
            match prior.0 {
                Some(v) => std::env::set_var("GH_TOKEN", v),
                None => std::env::remove_var("GH_TOKEN"),
            }
            match prior.1 {
                Some(v) => std::env::set_var("GITHUB_TOKEN", v),
                None => std::env::remove_var("GITHUB_TOKEN"),
            }
        }
    }

    /// Why (#3312): a project whose resolved `github:` binding pairs
    /// `config_dir` with `account` must have that account VERIFIED (and
    /// corrected, since the stub here starts with the wrong account active)
    /// before `resolve_project_aware` returns — proving the CLI path
    /// (`tm ticket`/`tm issue`/`tm watch`, via `load_gh_env`) now actually
    /// enforces #2081 rather than merely documenting it in config.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_enforces_paired_account() {
        let _g = fake_gh_lock();
        let config_dir = tempfile::tempdir().expect("config tempdir");
        let (stub, _stub) = GhScopedStub::logged_in_as("wrong-account").install();
        let prior_tokens = strip_ambient_tokens();

        let mut cfg = gh(&config_dir.path().display().to_string());
        cfg.account = Some("bobmatnyc".to_string());
        let config = cfg_of(Some(cfg), Vec::new());
        let result = resolve_project_aware(&config, None);

        restore_ambient_tokens(prior_tokens);
        assert!(result.is_ok(), "expected self-heal to succeed: {result:?}");
        assert_eq!(
            stub.active_account(),
            "bobmatnyc",
            "the switch must have taken effect"
        );
    }

    /// Why (#3312): when the enforced switch itself fails (the expected
    /// account was never logged in under this project's isolated
    /// `GH_CONFIG_DIR`), `resolve_project_aware` — and therefore every `tm
    /// ticket`/`tm issue`/`tm watch` invocation via `load_gh_env` — must
    /// hard-fail rather than silently proceed with the wrong `gh` identity.
    /// This is the causal proof: pre-#3312, NOTHING called this enforcement,
    /// so this exact mismatched-account scenario would have resolved `Ok`
    /// and every subsequent `gh` call in the command would have silently run
    /// as the wrong account.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_fails_closed_on_switch_failure() {
        let _g = fake_gh_lock();
        let config_dir = tempfile::tempdir().expect("config tempdir");
        let (_stub_handle, _stub) = GhScopedStub::logged_in_as("wrong-account")
            .with_failing_switch()
            .install();
        let prior_tokens = strip_ambient_tokens();

        let mut cfg = gh(&config_dir.path().display().to_string());
        cfg.account = Some("bobmatnyc".to_string());
        let config = cfg_of(Some(cfg), Vec::new());
        let result = resolve_project_aware(&config, None);

        restore_ambient_tokens(prior_tokens);
        let err = result.expect_err("mismatched account with a failed switch must be an Err");
        assert!(err.to_string().contains("bobmatnyc"), "err: {err}");
    }

    /// Why (#3312): a project that pairs `config_dir` with NO `account` (the
    /// pre-#3312, still-supported shape) must see NO enforcement at all —
    /// proven by scripting every scoped `gh` answer to fail, then asserting
    /// resolution still succeeds (which is only possible if `gh` was never
    /// invoked).
    /// Test: itself.
    #[test]
    fn resolve_project_aware_skips_enforcement_when_account_unset() {
        let _g = fake_gh_lock();
        let config_dir = tempfile::tempdir().expect("config tempdir");
        let (_stub_handle, _stub) = GhScopedStub::logged_in_as("irrelevant")
            .with_failing_api()
            .with_failing_switch()
            .install();

        let config = cfg_of(
            Some(gh(&config_dir.path().display().to_string())),
            Vec::new(),
        );
        let result = resolve_project_aware(&config, None);

        assert!(
            result.is_ok(),
            "no account paired with config_dir must skip enforcement entirely: {result:?}"
        );
    }

    /// FAIL-OPEN CHECK (#8934): a `tm` gh call run from a repository with no
    /// `origin` must not fall back to the global `github:` binding or the
    /// machine's active account — it gets the local-only pin, which strips
    /// every inherited token.
    /// Test: itself.
    #[test]
    fn load_gh_env_for_a_local_only_repo_disables_gh() {
        let repo = tempfile::TempDir::new().expect("tempdir");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(repo.path())
            .status()
            .expect("git init");
        assert!(init.success(), "git init failed");
        let env = load_gh_env_for(repo.path()).expect("local-only env");
        let value = |k: &str| {
            env.vars()
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            value("GH_CONFIG_DIR").as_deref(),
            Some(trusty_mpm::core::remote_mode::LOCAL_ONLY_GH_CONFIG_DIR)
        );
        assert!(
            env.unset_vars().iter().any(|k| k == "GITHUB_TOKEN"),
            "{env:?}"
        );
    }

    /// A no-origin repo whose `.trusty-mpm.toml` asks for `profile`.
    fn local_only_repo(profile: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let repo = tempfile::TempDir::new().expect("tempdir");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(repo.path())
            .status()
            .expect("git init");
        assert!(init.success(), "git init failed");
        let toml = format!("profile = \"{profile}\"\n");
        std::fs::write(repo.path().join(".trusty-mpm.toml"), toml).expect("toml");
        let root = std::fs::canonicalize(repo.path()).expect("canonical");
        (repo, root)
    }

    fn allowing(dir: &std::path::Path) -> trusty_mpm::core::config::MpmConfig {
        let mut config = trusty_mpm::core::config::MpmConfig::default();
        config.supervisor.projects = vec![dir.to_path_buf()];
        config
    }

    /// 🔴 #8934 HIGH 1 (07:47Z ruling): the Architect's directory has no origin
    /// by design; an ALLOW-LISTED supervisor there keeps the machine's gh for
    /// `tm issue`/`tm ticket`/`tm pr` and the in-place relaunch.
    /// Test: itself.
    #[test]
    fn an_allow_listed_supervisor_dir_keeps_gh_in_the_cli() {
        use trusty_mpm::core::remote_mode::remote_mode;
        let (_repo, root) = local_only_repo("supervisor");
        let mode = remote_mode(&root).expect("mode");
        assert!(local_only_pin(Some(&mode), &allowing(&root), None).is_none());
        // #8453: the project's own switch alone does not exempt it.
        let unlisted = trusty_mpm::core::config::MpmConfig::default();
        assert!(local_only_pin(Some(&mode), &unlisted, None).is_some());
    }

    /// 🔴 #8934 MEDIUM 5: inside a pinned local-only session, a `tm` gh call
    /// run from another directory keeps the session's pin.
    /// Test: itself.
    #[test]
    fn a_pinned_session_keeps_its_pin_outside_the_repo() {
        use trusty_mpm::core::remote_mode::{LOCAL_ONLY_GH_CONFIG_DIR, RemoteMode};
        let origin = RemoteMode::Origin("https://github.com/o/r.git".into());
        let mpm = trusty_mpm::core::config::MpmConfig::default();
        let pinned = local_only_pin(Some(&origin), &mpm, Some(LOCAL_ONLY_GH_CONFIG_DIR));
        assert!(pinned.is_some(), "the inherited pin must survive `cd`");
        assert!(local_only_pin(Some(&origin), &mpm, None).is_none());
    }
}
