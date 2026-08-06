//! Cold start: a bare `owner/repo` string becomes a managed base clone (#4990).
//!
//! Why: [`super::inproject::try_inproject_spawn`] can only start a managed
//! session from a checkout that ALREADY exists — it gates on
//! `path.join(".git").exists()` and reads `remote.origin.url` to learn the
//! GitHub identity. Nothing turned a string the operator types into that
//! checkout, so `tm` could not begin work on a repo that is not already on
//! disk. This module is the missing direction: identity first, checkout
//! second, then hand off to the same session-launch path an in-project spawn
//! uses.
//!
//! What: [`ensure_managed_checkout`] resolves `<repos_root>/<owner>/<repo>` via
//! [`super::inproject::base_clone_path`] and drives
//! [`super::inproject::ensure_base_clone`] against it, returning the path and
//! whether an existing checkout was reused. Reuse is gated by two checks that
//! FAIL LOUD rather than proceeding:
//!
//! 1. **Remote mismatch.** An existing checkout whose `origin` names a
//!    different repository is an error. trusty-mpm never re-points a remote —
//!    doing so silently would make `tm run <owner>/<repo>` operate on some
//!    other repository under that repository's name.
//! 2. **Dirty tree.** An existing checkout with uncommitted changes cannot be
//!    refreshed, so it is an error rather than a silent no-op on stale
//!    content. This is the shape `core::standalone::load::pull_ff_only` gets
//!    wrong: it warns and returns `Ok(())`, so a failed refresh is
//!    indistinguishable from a successful one.
//!
//! Only after both clear does the refresh run, through
//! [`super::inproject_hygiene::run_hygiene_for_base`] — the crate's single
//! non-destructive base-clone refresh (fetch, gated fast-forward, worktree
//! prune). Its own gates then have nothing left to decline for.
//!
//! Test: `inproject_cold_start_tests.rs` (pure canonicalization + the two
//! loud-failure paths against real temp git repos);
//! `tests/inproject_cold_start.rs` (fresh clone → the shape
//! `try_inproject_spawn` requires).

use std::path::{Path, PathBuf};

use tracing::info;

use super::{inproject, inproject_hygiene};

/// How many working-tree entries a [`ColdStartError::DirtyCheckout`] message
/// lists before it summarises the rest.
///
/// Why: a managed checkout can be thousands of lines dirty; an error that
/// scrolls the terminal hides its own first line, which is the remedy.
/// What: `10`.
/// Test: `dirty_message_truncates_long_status`.
const DIRTY_ENTRY_PREVIEW: usize = 10;

/// Why a cold start refused. Every variant is a loud stop, never a warning.
///
/// Why: the two reuse hazards this module exists to close are both
/// silent-success shapes elsewhere in the codebase, so they are modelled as
/// errors that carry the offending path and what is wrong with it — not as
/// booleans a caller could ignore.
/// What: `RemoteMismatch` and `DirtyCheckout` are the two decided fail-loud
/// cases; `NoOrigin` and `StatusUnavailable` are the fail-safe directions for
/// a checkout whose state cannot be established; `Provision` wraps the
/// underlying clone/hygiene failure verbatim.
/// Test: `existing_checkout_on_a_different_remote_fails_loud`,
/// `dirty_existing_checkout_fails_loud`.
#[derive(Debug, thiserror::Error)]
pub enum ColdStartError {
    /// The existing managed checkout belongs to a different repository.
    #[error(
        "managed checkout {} already exists and its origin is {found}, not {requested}.\n\
         trusty-mpm will not re-point a checkout's remote — that would run a different \
         repository under this one's name. Move or remove that directory, or ask for the \
         repo that is actually there.",
        .path.display()
    )]
    RemoteMismatch {
        /// The managed checkout that was inspected.
        path: PathBuf,
        /// The `remote.origin.url` found there.
        found: String,
        /// The clone URL the caller asked for.
        requested: String,
    },

    /// The existing managed checkout has uncommitted changes.
    #[error(
        "managed checkout {} has uncommitted changes, so it cannot be refreshed:\n{}\n\
         Refusing to start a session on content that may be stale. Commit, stash, or \
         discard those changes and run the command again.",
        .path.display(),
        summarize_entries(.entries)
    )]
    DirtyCheckout {
        /// The managed checkout that was inspected.
        path: PathBuf,
        /// `git status --porcelain` lines, verbatim.
        entries: Vec<String>,
    },

    /// The existing managed checkout has no `remote.origin.url`.
    #[error(
        "managed checkout {} exists but has no remote.origin.url, so it cannot be matched \
         against {requested}. Move or remove that directory and run the command again.",
        .path.display()
    )]
    NoOrigin {
        /// The managed checkout that was inspected.
        path: PathBuf,
        /// The clone URL the caller asked for.
        requested: String,
    },

    /// git could not report the state of the existing managed checkout.
    #[error(
        "cannot read the git state of managed checkout {}: {detail}",
        .path.display()
    )]
    StatusUnavailable {
        /// The managed checkout that was inspected.
        path: PathBuf,
        /// The underlying git failure.
        detail: String,
    },

    /// Cloning or refreshing the base clone failed.
    #[error("{0}")]
    Provision(String),
}

/// The managed base clone a cold start produced.
///
/// Why: the caller needs the path to launch into, and needs to say whether it
/// cloned or reused so the operator can tell a first run from a repeat one.
/// What: the absolute `<repos_root>/<owner>/<repo>` path, plus `reused`.
/// Test: `fresh_clone_yields_the_shape_try_inproject_spawn_requires`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedCheckout {
    /// Absolute path to the managed base clone.
    pub base_path: PathBuf,
    /// `true` when an existing checkout was verified and reused rather than cloned.
    pub reused: bool,
}

/// Ensure a managed base clone exists for `owner`/`repo`, cloning from `clone_url`.
///
/// Why: this is the cold-start entry point — the one call that takes an
/// identity with no local checkout behind it and returns a directory the
/// existing session-launch path can accept.
/// What: resolves the base path via [`super::inproject::base_clone_path`]
/// (the SAME resolver the daemon and `tm launch` use, so all three agree on
/// where a project lives) and delegates to [`ensure_managed_checkout_at`].
/// Test: `tests/inproject_cold_start.rs`.
pub fn ensure_managed_checkout(
    owner: &str,
    repo: &str,
    clone_url: &str,
) -> Result<ManagedCheckout, ColdStartError> {
    let base_path = inproject::base_clone_path(owner, repo);
    ensure_managed_checkout_at(&base_path, clone_url)
}

/// Ensure a managed base clone exists at an explicit `base_path`.
///
/// Why: split from [`ensure_managed_checkout`] so tests can supply a temp
/// directory directly rather than mutating `TRUSTY_MPM_REPOS_ROOT`, which
/// races other threads in the same test binary.
/// What: when `base_path/.git` is absent, clones. When it is present, runs the
/// remote check then the dirty check — both fail loud — and only then
/// refreshes through [`super::inproject_hygiene::run_hygiene_for_base`].
/// Either way it finishes through [`super::inproject::ensure_base_clone`], so
/// a reused checkout gets the same `.worktrees/` exclusion invariant a fresh
/// clone does.
///
/// ORDERING IS THE POINT. Both guards run BEFORE anything writes to the
/// directory, so a mismatched or dirty checkout is never fetched into, merged
/// into, or otherwise touched.
/// Test: `existing_checkout_on_a_different_remote_fails_loud`,
/// `dirty_existing_checkout_fails_loud`,
/// `clean_matching_checkout_is_reused_and_refreshed`.
pub fn ensure_managed_checkout_at(
    base_path: &Path,
    clone_url: &str,
) -> Result<ManagedCheckout, ColdStartError> {
    let reused = base_path.join(".git").exists();

    if reused {
        verify_remote_matches(base_path, clone_url)?;
        verify_tree_is_clean(base_path)?;

        // Refresh through the crate's single base-clone hygiene entry point.
        // It is non-destructive by construction (fetch, gated fast-forward,
        // worktree prune) and its dirty/ahead gates have nothing to decline
        // for here, because `verify_tree_is_clean` already stopped that case.
        info!(
            path = %base_path.display(),
            "cold-start: reusing managed checkout; refreshing"
        );
        inproject_hygiene::run_hygiene_for_base(base_path).map_err(ColdStartError::Provision)?;
    }

    inproject::ensure_base_clone(clone_url, base_path).map_err(ColdStartError::Provision)?;

    Ok(ManagedCheckout {
        base_path: base_path.to_path_buf(),
        reused,
    })
}

/// Refuse when an existing checkout's `origin` names a different repository.
///
/// Why: nothing checks this today. `core::standalone::load` pulls from
/// whatever `origin` happens to be configured, so re-registering an alias to a
/// new URL keeps pulling the old remote forever — silently, and with the new
/// URL's name on it. Auto-fixing is worse than refusing: re-pointing `origin`
/// under a directory that may hold branches, worktrees, and unpushed commits
/// from the OTHER repository is not a repair.
/// What: reads `remote.origin.url` via
/// [`super::inproject::get_origin_url`] and compares it to `requested` through
/// [`canonical_remote`], so remote spellings of the same repo agree. A
/// checkout with no origin at all is `NoOrigin`, not a match.
/// Test: `existing_checkout_on_a_different_remote_fails_loud`,
/// `equivalent_remote_spellings_match`.
fn verify_remote_matches(base_path: &Path, requested: &str) -> Result<(), ColdStartError> {
    let Some(found) = inproject::get_origin_url(base_path) else {
        return Err(ColdStartError::NoOrigin {
            path: base_path.to_path_buf(),
            requested: requested.to_string(),
        });
    };
    if canonical_remote(&found) == canonical_remote(requested) {
        return Ok(());
    }
    Err(ColdStartError::RemoteMismatch {
        path: base_path.to_path_buf(),
        found,
        requested: requested.to_string(),
    })
}

/// Refuse when an existing checkout has uncommitted changes.
///
/// Why: a dirty tree cannot be fast-forwarded, so proceeding means running the
/// session against whatever the checkout was last left at. `pull_ff_only`
/// (`core::standalone::load.rs`) does exactly that — it prints a warning and
/// returns `Ok(())`, making a failed refresh indistinguishable from a
/// successful one at the call site.
///
/// The check is `git status --porcelain` WITHOUT `--ignored`, deliberately:
/// the question is whether tracked work is at risk of being outrun, and
/// gitignored build output is not that. The gitignored-collision hazard has
/// its own, narrower guard inside the hygiene sweep
/// (`inproject_hygiene::colliding_untracked_paths`, #4961), which runs against
/// the specific paths an update would write.
/// What: `Ok(())` on empty output; `DirtyCheckout` carrying the porcelain
/// lines otherwise; `StatusUnavailable` when git itself fails, which is the
/// fail-safe direction — an unreadable checkout is never assumed clean.
/// Test: `dirty_existing_checkout_fails_loud`,
/// `clean_matching_checkout_is_reused_and_refreshed`.
fn verify_tree_is_clean(base_path: &Path) -> Result<(), ColdStartError> {
    match inproject_hygiene::porcelain_status(base_path) {
        Ok(entries) if entries.is_empty() => Ok(()),
        Ok(entries) => Err(ColdStartError::DirtyCheckout {
            path: base_path.to_path_buf(),
            entries,
        }),
        Err(detail) => Err(ColdStartError::StatusUnavailable {
            path: base_path.to_path_buf(),
            detail,
        }),
    }
}

/// Reduce a git remote URL to a comparable `host/owner/repo` token.
///
/// Why: the remote check must not fire on spelling. `git@github.com:o/r.git`,
/// `https://github.com/o/r`, and `ssh://git@github.com/o/r.git` are one
/// repository, and a mismatch error for any pair of them would be a false
/// alarm on the most common setup there is.
///
/// This is deliberately NOT
/// [`trusty_common::github_path::parse_github_path`], which drops the host on
/// purpose: two different hosts serving the same `owner/repo` are DIFFERENT
/// repositories, and that is precisely the confusion this check exists to
/// catch.
/// What: lower-cases, strips the scheme and any `user@` credential, rewrites
/// the scp-syntax `:` separator to `/`, and trims a trailing `.git` and
/// slashes. An explicit `:port` is rewritten to a path segment, so
/// `https://host:443/o/r` and `https://host/o/r` compare UNEQUAL — a false
/// mismatch, which fails loud rather than silently proceeding, the safe
/// direction for a check whose whole job is to refuse.
/// Test: `equivalent_remote_spellings_match`, `different_hosts_do_not_match`.
pub fn canonical_remote(url: &str) -> String {
    let lowered = url.trim().to_ascii_lowercase();
    let after_scheme = match lowered.find("://") {
        Some(i) => &lowered[i + 3..],
        None => &lowered[..],
    };
    let after_creds = match after_scheme.find('@') {
        Some(i) => &after_scheme[i + 1..],
        None => after_scheme,
    };
    // scp-syntax `host:owner/repo` and an explicit `host:port/…` both collapse
    // to a path separator; both sides of the comparison get the same treatment.
    let normalized = after_creds.replacen(':', "/", 1);
    let trimmed = normalized.trim_end_matches('/');
    let no_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    no_git
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Render the working-tree entries for a [`ColdStartError::DirtyCheckout`].
///
/// Why: see [`DIRTY_ENTRY_PREVIEW`] — the remedy is the message's last line,
/// and an untruncated status can push it off the screen.
/// What: joins up to [`DIRTY_ENTRY_PREVIEW`] entries with newlines, appending
/// a `… and N more` line when there are more.
/// Test: `dirty_message_truncates_long_status`.
fn summarize_entries(entries: &[String]) -> String {
    let shown: Vec<&str> = entries
        .iter()
        .take(DIRTY_ENTRY_PREVIEW)
        .map(String::as_str)
        .collect();
    let mut out = shown.join("\n");
    if entries.len() > DIRTY_ENTRY_PREVIEW {
        out.push_str(&format!(
            "\n  … and {} more",
            entries.len() - DIRTY_ENTRY_PREVIEW
        ));
    }
    out
}

#[cfg(test)]
#[path = "inproject_cold_start_tests.rs"]
mod tests;
