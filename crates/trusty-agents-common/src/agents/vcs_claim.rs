//! Does the project's VCS CLAIM this file? (issue #4448)
//!
//! Why: #4526 was blocked because `ShadowsBundled` + `Untracked` cannot tell a
//! stale leftover from a project's deliberate override — the two are the same
//! classification by construction. `Origin::Project` was going to supply the
//! missing declaration and was closed as superseded (#4443), so the
//! declaration has to come from somewhere the deployer can actually reach.
//!
//! It already does: **a repository that commits a project-tier agent is
//! declaring it.** `git ls-files` is that declaration, readable without any
//! new schema, ledger, or config surface. A tracked file is the project's; a
//! sweep must never move it, no matter what its name or schema says.
//!
//! What: [`VcsIndex::probe`] runs at most two `git` invocations for a WHOLE
//! sweep (not one per file) and answers [`VcsIndex::claim`] from the result.
//! Three states, never a bool — "no repository here" and "git could not be
//! asked" are different facts with opposite safe answers, and collapsing them
//! either freezes the sweep in every non-git project or lets it run blind when
//! git is broken.
//!
//! This module NEVER writes. Both invocations are read-only queries, and
//! neither is passed any caller-controlled argument beyond the directory.
//!
//! Test: `vcs_claim_tests.rs`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// What the project's VCS says about one file.
///
/// Why: [`Unknown`](Self::Unknown) is not a synonym for
/// [`Unclaimed`](Self::Unclaimed). If git could not be consulted, the honest
/// answer is that a claim cannot be ruled out — and the caller must refuse to
/// move rather than guess. Folding the two together is the fail-open shape
/// this repo has been bitten by before.
/// What: [`Claimed`](Self::Claimed) — the file is tracked;
/// [`Unclaimed`](Self::Unclaimed) — a repository was found and does not track
/// it, or there is no repository at all (nothing can be claiming it);
/// [`Unknown`](Self::Unknown) — git is absent or failed, so no conclusion.
/// Test: `probe_finds_tracked_files`, `claim_of_an_untracked_file`,
/// `claim_outside_a_repo_is_unclaimed`,
/// `claim_is_unknown_when_git_is_unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsClaim {
    /// Tracked — the project owns this file. Never movable.
    Claimed,
    /// No VCS claim exists on this file.
    Unclaimed,
    /// The VCS could not be consulted. Treat as possibly claimed.
    Unknown,
}

/// The tracked-file set for one directory, resolved once per sweep.
///
/// Why: a per-file `git ls-files --error-unmatch` would spawn a process for
/// every candidate in a hot launch path. One listing answers the whole tier.
/// What: built by [`VcsIndex::probe`]; queried by [`VcsIndex::claim`]. There is
/// deliberately NO constructor that fabricates an empty tracked set — such a
/// constructor is a permissive-by-accident injection point for exactly the gate
/// this type exists to enforce, so tests exercise real repositories instead.
/// Test: `vcs_claim_tests.rs`.
#[derive(Debug, Clone)]
pub struct VcsIndex {
    state: IndexState,
}

#[derive(Debug, Clone)]
enum IndexState {
    /// A work tree was found; these are its tracked entries under the probed
    /// directory, as bare file names.
    Repo(BTreeSet<String>),
    /// The directory is not inside a git work tree.
    NoRepo,
    /// git is missing, or a query failed. No conclusion is available.
    Unavailable,
}

impl VcsIndex {
    /// Resolve what the project's VCS tracks inside `dir`.
    ///
    /// Why: the sweep needs one authoritative answer for the whole directory
    /// before it moves anything, and it must distinguish "not a repo" from
    /// "could not ask" — see [`VcsClaim`].
    /// What: runs `git -C <dir> rev-parse --is-inside-work-tree`; a spawn
    /// failure yields [`IndexState::Unavailable`] and a non-zero exit yields
    /// [`IndexState::NoRepo`]. On success it runs `git -C <dir> ls-files -z`,
    /// whose output is NUL-separated paths relative to `dir`; a spawn failure
    /// or non-zero exit there also yields `Unavailable`, since a repo was
    /// confirmed and the listing is the only thing that could clear a file.
    /// Entries containing `/` are dropped — the agent tier is flat, and a
    /// nested path can never name a file the sweep considers.
    /// Test: `probe_finds_tracked_files`, `claim_outside_a_repo_is_unclaimed`,
    /// `probe_drops_nested_entries`, `probe_of_a_missing_directory_does_not_panic`.
    pub fn probe(dir: &Path) -> Self {
        let inside = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output();
        match inside {
            Err(_) => {
                return Self {
                    state: IndexState::Unavailable,
                };
            }
            Ok(out) if !out.status.success() => {
                return Self {
                    state: IndexState::NoRepo,
                };
            }
            Ok(_) => {}
        }

        let listed = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["ls-files", "-z"])
            .output();
        let Ok(out) = listed else {
            return Self {
                state: IndexState::Unavailable,
            };
        };
        if !out.status.success() {
            return Self {
                state: IndexState::Unavailable,
            };
        }

        let tracked = String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|s| !s.is_empty() && !s.contains('/'))
            .map(str::to_owned)
            .collect();
        Self {
            state: IndexState::Repo(tracked),
        }
    }

    /// What the VCS says about `file_name` (a bare name, not a path).
    ///
    /// Why/What: a lookup in the set [`VcsIndex::probe`] resolved. See
    /// [`VcsClaim`] for why the no-repo and could-not-ask cases differ.
    /// Test: `probe_finds_tracked_files`, `claim_of_an_untracked_file`,
    /// `claim_of_an_absent_file`.
    pub fn claim(&self, file_name: &str) -> VcsClaim {
        match &self.state {
            IndexState::Repo(tracked) if tracked.contains(file_name) => VcsClaim::Claimed,
            IndexState::Repo(_) | IndexState::NoRepo => VcsClaim::Unclaimed,
            IndexState::Unavailable => VcsClaim::Unknown,
        }
    }
}

#[cfg(test)]
#[path = "vcs_claim_tests.rs"]
mod tests;
