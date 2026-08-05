//! Resolve the commit a new session-worktree branch is cut from (#4957).
//!
//! Why: `inproject::create_session_worktree` ran `git worktree add -b
//! session/<name> <path>` with no start-point, so every session branch was cut
//! from the base checkout's `HEAD` — the local default-branch ref. Nothing in
//! the in-project spawn path fetches, so on a machine whose local `main` has
//! not moved in weeks every new session starts weeks behind: the report that
//! opened #4957 measured 667 commits, with eight `session/*` branches and
//! local `main` all pointing at the same three-week-old commit. This repo's
//! `CLAUDE.md` names `origin/<default>` the source of truth for exactly this
//! reason, and the clone-based sibling path
//! (`provisioner::workspace::worktree_add`) already fetches before adding.
//! What: [`resolve`] fetches the repo's actual default branch from `origin`
//! and returns the ref `git worktree add` should be given, plus whether that
//! ref is freshly fetched, stale, or absent. Degradation is never silent: a
//! failed fetch still yields a usable start point but carries a
//! [`StartPoint::warning`] naming the stale tip, so the caller can log it
//! rather than report a clean success.
//! Test: `inproject::tests::session_worktree_branches_from_fetched_origin_not_stale_local_main`,
//! `inproject::tests::session_worktree_falls_back_to_remote_tracking_ref_when_fetch_fails`,
//! `inproject::tests::session_worktree_without_a_remote_still_branches_from_head`.

use std::path::Path;
use std::process::Command;

/// Where a new session branch should be cut from, and how trustworthy that is.
///
/// Why: the caller must distinguish three materially different situations that
/// all still produce a worktree — a fresh remote tip (the intended path), a
/// last-known remote tip after a failed fetch (degraded, must warn), and no
/// remote at all (a purely local repo, where `HEAD` is simply correct and a
/// warning would be noise). Collapsing them into `Option<String>` is what let
/// #4957 read as success.
/// What: a start-point ref for `git worktree add` (`None` means "omit the
/// argument and let git use `HEAD`"), plus an optional operator-facing
/// warning.
/// Test: see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartPoint {
    /// `origin/<default>`, refreshed from the remote during this call.
    Fresh { git_ref: String },
    /// `origin/<default>` as of some earlier fetch — this call's fetch failed.
    Stale { git_ref: String, reason: String },
    /// The repo has no `origin` remote: `HEAD` is the only correct start point
    /// and this is not a degradation.
    LocalOnly,
    /// A remote exists but neither the fetch nor any remote-tracking ref could
    /// supply a start point, so `HEAD` is used and may be stale.
    UnverifiedHead { reason: String },
}

impl StartPoint {
    /// The ref to pass to `git worktree add`, or `None` to let git use `HEAD`.
    pub fn git_ref(&self) -> Option<&str> {
        match self {
            StartPoint::Fresh { git_ref } | StartPoint::Stale { git_ref, .. } => Some(git_ref),
            StartPoint::LocalOnly | StartPoint::UnverifiedHead { .. } => None,
        }
    }

    /// Operator-facing text when the start point is NOT a freshly-fetched
    /// remote tip; `None` when nothing is wrong.
    pub fn warning(&self) -> Option<&str> {
        match self {
            StartPoint::Stale { reason, .. } | StartPoint::UnverifiedHead { reason } => {
                Some(reason)
            }
            StartPoint::Fresh { .. } | StartPoint::LocalOnly => None,
        }
    }
}

/// Fetch `origin`'s default branch into `base_path` and report the start point
/// a new session branch should be cut from (#4957).
///
/// Why: see the module docs — without this, `git worktree add -b` silently
/// inherits the base checkout's local `HEAD`.
/// What: resolves the default branch via
/// [`super::inproject_hygiene::get_default_branch`] (the crate's existing
/// resolver — the branch is never hardcoded to `main`), falling back to the
/// checked-out branch when `origin/HEAD` is unset but `origin/<current>`
/// exists; fetches the explicit refspec
/// `+refs/heads/<branch>:refs/remotes/origin/<branch>` so the remote-tracking
/// ref is updated deterministically rather than relying on `FETCH_HEAD`; and
/// maps the outcome onto [`StartPoint`]. `GIT_TERMINAL_PROMPT=0` keeps a
/// credential prompt from hanging the daemon on a private remote.
/// Test: see the module docs.
pub fn resolve(base_path: &Path) -> StartPoint {
    let Some(branch) = default_remote_branch(base_path) else {
        return if has_origin_remote(base_path) {
            StartPoint::UnverifiedHead {
                reason: "could not resolve origin's default branch (origin/HEAD unset and no \
                         remote-tracking ref for the checked-out branch); branching from the \
                         base checkout's local HEAD, which may be behind the remote"
                    .to_string(),
            }
        } else {
            StartPoint::LocalOnly
        };
    };

    let tracking_ref = format!("refs/remotes/origin/{branch}");
    let git_ref = format!("origin/{branch}");
    let refspec = format!("+refs/heads/{branch}:{tracking_ref}");

    match fetch(base_path, &refspec) {
        Ok(()) => StartPoint::Fresh { git_ref },
        Err(err) if ref_exists(base_path, &tracking_ref) => StartPoint::Stale {
            reason: format!(
                "git fetch origin {branch} failed ({err}); branching from the last fetched \
                 {git_ref} ({}) instead — this session may be behind the remote",
                tip_description(base_path, &git_ref)
            ),
            git_ref,
        },
        Err(err) => StartPoint::UnverifiedHead {
            reason: format!(
                "git fetch origin {branch} failed ({err}) and there is no {git_ref} \
                 remote-tracking ref to fall back on; branching from the base checkout's local \
                 HEAD, which may be behind the remote"
            ),
        },
    }
}

/// Resolve the branch name `origin` considers default, without hardcoding it.
fn default_remote_branch(base_path: &Path) -> Option<String> {
    if let Some(branch) = super::inproject_hygiene::get_default_branch(base_path) {
        return Some(branch);
    }
    // `origin/HEAD` is unset — normal for a repo built with `git init` plus
    // `git remote add` rather than `git clone`. The checked-out branch is the
    // best available answer, but only when `origin` actually tracks it.
    let current = git_stdout(base_path, &["symbolic-ref", "--short", "HEAD"])?;
    ref_exists(base_path, &format!("refs/remotes/origin/{current}")).then_some(current)
}

fn fetch(base_path: &Path, refspec: &str) -> Result<(), String> {
    let out = git(base_path)
        .args(["fetch", "--no-tags", "origin", refspec])
        .output()
        .map_err(|e| format!("failed to spawn git fetch: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "exit {}: {}",
        out.status,
        stderr.trim().replace('\n', "; ")
    ))
}

fn has_origin_remote(base_path: &Path) -> bool {
    git_stdout(base_path, &["remote", "get-url", "origin"]).is_some()
}

fn ref_exists(base_path: &Path, full_ref: &str) -> bool {
    git(base_path)
        .args(["rev-parse", "--verify", "--quiet", full_ref])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Short sha plus commit date of `git_ref`, so a staleness warning is legible
/// rather than abstract. Best-effort: an unreadable ref reports as `unknown`.
fn tip_description(base_path: &Path, git_ref: &str) -> String {
    git_stdout(base_path, &["log", "-1", "--format=%h %cs", git_ref])
        .unwrap_or_else(|| "unknown tip".to_string())
}

fn git_stdout(base_path: &Path, args: &[&str]) -> Option<String> {
    let out = git(base_path).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn git(base_path: &Path) -> Command {
    let mut cmd = Command::new("git");
    // A daemon has no terminal: without this a private remote's credential
    // prompt blocks the spawn indefinitely instead of failing fast.
    cmd.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(base_path);
    cmd
}
