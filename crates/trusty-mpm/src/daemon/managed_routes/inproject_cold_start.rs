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
//! [`super::inproject::ensure_base_clone`] against it, returning the path,
//! whether an existing checkout was reused, and whether its fast-forward was
//! skipped. Reusing an existing checkout has two distinct outcomes, and the
//! difference between them is deliberate:
//!
//! 1. **Remote mismatch → FAIL LOUD.** An existing checkout whose `origin`
//!    names a different repository is an error. trusty-mpm never re-points a
//!    remote: doing so silently would make `tm run <owner>/<repo>` operate on
//!    some other repository under that repository's name, and every future
//!    pull would keep coming from the wrong place.
//! 2. **Dirty tree → WARN AND PROCEED.** A dirty tree cannot be
//!    fast-forwarded, so the fast-forward is skipped and the operator is told
//!    so in normal output. It is NOT an error, because since #4957 the session
//!    branch is cut from a freshly-fetched `origin/<default>`
//!    ([`super::inproject_start_point::resolve`]) and never inherits the base
//!    checkout's local `HEAD` — so a skipped fast-forward cannot leak stale
//!    content into the session. The tm checkout is shared with the operator and
//!    their editors, which makes uncommitted content its expected steady state
//!    (ADR-0030 §4/§5); refusing there would block the common case to guard
//!    against nothing.
//!
//! What must never happen is SILENCE. The failure shape this module exists to
//! avoid is `core::standalone::load::pull_ff_only`, which warns at a level
//! nobody reads and returns `Ok(())`, leaving a failed refresh
//! indistinguishable from a successful one at the call site. Here the skip is
//! both `warn!`-logged and returned to the caller in
//! [`ManagedCheckout::refresh_skipped`] so the CLI can print it.
//!
//! Either way the refresh is still ATTEMPTED through
//! [`super::inproject_hygiene::run_hygiene_for_base`] — the crate's single
//! non-destructive base-clone refresh — which fetches and then gates only the
//! fast-forward, matching ADR-0030 §4. One exception: a checkout carrying the
//! `inproject_hygiene::HYGIENE_OPT_OUT_MARKER` file
//! (`.trusty-mpm-no-hygiene`) skips the sweep entirely, fetch included. That is
//! the marker's whole purpose, and the session is still unaffected — its start
//! point does its own fetch (#4957).
//!
//! The reported skip is one-sided: `Some` means no fast-forward, `None` does
//! not mean there was one. See [`ManagedCheckout::refresh_skipped`].
//!
//! Test: `inproject_cold_start_tests.rs` (pure canonicalization, the
//! remote-mismatch refusal, and the dirty warn-and-proceed path against real
//! temp git repos); `tests/inproject_cold_start.rs` (fresh clone → the shape
//! `try_inproject_spawn` requires).

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::{inproject, inproject_hygiene};

/// How many working-tree entries a skipped-refresh notice lists before it
/// summarises the rest.
///
/// Why: a managed checkout can be thousands of lines dirty; a notice that
/// scrolls the terminal hides its own first line, which is the point.
/// What: `10`.
/// Test: `dirty_message_truncates_long_status`.
const DIRTY_ENTRY_PREVIEW: usize = 10;

/// Why a cold start refused. Every variant is a loud stop, never a warning.
///
/// Why: a checkout whose IDENTITY cannot be confirmed is the one hazard no
/// warning can cover — proceeding would operate on the wrong repository under
/// the requested one's name. A dirty tree is deliberately NOT here: it is
/// reported through [`ManagedCheckout::refresh_skipped`] instead, because the
/// session branch never inherits the base checkout's `HEAD` (module docs).
/// What: `RemoteMismatch` when `origin` names another repository; `NoOrigin`
/// when there is no `origin` to compare at all; `Provision` wraps the
/// underlying clone/hygiene failure verbatim.
/// Test: `existing_checkout_on_a_different_remote_fails_loud`,
/// `existing_checkout_without_an_origin_fails_loud`.
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

    /// Git could not report the existing checkout's `remote.origin.url` at all.
    ///
    /// Why: distinct from [`ColdStartError::NoOrigin`] since #4734 — telling an
    /// operator whose `.git/config` is unreadable that the checkout "has no
    /// remote.origin.url" sends them to remove a directory that is fine.
    #[error(
        "managed checkout {} exists but its remote could not be read, so it cannot be \
         matched against {requested}: {reason}",
        .path.display()
    )]
    OriginUnreadable {
        /// The managed checkout that was inspected.
        path: PathBuf,
        /// The clone URL the caller asked for.
        requested: String,
        /// What git reported.
        reason: String,
    },

    /// Cloning or refreshing the base clone failed.
    #[error("{0}")]
    Provision(String),
}

/// The managed base clone a cold start produced.
///
/// Why: the caller needs the path to launch into, whether it cloned or reused
/// (so the operator can tell a first run from a repeat one), and — critically —
/// whether the fast-forward was skipped. That last field exists so the skip
/// cannot be silent: a `warn!` alone goes to a log the operator is not reading,
/// which is the `pull_ff_only` failure mode. Returning it makes the CLI able to
/// print it in normal output.
/// What: the absolute `<repos_root>/<owner>/<repo>` path, `reused`, and
/// `refresh_skipped`, in a form suitable for printing directly after the path.
///
/// 🔴 `refresh_skipped` is ONE-SIDED, and reading it as a fast-forward receipt
/// is wrong. `Some(reason)` is a reliable "the fast-forward did not happen".
/// `None` means only that the dirty gate did not predict a skip — see
/// [`fast_forward_skip_reason`] for the five other conditions under which
/// `inproject_hygiene::decide_update` declines, and the gitignored-collision
/// refusal after it, none of which this field reports. A clean checkout sitting
/// on a non-default branch produces `None` and no fast-forward.
/// Test: `dirty_existing_checkout_warns_and_proceeds`,
/// `clean_matching_checkout_is_reused_and_refreshed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedCheckout {
    /// Absolute path to the managed base clone.
    pub base_path: PathBuf,
    /// `true` when an existing checkout was verified and reused rather than cloned.
    pub reused: bool,
    /// A predicted reason the fast-forward will be skipped, when one is known.
    ///
    /// `Some` implies no fast-forward; `None` does NOT imply one happened. See
    /// the type-level doc. The caller MUST surface a `Some` in normal output.
    pub refresh_skipped: Option<String>,
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
/// What: when `base_path/.git` is absent, clones. When it is present, the
/// remote check runs FIRST and can refuse; then the fast-forward gate is
/// evaluated ([`fast_forward_skip_reason`]) and reported rather than enforced;
/// then the refresh is attempted through
/// [`super::inproject_hygiene::run_hygiene_for_base`], which fetches and gates
/// the fast-forward on a SUPERSET of the conditions checked here — so it can
/// decline when nothing was reported. It skips wholesale, fetch included, when
/// the checkout carries `inproject_hygiene::HYGIENE_OPT_OUT_MARKER`. Either way
/// it finishes through [`super::inproject::ensure_base_clone`], so a reused
/// checkout gets the same `.worktrees/` exclusion invariant a fresh clone does.
///
/// ORDERING IS THE POINT. The identity check runs BEFORE anything writes to the
/// directory, so a checkout belonging to another repository is never fetched
/// into, merged into, or otherwise touched.
/// Test: `existing_checkout_on_a_different_remote_fails_loud`,
/// `dirty_existing_checkout_warns_and_proceeds`,
/// `clean_matching_checkout_is_reused_and_refreshed`.
pub fn ensure_managed_checkout_at(
    base_path: &Path,
    clone_url: &str,
) -> Result<ManagedCheckout, ColdStartError> {
    let reused = base_path.join(".git").exists();
    let mut refresh_skipped = None;

    if reused {
        // The one refusal: an existing checkout must be the repo that was asked
        // for. Checked before any write.
        verify_remote_matches(base_path, clone_url)?;

        refresh_skipped = fast_forward_skip_reason(base_path);
        if let Some(reason) = &refresh_skipped {
            // Logged here for the daemon/log path AND returned to the caller,
            // which prints it. A warn! alone would be the `pull_ff_only`
            // failure mode: technically not silent, practically unread.
            warn!(
                path = %base_path.display(),
                "cold-start: NOT fast-forwarding the managed checkout — {reason}"
            );
        } else {
            info!(
                path = %base_path.display(),
                "cold-start: reusing managed checkout; refreshing"
            );
        }

        // Attempted either way. The fetch keeps `origin/<default>` current, and
        // that ref — not the local HEAD — is what the session branch is cut from
        // (#4957), which is why a declined fast-forward is reportable rather
        // than fatal. Only the fast-forward is gated (ADR-0030 §4) — except
        // under the `HYGIENE_OPT_OUT_MARKER`, which skips the sweep whole.
        inproject_hygiene::run_hygiene_for_base(base_path).map_err(ColdStartError::Provision)?;
    }

    inproject::ensure_base_clone(clone_url, base_path).map_err(ColdStartError::Provision)?;

    Ok(ManagedCheckout {
        base_path: base_path.to_path_buf(),
        reused,
        refresh_skipped,
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
/// checkout with no origin at all is `NoOrigin`, not a match; a checkout git
/// cannot read the remote of is `OriginUnreadable` (#4734), because "move or
/// remove that directory" is the wrong instruction for a config git refused to
/// parse.
/// Test: `existing_checkout_on_a_different_remote_fails_loud`,
/// `existing_checkout_with_an_unreadable_remote_fails_loud`,
/// `equivalent_remote_spellings_match`.
fn verify_remote_matches(base_path: &Path, requested: &str) -> Result<(), ColdStartError> {
    let found = inproject::get_origin_url(base_path).map_err(|reason| {
        ColdStartError::OriginUnreadable {
            path: base_path.to_path_buf(),
            requested: requested.to_string(),
            reason,
        }
    })?;
    let Some(found) = found else {
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

/// Report why the fast-forward will be skipped, if it will be.
///
/// Why: this predicts the hygiene sweep's own dirty gate so the skip can be
/// stated to the operator in normal output. It does NOT enforce anything —
/// enforcement would block the common case for no benefit, because the session
/// branch is cut from freshly-fetched `origin/<default>` and never inherits the
/// base checkout's local `HEAD` (#4957). What it buys is that the skip is not
/// silent, which is the half of the `pull_ff_only` shape that IS a defect: that
/// function warns at a level nobody reads and returns `Ok(())`.
///
/// The check is `git status --porcelain` WITHOUT `--ignored`, deliberately: the
/// question is whether git can fast-forward this tree, and gitignored build
/// output does not affect that. The gitignored-collision hazard has its own,
/// narrower guard inside the hygiene sweep
/// (`inproject_hygiene::colliding_untracked_paths`, #4961), which runs against
/// the specific paths an update would write.
/// What: `None` when the tree is clean. `Some(reason)` when it is dirty (naming
/// up to [`DIRTY_ENTRY_PREVIEW`] entries) or when git could not report at all —
/// an unreadable checkout is never assumed clean, and the hygiene sweep's own
/// gate declines the fast-forward in that case too, so predicting a skip
/// matches what actually happens.
///
/// 🔴 PARTIAL PREDICTOR, deliberately. It mirrors two of the conditions
/// `inproject_hygiene::decide_update` checks, not all of them. That function
/// also declines on a detached HEAD, a checked-out branch that is not the
/// default branch, an unknown ahead-count, and unpushed commits; and
/// `update_to_origin` refuses separately when the update would clobber a
/// gitignored path (#4961). None of those produce a `Some` here, so a clean
/// checkout on a feature branch returns `None` and is still not fast-forwarded.
/// Widening this to a full receipt means either duplicating those gates or
/// having the sweep report its own decision, which is a change to
/// `inproject_hygiene`'s contract — out of scope while the consequence is only
/// a missing notice. The session is unaffected either way (#4957).
/// Test: `dirty_existing_checkout_warns_and_proceeds`,
/// `clean_matching_checkout_is_reused_and_refreshed`.
fn fast_forward_skip_reason(base_path: &Path) -> Option<String> {
    match inproject_hygiene::porcelain_status(base_path) {
        Ok(entries) if entries.is_empty() => None,
        Ok(entries) => Some(format!(
            "it has uncommitted changes:\n{}",
            summarize_entries(&entries)
        )),
        Err(detail) => Some(format!("its git state could not be read: {detail}")),
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

/// Render the working-tree entries naming why a fast-forward was skipped.
///
/// Why: see [`DIRTY_ENTRY_PREVIEW`] — an untruncated status can push the notice
/// that matters off the screen.
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
