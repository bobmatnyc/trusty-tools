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

/// The ONLY git stderr that means "there is genuinely no repository here".
///
/// Why: this literal is the whole safety margin of gate 3 — see
/// [`VcsIndex::probe`]. The parenthesised clause is load-bearing: git emits
/// `fatal: not a git repository: (null)` for a STALE WORKTREE POINTER, so the
/// shorter phrase `not a git repository` matches a broken repo and a
/// genuinely-absent one alike. Only the "searched the parents and found
/// nothing" form is real absence.
/// What: verified byte-identical against git's output for an empty temp
/// directory. Any wording drift falls through to [`IndexState::Unavailable`],
/// which refuses — the fail-closed direction.
/// Test: `claim_outside_a_repo_is_unclaimed` (the match),
/// `a_stale_worktree_pointer_is_unknown_not_no_repo` (the near-miss).
const NO_REPO_STDERR: &str = "not a git repository (or any of the parent directories)";

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
    ///
    /// What: runs `git -C <dir> rev-parse --is-inside-work-tree`, then
    /// `git -C <dir> ls-files -z`, whose output is NUL-separated paths relative
    /// to `dir`. Entries containing `/` are dropped — the agent tier is flat, so
    /// a nested path can never name a file the sweep considers. Only ONE
    /// outcome yields [`IndexState::NoRepo`]; every other non-success yields
    /// [`IndexState::Unavailable`], which refuses.
    ///
    /// 🔴 THE EXIT CODE ALONE IS NOT A CLASSIFIER. Git has no dedicated exit
    /// code for "this is not a repository" — it exits 128 for that AND for
    /// every other fatal condition. Reading any non-zero exit as `NoRepo`
    /// (which this did until #4448 review) sends a live work tree git merely
    /// declined to read into the SWEEPABLE state, and its committed agent files
    /// get moved. The triggers are ordinary, not exotic: `detected dubious
    /// ownership` on a checkout owned by another uid, a `.git` file whose
    /// worktree gitdir is gone, a broken `repositoryformatversion`, or any
    /// failing `git` shim on `PATH`.
    ///
    /// So the branch is on the REASON, matched against
    /// [`NO_REPO_STDERR`] — and note that a substring match on the shorter
    /// `not a git repository` would NOT be safe: a stale worktree pointer emits
    /// `fatal: not a git repository: (null)`, which contains that phrase while
    /// meaning the opposite. Only the parenthesised "searched the parents and
    /// found nothing" form is genuine absence. Any future git wording change
    /// therefore falls through to `Unavailable` — the refusing side.
    ///
    /// Success is likewise not enough: a BARE repository exits 0 printing
    /// `false`, so `stdout` must read exactly `true` before the listing is
    /// trusted.
    ///
    /// Test: `probe_finds_tracked_files`, `claim_outside_a_repo_is_unclaimed`,
    /// `probe_drops_nested_entries`, `probe_of_a_missing_directory_does_not_panic`,
    /// `an_unreadable_repo_is_unknown_not_no_repo`,
    /// `a_stale_worktree_pointer_is_unknown_not_no_repo`,
    /// `a_bare_repo_is_unknown`.
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
            // #4448: branch on WHY it failed, never on the bare exit code.
            Ok(out) if !out.status.success() => {
                let why = String::from_utf8_lossy(&out.stderr);
                return Self {
                    state: if why.contains(NO_REPO_STDERR) {
                        IndexState::NoRepo
                    } else {
                        IndexState::Unavailable
                    },
                };
            }
            // #4448: a bare repo exits 0 printing `false`; it has no work tree,
            // so nothing here can answer whether a file is claimed.
            Ok(out) if String::from_utf8_lossy(&out.stdout).trim() != "true" => {
                return Self {
                    state: IndexState::Unavailable,
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
