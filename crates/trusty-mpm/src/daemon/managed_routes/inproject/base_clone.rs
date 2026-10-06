//! The managed base clone: clone `origin` into `<repos_root>/<owner>/<repo>` once.
//!
//! Why: split out of `inproject.rs` (#9091) so the production module stays
//! under the 500-SLOC cap once the clone gained its account seam.
//! What: [`ensure_base_clone`] and its injectable form
//! [`ensure_base_clone_with`].
//! Test: `inproject/tests.rs` (the clone itself) and
//! `inproject/base_clone_tests.rs` (which account it clones as).

use std::path::Path;

use tracing::info;

use super::account_clone::{AccountCloneEnv, account_clone_env};
use super::{ensure_worktrees_gitignored, migrate_old_layout_aside};
use crate::core::gh_account_registry::RegistryPin;
use crate::core::gh_org_accounts::{OrgAccounts, OrgAccountsError};
use crate::core::remote_url_redact::{redact_stored_url, redact_url};

/// Ensure a base clone exists at `base_path`, cloning from `origin_url` if not.
///
/// Why: the first session against a repo triggers a one-time clone; subsequent
/// sessions reuse the same base directory and only add worktrees.
/// What: if `base_path/.git` exists, calls `ensure_worktrees_gitignored` and
/// returns `Ok(())` (idempotent). Otherwise it first migrates any pre-#1803
/// old-layout dir aside via [`migrate_old_layout_aside`] (#1805 — so a non-empty,
/// non-git directory no longer makes the clone fail and silently fall back to the
/// legacy full-clone-per-session path), then runs
/// `git clone --no-local <origin_url> <base_path>` and calls
/// `ensure_worktrees_gitignored`. A clone failure returns `Err` with the
/// command's stderr.
///
/// `account` (#7166) selects which logged-in `gh` identity the clone
/// authenticates as. #9091: `None` is resolved by
/// [`crate::core::gh_org_accounts::resolve_gh_account`] — the registry pin for
/// the origin, then the `[accounts]` org map for a github.com owner, then the
/// ambient identity (whatever `git`'s own credential resolution — `GH_TOKEN`,
/// SSH agent, credential helper — finds), so the clone runs as the account the
/// session will spawn as. A resolved login gets that account's credentials via
/// [`super::account_clone::account_clone_env`] BEFORE cloning, which (1) mints
/// a token through [`crate::core::gh_account::resolve_gh_account_env_with`]
/// (config_dir-first, `gh auth token -u <login>` as the fallback — the SAME
/// precedence the session's own spawn-time `GH_TOKEN` uses), (2) VERIFIES
/// that token actually belongs to `login` via a bounded `gh api user --jq
/// .login` call before it is ever applied — `gh auth token -u` does not
/// discriminate between logged-in accounts on a keyring-backed host, so
/// without this step the clone could silently authenticate as a DIFFERENT
/// account than the one requested (#7166 critic BLOCK; a mismatch, a
/// not-logged-in account, or a verification timeout all fail loud here,
/// naming the account, rather than reaching `git clone` and failing with
/// GitHub's generic "Repository not found" or succeeding under the wrong
/// identity), and (3) runs the clone with the resolved env set and every
/// [`crate::core::gh_identity::GH_INHERITED_IDENTITY_ENV`] var PLUS
/// `GH_HOST` removed from the child first, so an exported credential
/// belonging to a different account/host can never win over the explicit
/// selection. The token is never written to argv or into `origin_url`: it
/// reaches `git`'s already-configured `!gh auth git-credential` helper
/// exclusively through the child's environment.
///
/// 🟡 Known limit: `config_dir` is always `None` at this call site (this
/// cold-start clone has no already-registered project to read a pinned
/// `github.config_dir` from), so on a keyring-backed host the requested
/// account must ALSO be `gh`'s machine-global active account — otherwise the
/// verification step above refuses, correctly, rather than cloning under the
/// wrong identity. Pinning a `github.config_dir` (via `tm projects register
/// --gh-account <login> --gh-config-dir <dir>`) is the operator-facing
/// workaround today; wiring a config_dir-aware caller into this cold-start
/// path is the follow-up.
/// Test: see [`super::account_clone`]'s and
/// `ensure_base_clone_with_no_account_is_the_pre_7166_shape`'s
/// (`inproject/tests.rs`) own doc comments for the exact test names, and
/// [`ensure_base_clone_with`]'s.
pub fn ensure_base_clone(
    origin_url: &str,
    base_path: &Path,
    account: Option<&str>,
) -> Result<(), String> {
    ensure_base_clone_with(
        origin_url,
        base_path,
        account,
        || {
            crate::core::gh_account_registry::read_pin(
                &crate::project::registry_data_dir(),
                origin_url,
            )
        },
        OrgAccounts::load_default,
        account_clone_env,
    )
}

/// [`ensure_base_clone`] with the registry pin, the `[accounts]` loader and the
/// clone-credential step given (#9091).
///
/// Why: the account a clone runs as is decided by host state — the registry,
/// `~/.trusty-mpm/config.toml`, a logged-in `gh` — that no test may read.
/// What: on an actual clone only, the account is resolved by
/// [`crate::core::gh_org_accounts::resolve_gh_account_with`] and its clone env
/// built BEFORE anything touches the disk or announces the clone, so a refusal
/// (a broken table, an unreadable registry, an account with no credential)
/// leaves the filesystem exactly as it was.
/// Test: `a_mapped_org_clones_as_the_mapped_account`,
/// `a_pinned_project_clones_as_the_pin_not_the_map`,
/// `a_broken_accounts_table_refuses_the_clone_and_touches_nothing`,
/// `a_credentialed_origin_never_reaches_the_log_or_the_error` (#9124)
/// (`inproject/base_clone_tests.rs`).
pub(crate) fn ensure_base_clone_with(
    origin_url: &str,
    base_path: &Path,
    account: Option<&str>,
    pin: impl FnOnce() -> Result<Option<RegistryPin>, String>,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
    clone_env: impl FnOnce(&str) -> Result<AccountCloneEnv, String>,
) -> Result<(), String> {
    if base_path.join(".git").exists() {
        info!(
            path = %base_path.display(),
            "inproject: base clone already present, reusing"
        );
        ensure_worktrees_gitignored(base_path)?;
        return Ok(());
    }

    // #9091: choose the account and build its env before anything below runs,
    // so a refusal changes nothing on disk and emits no clone stage.
    let aliases = crate::session_manager::ssh_host_alias::SshHostAliases::for_current_user();
    let resolved = crate::core::gh_org_accounts::resolve_gh_account_with(
        account, origin_url, &aliases, pin, load,
    )
    .map_err(|e| format!("inproject: cannot choose the gh account to clone with: {e}"))?;
    let env = match resolved.as_ref().map(|r| r.login.as_str()) {
        Some(login) => {
            Some(clone_env(login).map_err(|e| format!("inproject: cannot clone as {login}: {e}"))?)
        }
        None => None,
    };

    // #1805: a pre-existing old-layout dir (non-empty, no top-level `.git`) would
    // otherwise make `git clone` fail; migrate it aside first so the fresh clone
    // succeeds instead of silently falling back to full-clone-per-session.
    migrate_old_layout_aside(base_path)?;

    // Announce the clone stage (issue #1904, #1919). Placed here — after the
    // idempotent `.git`-exists reuse check above returns early — so the event
    // fires ONLY on an actual first-run clone, mirroring
    // the (since removed, ADR-0055) clone provisioner's placement, which
    // always cloned and so emitted unconditionally. This is the exact "tm: first run for X —
    // cloning into ..., this may take a few minutes…" scenario #1904 set out
    // to make observable, and #1919 found it was never wired up here. No-op
    // unless the daemon's `spawn_managed` wrapped this call tree in
    // `provisioning_stage::scoped` (see that module's doc).
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::CloningRepo,
    );

    // #9124: `origin_url` may carry `user:token@`; the log gets it redacted.
    info!(
        url = %redact_stored_url(origin_url),
        dest = %base_path.display(),
        "inproject: cloning base repo"
    );

    if let Some(parent) = base_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "inproject: failed to create parent dir {}: {e}",
                parent.display()
            )
        })?;
    }

    // Use the parent of base_path (just created above) as cwd so a deleted
    // inherited cwd cannot cause git to fail at startup with "fatal: Unable to
    // read current working directory" (exit 128) → HTTP 500 on managed-spawn.
    let cwd = base_path.parent().unwrap_or(std::path::Path::new("/"));
    // #7182: built via the shared entry point (maintenance/gc disabled on
    // this one invocation) rather than a bare `Command::new("git")`.
    let mut cmd = trusty_common::git::command();
    cmd.args(["clone", "--no-local", origin_url])
        .arg(base_path)
        .current_dir(cwd);
    if let Some(env) = &env {
        env.apply(&mut cmd);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("inproject: git clone failed to spawn: {e}"))?;

    if !out.status.success() {
        // #9124: git's stderr can quote the URL it was given. #9259: it is
        // free text, so `redact_url`, not `redact_stored_url`.
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "inproject: git clone failed ({}): {}",
            out.status,
            redact_url(&stderr)
        ));
    }
    info!(dest = %base_path.display(), "inproject: base clone complete");
    ensure_worktrees_gitignored(base_path)?;
    crate::core::harness_exclude::ensure_and_log(base_path); // #8511
    // #7171: disable git's own background maintenance/gc on this FRESHLY
    // CLONED base — every worktree the base will ever host shares its
    // GIT_COMMON_DIR config, so one write here covers operator-run git in all
    // of them, the same sharing property the push guard below relies on.
    crate::core::git_maintenance::disable_and_log(base_path);
    // #2867: install the cross-branch push guard into this FRESHLY CLONED base.
    // `$GIT_COMMON_DIR/hooks` is shared by every worktree of a base clone —
    // provisioner-created and ad-hoc `git worktree add` alike (verified
    // empirically; see `crate::core::push_guard`) — so one file installed at
    // clone time protects every session this base will ever host, including the
    // agent-created worktrees no trusty-mpm code path ever configures. Scoping
    // the install to the fresh-clone path deliberately keeps it out of ALREADY
    // PROVISIONED bases: those are shared by concurrent live sessions, and
    // changing hook behaviour underneath them is an operator decision, not a
    // side effect of a session launch. That decision has a supported path —
    // `tm doctor`'s `push_guard` check reports an unprotected clone and
    // `tm repair push-guard` retrofits it — so "not automatic" does not mean
    // "not reachable".
    crate::core::push_guard::install_and_log(base_path);
    Ok(())
}

#[cfg(test)]
#[path = "base_clone_tests.rs"]
mod tests;
