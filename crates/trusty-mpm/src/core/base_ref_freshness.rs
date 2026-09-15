//! Is the local `origin/<base>` the ref a three-dot diff should be taken
//! against? (#7748)
//!
//! Why: every pre-push judgement in this workspace is a three-dot diff —
//! `git diff origin/main...HEAD` for the mandatory credential scan, the same
//! range for the changelog-fragment gate and the component labels. All three
//! read the LOCAL `refs/remotes/origin/main`, which a checkout that has not
//! fetched can carry hundreds of commits behind GitHub's tip. Measured twice on
//! 2026-09-13: a stale base would have put roughly 1,270 unrelated paths into a
//! credential scan, which is how a real secret hides in a scan people learn to
//! wave through.
//!
//! What: [`check`] compares the local ref with `git ls-remote origin
//! refs/heads/<base>`, fetches once when they disagree, and re-reads. The
//! verdict is [`BaseFreshness`], and [`BaseFreshness::refusal`] is the whole
//! contract for a caller: `Some(message)` means the diff base cannot be
//! trusted, and the message names the stale sha and the remote sha.
//!
//! FAIL-CLOSED. A comparison that cannot be made — no remote, no `git`, a
//! transport error — is [`BaseFreshness::Undetermined`], which refuses. A scan
//! whose base could not be verified has not passed; it has not run.
//!
//! Test: `base_ref_freshness_tests.rs`.

use std::path::{Path, PathBuf};

/// Where the local `origin/<base>` stands against the remote (#7748).
///
/// Why: a caller has to tell four states apart — already current, brought
/// current by this call's own fetch, provably behind with the fetch unable to
/// fix it, and unanswerable. Only the first two may be diffed against.
/// What: the verdict, carrying the shas that justify it.
/// Test: `fresh_when_the_local_ref_already_matches_the_remote`,
/// `a_stale_ref_is_refreshed_by_the_fetch`,
/// `a_ref_that_stays_behind_refuses_and_names_both_shas`,
/// `an_unanswerable_comparison_refuses_rather_than_passing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseFreshness {
    /// The local ref already equals the remote tip.
    Fresh {
        /// The sha both sides agree on.
        sha: String,
    },
    /// The local ref was behind and this call's fetch brought it current.
    Refreshed {
        /// What the local ref held before the fetch (`absent` when it had none).
        was: String,
        /// The remote tip it now holds.
        now: String,
    },
    /// The local ref is still not the remote tip after the fetch.
    Stale {
        /// The sha a diff would be taken against.
        local: String,
        /// The sha it should have been taken against.
        remote: String,
    },
    /// The comparison could not be made at all.
    Undetermined {
        /// Why, in one line.
        reason: String,
    },
}

/// The sha string used when the local ref does not exist at all.
const ABSENT: &str = "absent";

impl BaseFreshness {
    /// Why this base must not be diffed against, if it must not.
    ///
    /// Why: this is the fail-closed half. `Undetermined` refuses alongside
    /// `Stale`, because a security scan whose base could not be verified has no
    /// verdict to report — reading it as "passed" is the failure mode #7748
    /// exists to prevent.
    /// What: `None` for [`Self::Fresh`] and [`Self::Refreshed`]; otherwise a
    /// message naming both shas, or the reason the comparison failed.
    /// Test: `a_ref_that_stays_behind_refuses_and_names_both_shas`,
    /// `an_unanswerable_comparison_refuses_rather_than_passing`.
    pub fn refusal(&self) -> Option<String> {
        match self {
            Self::Fresh { .. } | Self::Refreshed { .. } => None,
            Self::Stale { local, remote } => Some(format!(
                "the local diff base is stale: it holds {local}, the remote tip is {remote}, and \
                 the fetch did not close the gap — a three-dot diff against it would report \
                 unrelated commits as this branch's own"
            )),
            Self::Undetermined { reason } => Some(format!(
                "the diff base could not be verified against the remote ({reason}) — an \
                 unverified base is not a clean one"
            )),
        }
    }
}

/// The three git reads [`check`] needs, behind one seam.
///
/// Why: the whole decision is a comparison of two strings plus one fetch, and a
/// test of the refusal arms must not need a remote, a network, or a clone.
/// What: the local remote-tracking ref, the remote tip, and the fetch that
/// closes a gap between them.
/// Test: the `FakeRefs` probe in `base_ref_freshness_tests.rs`.
pub trait BaseRefs {
    /// The sha of `refs/remotes/origin/<base>`; `Ok(None)` when it is absent.
    fn local(&self, base: &str) -> Result<Option<String>, String>;
    /// The sha `git ls-remote origin refs/heads/<base>` reports.
    fn remote(&self, base: &str) -> Result<String, String>;
    /// Fetch `base` from `origin`.
    fn fetch(&self, base: &str) -> Result<(), String>;
}

/// Compare, fetch if needed, and report where the local base ref stands.
///
/// Why: the order matters. Asking the REMOTE first means a stale local ref can
/// never be mistaken for the answer, and fetching only on disagreement keeps
/// the common case to one cheap `ls-remote`.
/// What: `Fresh` on agreement; otherwise one fetch and a re-read, yielding
/// `Refreshed` or `Stale`. Any failed read is `Undetermined`; a failed FETCH is
/// `Stale`, because the disagreement is already established fact.
/// Test: `fresh_when_the_local_ref_already_matches_the_remote`,
/// `a_stale_ref_is_refreshed_by_the_fetch`,
/// `a_ref_that_stays_behind_refuses_and_names_both_shas`,
/// `an_unanswerable_comparison_refuses_rather_than_passing`,
/// `a_failed_fetch_reports_stale_rather_than_undetermined`.
pub fn check<R: BaseRefs>(refs: &R, base: &str) -> BaseFreshness {
    let remote = match refs.remote(base) {
        Ok(sha) if !sha.trim().is_empty() => sha.trim().to_string(),
        Ok(_) => {
            return BaseFreshness::Undetermined {
                reason: format!("`git ls-remote origin refs/heads/{base}` named no commit"),
            };
        }
        Err(e) => return BaseFreshness::Undetermined { reason: e },
    };
    let before = match refs.local(base) {
        Ok(sha) => sha.map(|s| s.trim().to_string()).unwrap_or_default(),
        Err(e) => return BaseFreshness::Undetermined { reason: e },
    };
    if before == remote {
        return BaseFreshness::Fresh { sha: remote };
    }

    if let Err(e) = refs.fetch(base) {
        return BaseFreshness::Stale {
            local: label(&before),
            remote: format!("{remote} (the fetch failed: {e})"),
        };
    }
    match refs.local(base) {
        Ok(after) => {
            let after = after.map(|s| s.trim().to_string()).unwrap_or_default();
            if after == remote {
                BaseFreshness::Refreshed {
                    was: label(&before),
                    now: remote,
                }
            } else {
                BaseFreshness::Stale {
                    local: label(&after),
                    remote,
                }
            }
        }
        Err(e) => BaseFreshness::Undetermined { reason: e },
    }
}

/// An empty sha reads as `absent`, never as a blank in a message.
fn label(sha: &str) -> String {
    if sha.is_empty() {
        ABSENT.to_string()
    } else {
        sha.to_string()
    }
}

/// Production [`BaseRefs`] over this workspace's single git entry point.
///
/// Why: `trusty_common::git::command_in` is the one place git is spawned from
/// (#7171); a bare `Command::new("git")` here would be a second implementation
/// of that decision.
/// What: `rev-parse refs/remotes/origin/<base>`, `ls-remote origin
/// refs/heads/<base>`, and `fetch origin <base>`, all scoped to `root` and run
/// with no terminal prompt so a credential-less remote fails fast instead of
/// hanging a gate.
/// Test: exercised live; the decisions it feeds are covered against `FakeRefs`.
pub struct RealBaseRefs {
    /// The checkout every command runs in.
    root: PathBuf,
}

impl RealBaseRefs {
    /// A probe rooted at `root`.
    pub fn at(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Run one git command, returning trimmed stdout or a one-line error.
    fn git(&self, args: &[&str]) -> Result<String, String> {
        let out = trusty_common::git::command_in(&self.root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("`git {}` could not be run: {e}", args.join(" ")))?;
        if !out.status.success() {
            return Err(format!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl BaseRefs for RealBaseRefs {
    fn local(&self, base: &str) -> Result<Option<String>, String> {
        let r = format!("refs/remotes/origin/{base}");
        // An absent ref is not an error: a fresh clone of a branch that has
        // never been fetched is the case the fetch below exists to fix.
        match self.git(&["rev-parse", "--verify", "--quiet", &r]) {
            Ok(sha) if sha.is_empty() => Ok(None),
            Ok(sha) => Ok(Some(sha)),
            Err(_) => Ok(None),
        }
    }

    fn remote(&self, base: &str) -> Result<String, String> {
        let r = format!("refs/heads/{base}");
        let line = self.git(&["ls-remote", "origin", &r])?;
        Ok(line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string())
    }

    fn fetch(&self, base: &str) -> Result<(), String> {
        self.git(&["fetch", "origin", base]).map(|_| ())
    }
}

#[cfg(test)]
#[path = "base_ref_freshness_tests.rs"]
mod tests;
