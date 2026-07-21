//! Resolved git-operation identity: `gh`-auth env overrides plus an optional
//! commit-author override, applied to the managed/provisioner git subprocesses
//! (#2184).
//!
//! Why: [`crate::core::gh_identity`] resolves a `GithubConfig` into env
//! overrides for `gh`/git-credential-helper auth, but the managed spawn path
//! (`daemon::managed_routes::lifecycle::spawn_managed`) and the workspace
//! provisioner (`provisioner::workspace::RealGitBackend`) need TWO things
//! bundled together for every git subprocess they run: those same env
//! overrides (so `git clone`/`fetch` over HTTPS authenticate as the right
//! identity when a credential helper consults them) AND an optional commit
//! author override (`-c user.name=`/`-c user.email=`) that #2184 introduces as
//! a NET-NEW per-project setting — `tm` previously never set a commit identity
//! on any git invocation it made. [`GitIdentity`] is that bundle;
//! [`resolve_git_identity`] builds one from the resolved `GithubConfig` plus
//! the project's own commit-identity fields; [`resolve_for_config`] is the
//! convenience entry point that also finds the matching project (by
//! `repo_url`) in a loaded [`TrustyToolsConfig`].
//!
//! What: `GitIdentity::env` are the `(name, value)` pairs to apply via
//! `Command::envs`; `GitIdentity::commit_config_args` renders the `-c
//! user.name=…`/`-c user.email=…` args to prepend to a `git` invocation (empty
//! when neither is set, so an unconfigured project's git commands are
//! byte-for-byte identical to pre-#2184 behaviour).
//!
//! Test: `resolve_git_identity_*`, `resolve_for_config_*`,
//! `commit_config_args_*` in the inline `tests` module.
//!
//! ## Account enforcement wiring (#2081/#3312)
//!
//! [`resolve_for_config`] above stays deliberately pure (no I/O) — every
//! existing test here needs that. But #2081's per-project account
//! enforcement (verifying, and self-healing, WHICH account is active inside
//! a resolved `config_dir`) had no production call site until #3312.
//! [`resolve_for_config_enforced`] is the daemon's wired entry point: it
//! resolves via [`resolve_for_config`] unchanged, then — only when the
//! selected `GithubConfig` pairs `config_dir` with an `account` (see
//! `core::gh_account::configured_account_pair`) — runs the verify/correct
//! check on the Tokio blocking pool (it may shell out to `gh`) BEFORE
//! returning, so a wrong-account `config_dir` can never reach the clone/push
//! `RealGitBackend` performs. `spawn_managed_cloned` and the local→managed
//! redirect path (`daemon::managed_routes::lifecycle`) both call this instead
//! of the plain resolver.

use crate::core::gh_account::{configured_account_pair, ensure_gh_account_in_dir};
use crate::core::gh_identity::{GhIdentityError, resolve_gh_env, select_github_config};
use crate::core::trusty_tools_config::{GithubConfig, TrustyToolsConfig};
use crate::project::record::repo_url_matches;

/// A resolved git-operation identity: auth env overrides plus an optional
/// commit-author override.
///
/// Why: bundling both concerns lets `RealGitBackend` apply a SINGLE resolved
/// value to every git subprocess it runs, rather than threading two separate
/// values through the `GitBackend` trait.
/// What: `env` — ordered `(name, value)` overrides (mirrors
/// [`crate::core::gh_identity::GhEnv::vars`]); `commit_name`/`commit_email` —
/// optional git commit author overrides. All empty/`None` (the `Default`) is
/// the pre-#2184 ambient behaviour: no env overrides, no commit identity
/// applied.
/// Test: `commit_config_args_empty_when_unset`,
/// `commit_config_args_both_set`, `commit_config_args_name_only`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitIdentity {
    /// Env overrides to apply to the git subprocess (e.g. `GH_CONFIG_DIR`,
    /// `GH_TOKEN`, `GH_HOST`) so an HTTPS credential helper picks the right
    /// identity.
    pub env: Vec<(String, String)>,
    /// Git commit author name override, applied as `-c user.name=<name>`.
    pub commit_name: Option<String>,
    /// Git commit author email override, applied as `-c user.email=<email>`.
    pub commit_email: Option<String>,
}

impl GitIdentity {
    /// Whether this identity carries no overrides at all (fully ambient).
    ///
    /// Why: lets callers skip building a command wrapper entirely when there
    /// is nothing to apply.
    /// What: true when `env` is empty and both commit fields are `None`.
    /// Test: covered by `resolve_git_identity_none_when_nothing_configured`.
    pub fn is_empty(&self) -> bool {
        self.env.is_empty() && self.commit_name.is_none() && self.commit_email.is_none()
    }

    /// Render the `-c user.name=…`/`-c user.email=…` args for a `git` command.
    ///
    /// Why: git accepts repeated `-c key=value` overrides BEFORE the
    /// subcommand; centralising the rendering means every git subprocess site
    /// builds the identical arg shape.
    /// What: returns `[]` when neither field is set; otherwise one `-c
    /// user.name=<name>` pair and/or one `-c user.email=<email>` pair, in that
    /// order, only for the fields that are `Some`.
    /// Test: `commit_config_args_empty_when_unset`,
    /// `commit_config_args_both_set`, `commit_config_args_name_only`.
    pub fn commit_config_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(name) = &self.commit_name {
            args.push("-c".to_string());
            args.push(format!("user.name={name}"));
        }
        if let Some(email) = &self.commit_email {
            args.push("-c".to_string());
            args.push(format!("user.email={email}"));
        }
        args
    }
}

/// Build a [`GitIdentity`] from already-selected `GithubConfig` tiers plus
/// commit-identity fields.
///
/// Why: the pure combination step — apply [`select_github_config`]'s
/// project-over-global precedence, resolve it into env overrides via
/// [`resolve_gh_env`], and attach the (project-scoped-only; #2184 does not
/// define a global commit identity tier) commit author override — kept
/// separate from any config/registry lookup so it is unit-testable without a
/// loaded [`TrustyToolsConfig`].
/// What: returns `Err` only when [`resolve_gh_env`] refuses an `account`-only
/// binding (see `core::gh_identity` module docs).
/// Test: `resolve_git_identity_project_overrides_global`,
/// `resolve_git_identity_falls_back_to_global`,
/// `resolve_git_identity_none_when_nothing_configured`,
/// `resolve_git_identity_commit_identity_independent_of_github`.
pub fn resolve_git_identity(
    project_github: Option<&GithubConfig>,
    global_github: Option<&GithubConfig>,
    commit_name: Option<&str>,
    commit_email: Option<&str>,
) -> Result<GitIdentity, GhIdentityError> {
    let selected = select_github_config(project_github, global_github);
    let env = resolve_gh_env(selected)?;
    Ok(GitIdentity {
        env: env.vars().to_vec(),
        commit_name: commit_name.map(str::to_string),
        commit_email: commit_email.map(str::to_string),
    })
}

/// Resolve the [`GitIdentity`] for `repo_url` from a loaded [`TrustyToolsConfig`].
///
/// Why: `spawn_managed`'s clone-based and local-redirect provisioning paths
/// already load `TrustyToolsConfig` once per spawn; this is the one place that
/// turns that config plus the target `repo_url` into the identity
/// `RealGitBackend` should use, so the daemon's two call sites cannot diverge.
/// What: finds the first `config.projects` entry whose `repo_url` matches (via
/// [`repo_url_matches`], which compares parsed `owner/repo` when possible) and
/// uses ITS `github`/`commit_name`/`commit_email` fields; falls back to the
/// global `config.github` tier when no project matches or the matched project
/// has no `github` binding of its own (commit identity is project-scoped only
/// — no global fallback tier). No match at all → fully ambient `GitIdentity`
/// (`is_empty()` — no regression).
/// Test: `resolve_for_config_matches_project_by_repo_url`,
/// `resolve_for_config_falls_back_to_global_github`,
/// `resolve_for_config_ambient_when_no_match`.
pub fn resolve_for_config(
    config: &TrustyToolsConfig,
    repo_url: &str,
) -> Result<GitIdentity, GhIdentityError> {
    let matched = config
        .projects
        .iter()
        .find(|p| repo_url_matches(&p.repo_url, repo_url));

    let project_github = matched.and_then(|p| p.github.as_ref());
    let commit_name = matched.and_then(|p| p.commit_name.as_deref());
    let commit_email = matched.and_then(|p| p.commit_email.as_deref());

    resolve_git_identity(
        project_github,
        config.github.as_ref(),
        commit_name,
        commit_email,
    )
}

/// Resolve the effective [`GithubConfig`] for `repo_url` — the SAME
/// project-over-global precedence [`resolve_for_config`] applies internally
/// — exposed so a caller that needs the pre-[`GhEnv`] config (the #2081/#3312
/// account-enforcement decision) does not have to reimplement the project
/// lookup and risk diverging from it.
///
/// Why: [`resolve_for_config_enforced`] needs to know WHICH `GithubConfig`
/// would be used for `repo_url` in order to decide whether an `account` is
/// paired with a `config_dir` (see `core::gh_account::configured_account_pair`);
/// factoring the lookup out keeps that decision and `resolve_for_config`
/// itself from ever disagreeing on which tier won.
/// What: mirrors `resolve_for_config`'s `matched`/`project_github` steps,
/// then applies [`select_github_config`].
/// Test: `select_github_config_for_project_wins`,
/// `select_github_config_for_falls_back_to_global`,
/// `select_github_config_for_no_match_is_global`.
pub fn select_github_config_for<'a>(
    config: &'a TrustyToolsConfig,
    repo_url: &str,
) -> Option<&'a GithubConfig> {
    let project_github = config
        .projects
        .iter()
        .find(|p| repo_url_matches(&p.repo_url, repo_url))
        .and_then(|p| p.github.as_ref());
    select_github_config(project_github, config.github.as_ref())
}

/// Resolve `repo_url`'s [`GitIdentity`] AND enforce any `#2081` account
/// pairing before returning — the daemon's wired entry point (#3312) in
/// place of the plain [`resolve_for_config`].
///
/// Why: `resolve_for_config` (and the pure `resolve_git_identity`/
/// `resolve_gh_env` chain under it) intentionally does NO I/O — every
/// existing test in this module depends on that. But the resolved
/// `config_dir` may itself have the WRONG account active (the exact #2081
/// incident shape), and only a real `gh auth status` check inside that
/// directory can catch it. The daemon's two spawn paths
/// (`spawn_managed_cloned`, the local→managed redirect) both provision a
/// clone via `RealGitBackend` using the identity this resolves — enforcing
/// HERE, before either call, means a wrong-account `config_dir` can never
/// reach a clone/push. The check runs on `tokio::task::spawn_blocking` (it
/// may shell out to `gh`, bounded by `GH_ENFORCE_TIMEOUT`) so it never stalls
/// the async executor, mirroring `gh_account::resolve_gh_account_env_for_registry`'s
/// existing blocking-pool convention for `gh` subprocess calls from an async
/// context.
/// What: resolves via [`resolve_for_config`] first, surfacing its typed error
/// unchanged on failure. When [`select_github_config_for`]'s result pairs
/// `config_dir` with a non-blank `account` (`configured_account_pair`
/// returns `Some`), verifies/self-heals the active account inside that
/// directory via [`ensure_gh_account_in_dir`] on the blocking pool; a
/// mismatch that cannot be corrected — or a panicked blocking task — is a
/// hard `Err`, never a silent pass. No `account` paired with `config_dir` →
/// no enforcement, `resolve_for_config`'s result is returned unchanged (the
/// pre-#3312, unconfigured-project behaviour, byte-for-byte).
/// Test: `resolve_for_config_enforced_self_heals_mismatch`,
/// `resolve_for_config_enforced_skips_when_account_unset`, and
/// `resolve_for_config_enforced_fails_closed_on_switch_failure` — which also
/// carries the causality proof: it asserts `resolve_for_config` ALONE (the
/// exact pre-#3312 production path) still resolves `Ok` on the same
/// mismatched-account fixture that `resolve_for_config_enforced` correctly
/// rejects.
pub async fn resolve_for_config_enforced(
    config: &TrustyToolsConfig,
    repo_url: &str,
) -> anyhow::Result<GitIdentity> {
    let identity = resolve_for_config(config, repo_url)?;

    if let Some((dir, account)) =
        configured_account_pair(select_github_config_for(config, repo_url))
    {
        tokio::task::spawn_blocking(move || ensure_gh_account_in_dir(&account, &dir))
            .await
            .map_err(|e| anyhow::anyhow!("gh account enforcement task panicked: {e}"))??;
    }

    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh(account: &str) -> GithubConfig {
        GithubConfig {
            account: None,
            config_dir: Some(std::path::PathBuf::from(format!("/cfg/{account}"))),
            token_env: None,
            host: None,
        }
    }

    // ── resolve_git_identity ──────────────────────────────────────────────

    /// Why: a project-scoped `github:` binding must win over the global one.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_project_overrides_global() {
        let project = gh("project");
        let global = gh("global");
        let identity = resolve_git_identity(Some(&project), Some(&global), None, None).expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())]
        );
    }

    /// Why: with no project binding, the global tier must be used.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_falls_back_to_global() {
        let global = gh("global");
        let identity = resolve_git_identity(None, Some(&global), None, None).expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// Why: with nothing configured anywhere, the identity must be fully
    /// ambient — no regression for projects with no #2184 binding.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_none_when_nothing_configured() {
        let identity = resolve_git_identity(None, None, None, None).expect("ok");
        assert!(identity.is_empty());
    }

    /// Why: commit identity is an independent axis from the `gh` env
    /// overrides — it must apply even when no `github:` binding exists at
    /// either tier.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_commit_identity_independent_of_github() {
        let identity =
            resolve_git_identity(None, None, Some("Bot"), Some("bot@example.com")).expect("ok");
        assert!(identity.env.is_empty());
        assert_eq!(identity.commit_name.as_deref(), Some("Bot"));
        assert_eq!(identity.commit_email.as_deref(), Some("bot@example.com"));
    }

    // ── resolve_for_config ────────────────────────────────────────────────

    fn project_config(
        repo_url: &str,
        github: Option<GithubConfig>,
    ) -> crate::core::trusty_tools_config::ProjectConfig {
        crate::core::trusty_tools_config::ProjectConfig {
            name: "p".into(),
            repo_url: repo_url.into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github,
            commit_name: Some("Project Bot".into()),
            commit_email: Some("bot@project.example.com".into()),
            untracked_sync: None,
        }
    }

    /// Why: `resolve_for_config` must find the project by matching `repo_url`
    /// (tolerating `.git`/scheme differences via `repo_url_matches`) and use
    /// its `github`/commit-identity fields.
    /// Test: itself.
    #[test]
    fn resolve_for_config_matches_project_by_repo_url() {
        let config = TrustyToolsConfig {
            projects: vec![project_config(
                "https://github.com/acme/widget.git",
                Some(gh("project")),
            )],
            ..Default::default()
        };
        let identity = resolve_for_config(&config, "https://github.com/acme/widget").expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())]
        );
        assert_eq!(identity.commit_name.as_deref(), Some("Project Bot"));
        assert_eq!(
            identity.commit_email.as_deref(),
            Some("bot@project.example.com")
        );
    }

    /// Why: a project match with NO `github:` binding of its own must fall
    /// back to the global tier (project-over-global, not project-only).
    /// Test: itself.
    #[test]
    fn resolve_for_config_falls_back_to_global_github() {
        let config = TrustyToolsConfig {
            github: Some(gh("global")),
            projects: vec![project_config("https://github.com/acme/widget", None)],
            ..Default::default()
        };
        let identity = resolve_for_config(&config, "https://github.com/acme/widget").expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// Why: a `repo_url` matching NO declared project must resolve to a fully
    /// ambient identity — the no-regression guarantee for repos `tm` clones
    /// that were never declared in `config.projects`.
    /// Test: itself.
    #[test]
    fn resolve_for_config_ambient_when_no_match() {
        let config = TrustyToolsConfig::default();
        let identity = resolve_for_config(&config, "https://github.com/someone/else").expect("ok");
        assert!(identity.is_empty());
    }

    // ── select_github_config_for ──────────────────────────────────────────

    /// Why: `select_github_config_for` must agree with what
    /// `resolve_for_config` actually uses — a project's own binding wins.
    /// Test: itself.
    #[test]
    fn select_github_config_for_project_wins() {
        let config = TrustyToolsConfig {
            github: Some(gh("global")),
            projects: vec![project_config(
                "https://github.com/acme/widget.git",
                Some(gh("project")),
            )],
            ..Default::default()
        };
        let selected = select_github_config_for(&config, "https://github.com/acme/widget");
        assert_eq!(selected, Some(&gh("project")));
    }

    /// Why: no project-level binding must fall back to global, matching
    /// `resolve_for_config`'s own fallback.
    /// Test: itself.
    #[test]
    fn select_github_config_for_falls_back_to_global() {
        let config = TrustyToolsConfig {
            github: Some(gh("global")),
            projects: vec![project_config("https://github.com/acme/widget", None)],
            ..Default::default()
        };
        let selected = select_github_config_for(&config, "https://github.com/acme/widget");
        assert_eq!(selected, Some(&gh("global")));
    }

    /// Why: no match at all with no global tier either must resolve to
    /// `None` — the ambient case.
    /// Test: itself.
    #[test]
    fn select_github_config_for_no_match_is_global() {
        let config = TrustyToolsConfig::default();
        assert_eq!(
            select_github_config_for(&config, "https://github.com/someone/else"),
            None
        );
    }

    // ── resolve_for_config_enforced (#2081/#3312) ─────────────────────────
    //
    // A fake `gh` on `PATH` stands in for the real binary (this workspace's
    // established fake-binary-via-PATH test convention; mirrors
    // `core::gh_account_enforce`'s test module).

    fn fake_gh_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::core::trusty_tools_config::env_test_lock()
    }

    /// Drive `fut` to completion on a fresh current-thread runtime, blocking
    /// the CALLING thread synchronously (no `.await` in the caller's own
    /// frame). Lets these tests hold the plain `std::sync::MutexGuard` from
    /// [`fake_gh_lock`] for their entire body — including while
    /// `resolve_for_config_enforced` internally awaits its `spawn_blocking`
    /// check — without tripping `clippy::await_holding_lock` (that lint
    /// detects a guard held across a literal `.await` inside an async
    /// fn/block; a plain `#[test]` blockingly driving a future via
    /// `Runtime::block_on` has no such point). Using the SAME crate-wide
    /// `env_test_lock` (rather than a second, `tokio`-aware lock) is what
    /// actually matters here: it is the one lock every other PATH-mutating
    /// test in this test binary (`core::gh_account_enforce`'s tests included)
    /// already serialises on, so this module cannot race them.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime for test")
            .block_on(fut)
    }

    #[cfg(unix)]
    fn write_fake_gh(bin_dir: &std::path::Path, initial: &str, switch_fails: bool) {
        use std::os::unix::fs::PermissionsExt;
        let switch_exit = if switch_fails { 1 } else { 0 };
        let script = format!(
            r#"#!/bin/sh
STATE="$GH_CONFIG_DIR/.fake_active"
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  if [ -f "$STATE" ]; then ACTIVE=$(cat "$STATE"); else ACTIVE="{initial}"; fi
  echo "github.com"
  echo "  - Logged in to github.com account $ACTIVE (keyring)"
  echo "  - Active account: true"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "switch" ]; then
  if [ {switch_exit} -ne 0 ]; then exit 1; fi
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

    fn account_paired_config(config_dir: &std::path::Path, account: &str) -> TrustyToolsConfig {
        TrustyToolsConfig {
            github: Some(GithubConfig {
                config_dir: Some(config_dir.to_path_buf()),
                token_env: None,
                account: Some(account.to_string()),
                host: None,
            }),
            ..Default::default()
        }
    }

    /// Why (#3312): the exact #2081 incident shape (the resolved `config_dir`
    /// has the WRONG account active) must be self-healed before
    /// `resolve_for_config_enforced` returns `Ok` — proving the daemon's
    /// spawn paths now actually enforce this rather than merely resolving an
    /// identity blind to what's active inside it.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn resolve_for_config_enforced_self_heals_mismatch() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "wrong-account", false);
        let prior_path = prepend_path(bin_dir.path());

        let config = account_paired_config(config_dir.path(), "bobmatnyc");
        let result = block_on(resolve_for_config_enforced(
            &config,
            "https://github.com/acme/widget",
        ));

        restore_path(prior_path);
        assert!(result.is_ok(), "expected self-heal to succeed: {result:?}");
        let state = std::fs::read_to_string(config_dir.path().join(".fake_active"))
            .expect("state file written by the fake gh's switch");
        assert_eq!(state.trim(), "bobmatnyc");
    }

    /// Why (#3312): when the switch itself fails, `resolve_for_config_enforced`
    /// must hard-fail — never return a `GitIdentity` a caller could still
    /// clone/push with. This is the causality proof required for #3312:
    /// `resolve_for_config` ALONE (the exact pre-#3312 production path,
    /// still used unchanged by this very function internally) resolves `Ok`
    /// on this identical mismatched-account fixture, which is exactly the
    /// silent-wrong-account bug the issue describes. Only the NEW enforced
    /// wrapper catches it.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn resolve_for_config_enforced_fails_closed_on_switch_failure() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "wrong-account", true);
        let prior_path = prepend_path(bin_dir.path());

        let config = account_paired_config(config_dir.path(), "bobmatnyc");

        // The causal proof: the PLAIN pre-#3312 resolver is blind to the
        // mismatch and proceeds regardless.
        let unenforced = resolve_for_config(&config, "https://github.com/acme/widget");
        assert!(
            unenforced.is_ok(),
            "sanity: the plain resolver must still ignore active-account state: {unenforced:?}"
        );

        // The wired daemon path must NOT proceed.
        let enforced = block_on(resolve_for_config_enforced(
            &config,
            "https://github.com/acme/widget",
        ));

        restore_path(prior_path);
        let err = enforced.expect_err("mismatched account with a failed switch must be an Err");
        assert!(err.to_string().contains("bobmatnyc"), "err: {err}");
    }

    /// Why (#3312): a project with `config_dir` but no paired `account` (the
    /// pre-#3312, still-supported shape) must see NO enforcement — proven by
    /// making every `gh` invocation fail and asserting resolution still
    /// succeeds (only possible if `gh` was never invoked).
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn resolve_for_config_enforced_skips_when_account_unset() {
        let _g = fake_gh_lock();
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let config_dir = tempfile::tempdir().expect("config tempdir");
        write_fake_gh(bin_dir.path(), "irrelevant", true);
        let prior_path = prepend_path(bin_dir.path());

        let config = TrustyToolsConfig {
            github: Some(gh(&config_dir.path().display().to_string())),
            ..Default::default()
        };
        let result = block_on(resolve_for_config_enforced(
            &config,
            "https://github.com/acme/widget",
        ));

        restore_path(prior_path);
        assert!(
            result.is_ok(),
            "no account paired with config_dir must skip enforcement entirely: {result:?}"
        );
    }

    // ── commit_config_args ────────────────────────────────────────────────

    /// Why: an unconfigured identity must render NO `-c` args, so a git
    /// invocation for a project with no commit-identity override is
    /// byte-for-byte identical to pre-#2184 behaviour.
    /// Test: itself.
    #[test]
    fn commit_config_args_empty_when_unset() {
        assert!(GitIdentity::default().commit_config_args().is_empty());
    }

    /// Why: both fields set must render both `-c` pairs, name before email.
    /// Test: itself.
    #[test]
    fn commit_config_args_both_set() {
        let identity = GitIdentity {
            commit_name: Some("Bot".into()),
            commit_email: Some("bot@example.com".into()),
            ..Default::default()
        };
        assert_eq!(
            identity.commit_config_args(),
            vec![
                "-c".to_string(),
                "user.name=Bot".to_string(),
                "-c".to_string(),
                "user.email=bot@example.com".to_string(),
            ]
        );
    }

    /// Why: only the set field must render — no spurious empty override.
    /// Test: itself.
    #[test]
    fn commit_config_args_name_only() {
        let identity = GitIdentity {
            commit_name: Some("Bot".into()),
            ..Default::default()
        };
        assert_eq!(
            identity.commit_config_args(),
            vec!["-c".to_string(), "user.name=Bot".to_string()]
        );
    }
}
