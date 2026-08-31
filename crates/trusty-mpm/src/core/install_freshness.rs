//! Whether a `cargo install --path` source is behind `origin/main` (#4462).
//!
//! Why: `tm` is ONE global binary on PATH, shared by every managed session on
//! the machine. Reinstalling it from a worktree that predates a fix silently
//! regresses that fix for all of them at once, and nothing said so: cargo
//! records the source directory, not whether its commits are current, and the
//! binary's own version number does not move when the source is merely stale.
//! `tm reinstall --binary` takes exactly that route — it re-runs
//! `cargo install --path <dir>` against whatever directory the ledger recorded
//! — so it is where the question has to be asked. Publishing already has this
//! guard (`check-publish-ready.sh`, "publish only from merged main"); local
//! installs had none.
//!
//! **It refuses only on positive evidence.** A directory that is not a git
//! repository, has no `origin/main`, or has no usable `git` reports
//! [`SourceFreshness::Unknown`] and the install proceeds with a warning. The
//! answer is also only as current as the repository's `origin/main` ref: the
//! probe fetches first on a best-effort basis, and a failed fetch is neither
//! fatal nor reported — the comparison then runs against the cached ref.
//!
//! **Installing from a feature branch stays possible.** That is an ordinary
//! dev-loop action; the guard exists to make it deliberate, not impossible.
//! [`ALLOW_STALE_INSTALL_ENV`] lifts it, mirroring the `ALLOW_UNMERGED_PUBLISH`
//! convention on the publish side.
//!
//! What: [`probe_source_freshness`] answers for a directory;
//! [`stale_install_refusal`] is the pure decision over that answer plus the
//! override, so both branches are testable without an install.
//! Test: the `tests` module below.

use std::path::Path;
use std::process::Command;

/// Set this to `1` to install from a source behind `origin/main` anyway.
///
/// Why: installing from a feature branch is a legitimate dev-loop action. The
/// guard's job is to make it a decision somebody took, not to block it.
pub const ALLOW_STALE_INSTALL_ENV: &str = "TRUSTY_MPM_ALLOW_STALE_INSTALL";

/// Where an install source's `HEAD` sits relative to its `origin/main`.
///
/// Why: "behind" and "cannot tell" have opposite consequences — one is the
/// silent regression this guard exists to stop, the other is a directory the
/// guard simply knows nothing about and must not block.
/// What: `Unknown` carries the reason, so the caller can print it rather than
/// warning in the abstract.
/// Test: `head_at_origin_main_is_current`, `head_behind_origin_main_is_behind`,
/// `a_directory_that_is_not_a_repo_is_unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFreshness {
    /// `HEAD` is `origin/main` or a descendant of it.
    Current,
    /// `origin/main` is not an ancestor of `HEAD` — the source predates
    /// whatever landed on main since.
    Behind,
    /// Could not be determined. Carries why.
    Unknown(String),
}

/// Ask git where `dir`'s `HEAD` sits relative to `origin/main`.
///
/// Why: this is the one impure half, kept apart from the decision so the
/// decision is testable without a repository.
/// What: best-effort `git fetch origin main` (a stale `origin/main` would make
/// the comparison answer about the wrong commit; a failed fetch — offline, no
/// remote — is not fatal and is not reported here, since the comparison
/// against the ref on disk is still worth making), then
/// `git merge-base --is-ancestor origin/main HEAD`: exit 0 is
/// [`SourceFreshness::Current`], exit 1 is [`SourceFreshness::Behind`], and
/// anything else — no repository, no `origin/main`, no git — is
/// [`SourceFreshness::Unknown`].
/// Test: `head_at_origin_main_is_current`, `head_behind_origin_main_is_behind`,
/// `a_directory_that_is_not_a_repo_is_unknown`.
pub fn probe_source_freshness(dir: &Path) -> SourceFreshness {
    let _ = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["fetch", "--quiet", "origin", "main"])
        .output();

    let output = match Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", "origin/main", "HEAD"])
        .output()
    {
        Ok(o) => o,
        Err(e) => return SourceFreshness::Unknown(format!("could not run git: {e}")),
    };
    match output.status.code() {
        Some(0) => SourceFreshness::Current,
        Some(1) => SourceFreshness::Behind,
        _ => SourceFreshness::Unknown(
            String::from_utf8_lossy(&output.stderr)
                .trim()
                .to_string()
                .to_owned(),
        ),
    }
}

/// The refusal, if this source may not be installed from.
///
/// Why: pure over the probe's answer and the override, so the refuse and
/// permit branches are covered without running an install.
/// What: `Some(reason)` only when `freshness` is [`SourceFreshness::Behind`]
/// and the override is not set. `Current` and `Unknown` both return `None` —
/// the guard denies on positive evidence and nothing else.
/// Test: `a_behind_source_is_refused`, `the_override_permits_a_behind_source`,
/// `an_unknown_source_is_permitted`.
pub fn stale_install_refusal(
    dir: &Path,
    freshness: &SourceFreshness,
    override_set: bool,
) -> Option<String> {
    if *freshness != SourceFreshness::Behind || override_set {
        return None;
    }
    Some(format!(
        "Refusing to install from {}: its HEAD is behind `origin/main`, so this build predates \
         whatever has landed on main since (#4462). `tm` is one global binary shared by every \
         managed session on this machine, so installing a stale build regresses those fixes for \
         all of them at once, silently — the binary's own version number does not move. Update \
         the checkout (`git fetch origin && git merge --ff-only origin/main`) and install again, \
         or set {ALLOW_STALE_INSTALL_ENV}=1 to install this source deliberately.",
        dir.display()
    ))
}

/// Whether the operator set [`ALLOW_STALE_INSTALL_ENV`].
///
/// What: any value but `0`/empty counts as set.
pub fn stale_install_override() -> bool {
    std::env::var(ALLOW_STALE_INSTALL_ENV)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "install_freshness_tests.rs"]
mod install_freshness_tests;
