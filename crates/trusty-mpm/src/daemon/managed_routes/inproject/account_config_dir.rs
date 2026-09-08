//! Per-account `gh` config directory bootstrap (#7166, owner ruling "do B").
//!
//! Why: `resolve_gh_account_env_with`'s `config_dir` arm is the ONLY selector
//! that actually discriminates between logged-in accounts on a keyring-backed
//! `gh` install (`core::gh_account`'s own #5851 rationale for demoting `-u`)
//! — but nothing built one automatically for the `--account <login>` clone
//! selector; #7166's round-3 fix could only verify a possibly-wrong `-u`
//! token, never select the right one on such a host without a manual
//! `--gh-config-dir` pin. This module builds and reuses ONE isolated config
//! dir per known account, so the selector works out of the box.
//!
//! Layout: `<tm state root>/gh-accounts/<login>/` — `resolve_gh_account_env_with`'s
//! own `config_dir` arm (`core/gh_account.rs`, #5851's PR) accepts ANY
//! operator-chosen directory and defines no per-account convention of its
//! own, so this is a fresh layout, not a pre-existing one being reused.
//! `<tm state root>` is `crate::core::paths::FrameworkPaths::default().root`
//! (`~/.trusty-mpm`) in production; every function here takes it as a
//! parameter so tests point it at a tempdir instead.
//!
//! What: [`ensure_account_config_dir`] is the pure(ish), fully-injectable
//! core (real filesystem I/O, but every path is a parameter — no ambient
//! `$HOME`/`GH_CONFIG_DIR` read); [`ensure_account_config_dir_default`] is
//! the production entry point, resolving both the state root and the
//! operator's own `gh` config dir the same way `gh` itself does
//! ([`crate::core::gh_account::gh_config_dir`]).
//!
//! On first use for a never-before-selected `login`: reads the OPERATOR's
//! `hosts.yml` (never mutated) to confirm `login` is actually logged in —
//! refusing, dir untouched, when it is not — copies `config.yml` if present,
//! and writes a NEW `hosts.yml` naming only `login`. No token is ever
//! copied: `gh`'s credential store keys by login in the OS keyring/git
//! credential store, so the freshly-written `hosts.yml` alone is enough for
//! `gh auth token` (run with `GH_CONFIG_DIR` pointed at this dir) to resolve
//! that account's own credential. `gh auth switch` is never called — the
//! isolation comes from a SEPARATE config dir, not from mutating the shared
//! one's active pointer.
//!
//! Test: `account_config_dir_tests.rs`.

use std::path::{Path, PathBuf};

/// Directory name under the tm state root holding one subdirectory per known
/// account.
///
/// Test: `ensure_account_config_dir_places_it_under_gh_accounts`.
pub(super) const GH_ACCOUNTS_DIR_NAME: &str = "gh-accounts";

/// The `<state_root>/gh-accounts/<login>` path for `login`.
///
/// Why: split out so both [`ensure_account_config_dir`] and its callers can
/// name the SAME path without recomputing the join.
/// What: `state_root.join("gh-accounts").join(login)`.
/// Test: `ensure_account_config_dir_places_it_under_gh_accounts`.
pub(super) fn account_config_dir(state_root: &Path, login: &str) -> PathBuf {
    state_root.join(GH_ACCOUNTS_DIR_NAME).join(login)
}

/// Reject a `login` value that would escape or corrupt the intended
/// `<state_root>/gh-accounts/<login>` join (#7166 review follow-up MEDIUM).
///
/// Why: `is_name_segment` (`bin/tm/commands/register_args.rs`) only restricts
/// the CHARACTER SET (alnum, `.`, `_`, `-`) — it does not reject the exact
/// strings `.`/`..`, so `--account ..` passes CLI validation and resolves to
/// `<state_root>/gh-accounts/..` = `<state_root>` itself. That this is
/// currently unexploitable is incidental to a check written for a different
/// purpose ([`crate::core::gh_account::GhAccountStatus::
/// canonical_logged_in_login`] requires an exact, case-insensitive match
/// against a real, already-logged-in `gh` account, and no GitHub login can be
/// `.`/`..`) — not a guard at THIS path-construction site.
/// What: refuses `login` when it is empty, exactly `.` or `..`, or contains a
/// path separator (`/` or `\`), independent of whatever the caller already
/// validated.
/// Test: `ensure_account_config_dir_refuses_dot`,
/// `ensure_account_config_dir_refuses_dotdot`,
/// `ensure_account_config_dir_refuses_empty`,
/// `ensure_account_config_dir_refuses_a_forward_slash`,
/// `ensure_account_config_dir_refuses_a_backslash`.
fn reject_unsafe_login_segment(login: &str) -> Result<(), String> {
    if login.is_empty()
        || login == "."
        || login == ".."
        || login.contains('/')
        || login.contains('\\')
    {
        return Err(format!(
            "'{login}' is not a valid account login for a config directory segment"
        ));
    }
    Ok(())
}

/// `true` when `path` exists and is ITSELF a symlink (#7166 review follow-up
/// LOW) — checked with `symlink_metadata`, which does NOT follow the link,
/// unlike the `is_file`/`exists` calls used elsewhere in this module.
/// Test: `ensure_account_config_dir_refuses_a_symlinked_dir`,
/// `ensure_account_config_dir_refuses_a_symlinked_hosts_yml`.
fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Restrict `dir` to owner-only access (#7166 review follow-up MEDIUM) —
/// `gh`'s own `hosts.yml` is written 0600 because it normally holds a token;
/// this directory never holds one, but it does reveal which GitHub accounts
/// are used with `tm` and copies the operator's real `config.yml` verbatim,
/// so it must not be left at the ambient umask on a shared host.
/// Test: `ensure_account_config_dir_sets_restrictive_permissions`.
#[cfg(unix)]
fn set_private_dir_permissions(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot set permissions on {}: {e}", dir.display()))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_dir: &Path) -> Result<(), String> {
    Ok(())
}

/// Restrict a written file (`hosts.yml`/`config.yml`) to owner-only access
/// (#7166 review follow-up MEDIUM) — same rationale as
/// [`set_private_dir_permissions`].
/// Test: `ensure_account_config_dir_sets_restrictive_permissions`.
#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot set permissions on {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Ensure a per-account `gh` config dir exists for `login`, building it from
/// `operator_gh_config_dir` on first use.
///
/// Why: the injectable-paths seam this codebase uses throughout for `gh`/
/// filesystem I/O that cannot run hermetically against the REAL `$HOME` —
/// tests point both paths at tempdirs and a fake `operator_gh_config_dir`.
/// What: `<state_root>/gh-accounts/<login>/hosts.yml` existing already means
/// "reused untouched" — returns `Ok(dir)` immediately, no filesystem writes.
/// Otherwise reads `<operator_gh_config_dir>/hosts.yml`; a missing/unparseable
/// file, or one that does not name `login` (case-insensitively; matches
/// [`crate::core::gh_account::GhAccountStatus::canonical_logged_in_login`]'s
/// convention) is a refusal naming the `gh auth login` remedy — the directory
/// is NEVER created in that case. Otherwise creates the directory, copies
/// `config.yml` when the operator has one (absent is not an error — `gh`
/// tolerates a config dir with no `config.yml`), and writes a fresh
/// `hosts.yml` naming ONLY the canonical spelling of `login` (no token —
/// `gh`'s credential store keys by login independently of this file).
/// Test: `ensure_account_config_dir_builds_from_operator_hosts_yml`,
/// `ensure_account_config_dir_refuses_an_unknown_login`,
/// `ensure_account_config_dir_reuses_an_existing_dir_untouched`,
/// `ensure_account_config_dir_copies_config_yml_when_present`,
/// `ensure_account_config_dir_tolerates_a_missing_config_yml`.
pub(super) fn ensure_account_config_dir(
    state_root: &Path,
    operator_gh_config_dir: &Path,
    login: &str,
) -> Result<PathBuf, String> {
    // #7166 review follow-up MEDIUM: reject before the path is even joined —
    // see the function's own doc for why the CLI's `is_name_segment` check is
    // not a substitute for this.
    reject_unsafe_login_segment(login)?;
    let dir = account_config_dir(state_root, login);

    // #7166 review follow-up LOW: refuse a pre-planted symlink before any
    // create/write. Checked on `dir` itself AND on `hosts.yml` — the latter
    // because `is_file()` below FOLLOWS a symlink, so a symlinked `hosts.yml`
    // pointing at a real file would otherwise satisfy the "already built,
    // reuse untouched" check without ever being detected as a symlink.
    if is_symlink(&dir) {
        return Err(format!(
            "{} is a symlink — refusing to use it as a per-account gh config directory; \
             remove it and retry",
            dir.display()
        ));
    }
    let hosts_yml_path = dir.join("hosts.yml");
    if is_symlink(&hosts_yml_path) {
        return Err(format!(
            "{} is a symlink — refusing to use it as a per-account gh config file; remove it \
             and retry",
            hosts_yml_path.display()
        ));
    }
    if hosts_yml_path.is_file() {
        return Ok(dir);
    }

    let operator_hosts_path = operator_gh_config_dir.join("hosts.yml");
    let operator_hosts_text = std::fs::read_to_string(&operator_hosts_path).map_err(|e| {
        format!(
            "cannot read {} to select account '{login}': {e}. Run `gh auth login` first.",
            operator_hosts_path.display()
        )
    })?;
    let status =
        crate::core::gh_account::parse_gh_account_status_from_hosts_yml(&operator_hosts_text)
            .ok_or_else(|| {
                format!(
                    "{} does not name any github.com account. Run `gh auth login` first.",
                    operator_hosts_path.display()
                )
            })?;
    let canonical = status.canonical_logged_in_login(login).ok_or_else(|| {
        let known = if status.logged_in.is_empty() {
            "none".to_string()
        } else {
            status.logged_in.join(", ")
        };
        format!(
            "'{login}' is not logged into gh on this host (known accounts: {known}). Run \
             `gh auth login --hostname github.com` (or `gh auth switch --user {login}` if it \
             is already logged in under a different case) to authenticate it first."
        )
    })?;

    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // #7166 review follow-up MEDIUM: 0700 before anything is written into it.
    set_private_dir_permissions(&dir)?;

    let operator_config_path = operator_gh_config_dir.join("config.yml");
    if operator_config_path.is_file() {
        let copied_config_path = dir.join("config.yml");
        std::fs::copy(&operator_config_path, &copied_config_path).map_err(|e| {
            format!(
                "cannot copy {} to {}: {e}",
                operator_config_path.display(),
                dir.display()
            )
        })?;
        set_private_file_permissions(&copied_config_path)?;
    }

    let hosts_yml = render_single_account_hosts_yml(&canonical);
    std::fs::write(&hosts_yml_path, hosts_yml)
        .map_err(|e| format!("cannot write {}: {e}", hosts_yml_path.display()))?;
    set_private_file_permissions(&hosts_yml_path)?;

    Ok(dir)
}

/// Render a `hosts.yml` naming exactly one github.com account.
///
/// Why: split out as a pure function so its exact shape — parseable by
/// [`crate::core::gh_account::parse_gh_account_status_from_hosts_yml`], the
/// SAME parser `gh` itself effectively mirrors — is asserted without any
/// filesystem I/O.
/// What: `github.com:\n    users:\n        <login>:\n    user: <login>\n`.
/// No `git_protocol` line — `gh` defaults it, and omitting it keeps this
/// writer from having an opinion `gh`'s own `hosts.yml` schema does not
/// require.
/// Test: `render_single_account_hosts_yml_round_trips_through_the_parser`.
fn render_single_account_hosts_yml(login: &str) -> String {
    format!("github.com:\n    users:\n        {login}:\n    user: {login}\n")
}

/// Production entry point: resolve the tm state root and the operator's own
/// `gh` config dir the same way `gh` itself does, then delegate to
/// [`ensure_account_config_dir`].
///
/// Why: the one non-test call site — every other function in this module
/// takes its paths as parameters specifically so this is the ONLY place that
/// reads `$HOME`/`GH_CONFIG_DIR`/`XDG_CONFIG_HOME`.
/// What: state root is
/// [`crate::core::paths::FrameworkPaths::default`]`().root` (`~/.trusty-mpm`);
/// the operator's `gh` config dir is
/// [`crate::core::gh_account::gh_config_dir`] (honours `GH_CONFIG_DIR` /
/// `XDG_CONFIG_HOME`, else `~/.config/gh`), or a synthetic
/// `~/.config/gh` when even `dirs::home_dir()` fails (matches
/// `gh_config_dir`'s own last-resort fallback shape).
/// Test: none directly (thin, ambient-environment wiring); the logic it
/// delegates to is covered by [`ensure_account_config_dir`]'s tests.
pub(super) fn ensure_account_config_dir_default(login: &str) -> Result<PathBuf, String> {
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    let operator_gh_config_dir = crate::core::gh_account::gh_config_dir()
        .unwrap_or_else(|| PathBuf::from(".config").join("gh"));
    ensure_account_config_dir(&state_root, &operator_gh_config_dir, login)
}

#[cfg(test)]
#[path = "account_config_dir_tests.rs"]
mod tests;
