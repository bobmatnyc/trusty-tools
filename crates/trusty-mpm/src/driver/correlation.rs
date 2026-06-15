//! Session ↔ artifact correlation for the driver autonomy policy.
//!
//! Why: the driver (the calling agentic process that operates trusty-mpm) must
//! understand the *scope boundary* of a managed session before it can safely
//! auto-accept work. A session is meaningful only in relation to the artifacts
//! it is supposed to produce — a worktree, a branch, a pull request, an issue.
//! Without an explicit correlation the driver cannot tell whether a proposed
//! change stays in-scope or has drifted into unrelated territory, and drift is
//! exactly the failure mode that makes unattended autonomy dangerous.
//! What: defines [`SessionCorrelation`] — the optional `{worktree, branch,
//! pr_id, issue_id}` link between a managed session and its artifacts — plus
//! [`ScopeCheck`], the pure in-scope validation result, and the
//! [`SessionCorrelation::validate_in_scope`] guardrail that classifies a set of
//! touched paths / referenced ids against the correlation.
//! Test: see the `tests` module below — every constructor, predicate, and the
//! scope validator are exercised without any I/O or network call.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Correlation linking a managed session to the artifacts it owns.
///
/// Why: the autonomy policy needs a stable, serializable scope descriptor so the
/// driver can answer "is this change in-scope for this session?" deterministically.
/// Every field is optional because correlation accrues over a session's life: a
/// session may start with only a worktree + branch and gain a `pr_id` / `issue_id`
/// once the work is pushed and a PR is opened.
/// What: holds the worktree path the session is confined to, the git branch it
/// commits onto, and the optional PR / issue identifiers it is meant to close.
/// All fields are `Option` so a partially-correlated session is representable.
/// Test: `default_is_fully_unset`, `serde_round_trip`, `with_builders_set_fields`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCorrelation {
    /// Isolated worktree the session is confined to. Paths outside it are
    /// out-of-scope by construction.
    pub worktree: Option<PathBuf>,
    /// Git branch the session commits onto (e.g. `feat/1204-autonomy-tiers`).
    pub branch: Option<String>,
    /// Pull-request identifier the session's work belongs to (e.g. `1204`).
    pub pr_id: Option<u64>,
    /// Issue identifier the session is meant to resolve (e.g. `1204`).
    pub issue_id: Option<u64>,
}

impl SessionCorrelation {
    /// Construct an empty correlation (nothing linked yet).
    ///
    /// Why: a freshly-provisioned session has no artifacts; callers build the
    /// correlation up incrementally with the `with_*` setters.
    /// What: returns the `Default` value — all four fields `None`.
    /// Test: `default_is_fully_unset`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the worktree this session is confined to.
    ///
    /// Why: the worktree is the primary scope boundary — every touched path is
    /// validated against it.
    /// What: stores `path` and returns `self` for chaining.
    /// Test: `with_builders_set_fields`.
    #[must_use]
    pub fn with_worktree(mut self, path: impl Into<PathBuf>) -> Self {
        self.worktree = Some(path.into());
        self
    }

    /// Set the git branch this session commits onto.
    ///
    /// Why: branch correlation lets the driver reject work pushed to the wrong ref.
    /// What: stores `branch` and returns `self` for chaining.
    /// Test: `with_builders_set_fields`.
    #[must_use]
    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }

    /// Set the pull-request id this session's work belongs to.
    ///
    /// Why: PR correlation lets the driver tie a `pending_decision` to the diff
    /// trusty-review evaluated.
    /// What: stores `pr_id` and returns `self` for chaining.
    /// Test: `with_builders_set_fields`.
    #[must_use]
    pub fn with_pr_id(mut self, pr_id: u64) -> Self {
        self.pr_id = Some(pr_id);
        self
    }

    /// Set the issue id this session is meant to resolve.
    ///
    /// Why: issue correlation is the canonical scope anchor — the driver checks
    /// that referenced issue ids match before auto-accepting cross-cutting work.
    /// What: stores `issue_id` and returns `self` for chaining.
    /// Test: `with_builders_set_fields`.
    #[must_use]
    pub fn with_issue_id(mut self, issue_id: u64) -> Self {
        self.issue_id = Some(issue_id);
        self
    }

    /// Whether this session has any artifact correlation at all.
    ///
    /// Why: an uncorrelated session cannot be scope-validated, so the policy
    /// treats "no correlation" as a reason to fall back to a more cautious tier.
    /// What: returns `true` when every field is `None`.
    /// Test: `is_uncorrelated_tracks_fields`.
    pub fn is_uncorrelated(&self) -> bool {
        self.worktree.is_none()
            && self.branch.is_none()
            && self.pr_id.is_none()
            && self.issue_id.is_none()
    }

    /// Whether a single path lies inside the correlated worktree.
    ///
    /// Why: the core scope predicate — a change is only auto-acceptable if every
    /// path it touches is confined to the session's worktree.
    /// What: returns `true` when `worktree` is set and `path` is equal to or a
    /// descendant of it. With no worktree set, scope is unknowable so returns
    /// `false` (cautious default — the caller decides how to treat "unknown").
    /// Test: `path_in_worktree_matches_descendants`, `path_out_of_worktree`.
    pub fn path_in_scope(&self, path: impl AsRef<Path>) -> bool {
        match &self.worktree {
            Some(root) => path.as_ref().starts_with(root),
            None => false,
        }
    }

    /// Validate that a proposed change stays inside this session's scope.
    ///
    /// Why: this is the guardrail the autonomy policy consults before allowing an
    /// auto-accept. It is a pure function over the correlation and the change's
    /// touched paths plus referenced issue ids — no filesystem or network access —
    /// so it is fully unit-testable and cannot be subverted by a misbehaving LLM.
    /// What: returns a [`ScopeCheck`] describing whether all `touched_paths` are
    /// inside the worktree and whether `referenced_issue_ids` are consistent with
    /// the correlated `issue_id`. An uncorrelated session yields
    /// [`ScopeCheck::Uncorrelated`]; an in-scope change yields
    /// [`ScopeCheck::InScope`]; otherwise [`ScopeCheck::OutOfScope`] with the
    /// offending paths / ids.
    /// Test: `validate_in_scope_*` family below.
    pub fn validate_in_scope(
        &self,
        touched_paths: &[PathBuf],
        referenced_issue_ids: &[u64],
    ) -> ScopeCheck {
        if self.is_uncorrelated() {
            return ScopeCheck::Uncorrelated;
        }

        // Path scoping only applies when a worktree boundary is known. If no
        // worktree is set we cannot judge paths, so we skip the path check
        // rather than failing every path.
        let stray_paths: Vec<PathBuf> = if self.worktree.is_some() {
            touched_paths
                .iter()
                .filter(|p| !self.path_in_scope(p))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // Issue scoping only applies when an issue id is correlated. Any
        // referenced id that differs from the correlated one is foreign.
        let foreign_issue_ids: Vec<u64> = match self.issue_id {
            Some(expected) => referenced_issue_ids
                .iter()
                .copied()
                .filter(|id| *id != expected)
                .collect(),
            None => Vec::new(),
        };

        if stray_paths.is_empty() && foreign_issue_ids.is_empty() {
            ScopeCheck::InScope
        } else {
            ScopeCheck::OutOfScope {
                stray_paths,
                foreign_issue_ids,
            }
        }
    }
}

/// Result of validating a proposed change against a session's correlation.
///
/// Why: the autonomy policy needs a three-way answer — in-scope, out-of-scope,
/// or "cannot tell because the session is uncorrelated" — rather than a bare
/// boolean, because the third case must downgrade the tier rather than reject.
/// What: an enum with [`ScopeCheck::InScope`], [`ScopeCheck::OutOfScope`]
/// (carrying the offending paths and foreign issue ids), and
/// [`ScopeCheck::Uncorrelated`].
/// Test: produced and matched throughout the `validate_in_scope_*` tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeCheck {
    /// Every touched path is inside the worktree and all referenced issue ids
    /// match the correlated issue.
    InScope,
    /// At least one path escaped the worktree or referenced a foreign issue id.
    OutOfScope {
        /// Touched paths that fell outside the correlated worktree.
        stray_paths: Vec<PathBuf>,
        /// Referenced issue ids that did not match the correlated issue id.
        foreign_issue_ids: Vec<u64>,
    },
    /// The session has no artifact correlation, so scope cannot be judged.
    Uncorrelated,
}

impl ScopeCheck {
    /// Whether this check permits an auto-accept on scope grounds.
    ///
    /// Why: callers want a quick boolean gate; only [`ScopeCheck::InScope`]
    /// clears the scope guardrail. `Uncorrelated` is deliberately *not* a pass —
    /// the policy must decide separately how to treat an uncorrelated session.
    /// What: returns `true` only for the `InScope` variant.
    /// Test: `scope_check_is_in_scope`.
    pub fn is_in_scope(&self) -> bool {
        matches!(self, ScopeCheck::InScope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_fully_unset() {
        let c = SessionCorrelation::new();
        assert!(c.worktree.is_none());
        assert!(c.branch.is_none());
        assert!(c.pr_id.is_none());
        assert!(c.issue_id.is_none());
        assert!(c.is_uncorrelated());
    }

    #[test]
    fn with_builders_set_fields() {
        let c = SessionCorrelation::new()
            .with_worktree("/tmp/wt")
            .with_branch("feat/x")
            .with_pr_id(42)
            .with_issue_id(1204);
        assert_eq!(c.worktree, Some(PathBuf::from("/tmp/wt")));
        assert_eq!(c.branch.as_deref(), Some("feat/x"));
        assert_eq!(c.pr_id, Some(42));
        assert_eq!(c.issue_id, Some(1204));
    }

    #[test]
    fn is_uncorrelated_tracks_fields() {
        assert!(SessionCorrelation::new().is_uncorrelated());
        assert!(!SessionCorrelation::new().with_branch("b").is_uncorrelated());
        assert!(!SessionCorrelation::new().with_issue_id(1).is_uncorrelated());
    }

    #[test]
    fn serde_round_trip() {
        let c = SessionCorrelation::new()
            .with_worktree("/tmp/wt")
            .with_branch("feat/x")
            .with_pr_id(7)
            .with_issue_id(1204);
        let json = serde_json::to_string(&c).expect("serialize");
        let back: SessionCorrelation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }

    #[test]
    fn path_in_worktree_matches_descendants() {
        let c = SessionCorrelation::new().with_worktree("/repo/wt");
        assert!(c.path_in_scope("/repo/wt"));
        assert!(c.path_in_scope("/repo/wt/src/lib.rs"));
    }

    #[test]
    fn path_out_of_worktree() {
        let c = SessionCorrelation::new().with_worktree("/repo/wt");
        assert!(!c.path_in_scope("/repo/other/file.rs"));
        assert!(!c.path_in_scope("/etc/passwd"));
    }

    #[test]
    fn path_in_scope_without_worktree_is_false() {
        let c = SessionCorrelation::new().with_branch("b");
        assert!(!c.path_in_scope("/anything"));
    }

    #[test]
    fn validate_in_scope_uncorrelated() {
        let c = SessionCorrelation::new();
        let check = c.validate_in_scope(&[PathBuf::from("/x")], &[1]);
        assert_eq!(check, ScopeCheck::Uncorrelated);
        assert!(!check.is_in_scope());
    }

    #[test]
    fn validate_in_scope_all_paths_inside() {
        let c = SessionCorrelation::new()
            .with_worktree("/repo/wt")
            .with_issue_id(1204);
        let check = c.validate_in_scope(
            &[
                PathBuf::from("/repo/wt/a.rs"),
                PathBuf::from("/repo/wt/sub/b.rs"),
            ],
            &[1204],
        );
        assert_eq!(check, ScopeCheck::InScope);
        assert!(check.is_in_scope());
    }

    #[test]
    fn validate_in_scope_stray_path() {
        let c = SessionCorrelation::new().with_worktree("/repo/wt");
        let stray = PathBuf::from("/repo/other.rs");
        let check = c.validate_in_scope(&[PathBuf::from("/repo/wt/a.rs"), stray.clone()], &[]);
        match check {
            ScopeCheck::OutOfScope {
                stray_paths,
                foreign_issue_ids,
            } => {
                assert_eq!(stray_paths, vec![stray]);
                assert!(foreign_issue_ids.is_empty());
            }
            other => panic!("expected OutOfScope, got {other:?}"),
        }
    }

    #[test]
    fn validate_in_scope_foreign_issue() {
        let c = SessionCorrelation::new()
            .with_worktree("/repo/wt")
            .with_issue_id(1204);
        let check = c.validate_in_scope(&[PathBuf::from("/repo/wt/a.rs")], &[1204, 9999]);
        match check {
            ScopeCheck::OutOfScope {
                stray_paths,
                foreign_issue_ids,
            } => {
                assert!(stray_paths.is_empty());
                assert_eq!(foreign_issue_ids, vec![9999]);
            }
            other => panic!("expected OutOfScope, got {other:?}"),
        }
    }

    #[test]
    fn validate_in_scope_ignores_issue_when_uncorrelated_issue() {
        // worktree set, no issue id correlated → issue refs are not checked.
        let c = SessionCorrelation::new().with_worktree("/repo/wt");
        let check = c.validate_in_scope(&[PathBuf::from("/repo/wt/a.rs")], &[1, 2, 3]);
        assert_eq!(check, ScopeCheck::InScope);
    }
}
