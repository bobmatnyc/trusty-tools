//! Per-repository full-history walk bookkeeping (issue #6073).
//!
//! Why: the fully-unbounded collect path — no `since_date`, no `--weeks` —
//! has no week-level `collection_runs` bookkeeping, so it re-walked the entire
//! history on every invocation. trusty-audit retries a failed repository by
//! re-running all nine sweep stages, which meant any render-stage failure paid
//! a full re-collect; the issue measured that against a 594 MB extract
//! database. Recording the tip each completed walk reached lets the next run
//! skip the walk outright, or hide the recorded tip and walk only what is new.
//! What: [`WalkTips`] (what the repository looks like now), [`WalkScope`]
//! (what the walk was allowed to see), [`WalkState`] (what the last completed
//! walk recorded), [`WalkPlan`] and [`plan`] (the pure decision between them),
//! and the `repo_walk_state` load/record helpers.
//! Test: the `tests` module below, plus
//! `crate::collect::git::extractor::tests::{unchanged_head_skips_the_walk,
//! advanced_head_walks_only_the_new_commits,
//! unreachable_base_forces_a_full_rewalk,
//! a_scoped_walk_does_not_license_skipping_a_full_one,
//! a_pre_v25_database_reads_as_never_walked}`.

use git2::{Oid, Repository};
use rusqlite::{params, Connection};
use tracing::debug;

use crate::collect::errors::{CollectError, Result};

/// What a repository's refs look like right now.
///
/// Why: skipping a walk on an unchanged HEAD alone would be unsound — the
/// default revwalk seeds from every `refs/heads/*` and `refs/remotes/origin/*`
/// ref, so a side branch that moved while HEAD stood still carries commits a
/// HEAD-only comparison would silently drop. `tips_digest` covers every ref,
/// making the skip conservative: an unrelated ref moving costs a walk, but no
/// commit is ever missed.
/// What: the resolved HEAD sha and ref name (both empty on an unborn HEAD),
/// plus a digest over every sorted `(refname, oid)` pair.
/// Test: `tests::a_moved_side_branch_changes_the_digest`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WalkTips {
    /// Resolved `HEAD` commit sha, or empty when `HEAD` is unborn.
    pub head_sha: String,
    /// Full `HEAD` ref name (e.g. `refs/heads/main`), or empty when unborn.
    pub head_ref: String,
    /// Digest over every branch and remote-tracking `(refname, oid)` pair.
    pub tips_digest: String,
}

/// What a walk was allowed to see — the per-run narrowing flags.
///
/// Why: `--branch`, `--head-only`, a per-repo `branch:` override and
/// `skip_merges` all shrink what a walk records, and none of them is visible
/// in [`WalkTips`], which digests every ref whatever the walk actually
/// covered. Without the scope beside the tips, a `--branch main` run records a
/// tip over refs it never walked and the next full-scope run skips, leaving
/// every side-branch commit permanently absent. Recording the scope makes a
/// narrower or wider run a different walk, so it re-walks instead.
/// What: the seeding flags in a stable, comparable form. [`Self::as_key`] is
/// the string stored in `repo_walk_state.walk_scope`; two walks are the same
/// scope exactly when their keys are equal.
/// Test: `tests::a_narrower_scope_is_a_different_walk`,
/// `crate::collect::git::extractor::tests::a_scoped_walk_does_not_license_skipping_a_full_one`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WalkScope {
    /// Branch names the revwalk seeds from — `--branch` plus any per-repo
    /// `branch:` override. Empty means "every branch and remote-tracking ref".
    pub branches: Vec<String>,
    /// Whether the revwalk seeds from `HEAD` alone.
    pub head_only: bool,
    /// Whether merge commits are dropped rather than written.
    pub skip_merges: bool,
}

impl WalkScope {
    /// The comparable form stored in `repo_walk_state.walk_scope`.
    ///
    /// What: branch names sorted and deduplicated so two runs listing the same
    /// branches in different orders compare equal, then the two booleans. The
    /// branch list is JSON-encoded rather than comma-joined (#6073 review):
    /// git permits a comma inside a ref name, so `,` as a separator gave
    /// branch `a,b` and branches `a` + `b` one key, and a run scoped to either
    /// would then skip the other. The value stays readable in the database on
    /// purpose — an operator asking why a repository re-walked can read the
    /// answer out of the row.
    /// Test: `tests::a_comma_in_a_branch_name_is_a_distinct_scope`.
    pub fn as_key(&self) -> String {
        let mut branches = self.branches.clone();
        branches.sort();
        branches.dedup();
        // Serializing a `Vec<String>` cannot fail — no non-string map key, no
        // non-finite float, and the strings are already valid UTF-8.
        let branches =
            serde_json::to_string(&branches).expect("a Vec<String> always serialises to JSON");
        format!(
            "branches={branches};head_only={};skip_merges={}",
            u8::from(self.head_only),
            u8::from(self.skip_merges),
        )
    }
}

/// The tip a previous COMPLETED walk of this repository reached.
///
/// Why: `walk_complete` distinguishes "this walk finished" from "a walk
/// started and was interrupted" — the Ctrl-C-and-rerun flow the issue names.
/// An interrupted walk must not license a skip.
/// Test: `tests::an_incomplete_previous_walk_forces_a_full_rewalk`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WalkState {
    /// The `HEAD` sha the recorded walk reached.
    pub head_sha: String,
    /// The `HEAD` ref name the recorded walk followed.
    pub head_ref: String,
    /// The ref digest at the time of the recorded walk.
    pub tips_digest: String,
    /// The [`WalkScope::as_key`] of the recorded walk.
    pub walk_scope: String,
    /// Whether that walk ran to completion.
    pub walk_complete: bool,
}

/// Why a run must walk the full history rather than skip or go incremental.
///
/// Why: the requirement is a log line NAMING the reason, so the reason is a
/// value rather than a string built at the call site.
/// Test: `tests::plan_names_every_full_walk_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FullWalkReason {
    /// The operator passed `--force`, the standing re-collect lever.
    Forced,
    /// No `repo_walk_state` row — a fresh database, or one predating v25.
    NeverWalked,
    /// A row exists but its walk never finished.
    PreviousWalkIncomplete,
    /// This run's [`WalkScope`] differs from the recorded one, so the recorded
    /// tip describes a walk that saw a different set of refs or commits.
    ScopeChanged,
    /// The recorded sha is no longer reachable from the current head — a
    /// force-push or history rewrite.
    BaseUnreachable,
}

impl FullWalkReason {
    /// The reason as it appears in the log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forced => "--force was passed, so the full history is re-walked",
            Self::NeverWalked => "no completed walk recorded for this repository",
            Self::PreviousWalkIncomplete => "the previously recorded walk did not complete",
            Self::ScopeChanged => {
                "this run's branch/head-only/merge scope differs from the recorded walk"
            }
            Self::BaseUnreachable => {
                "the previously walked commit is no longer reachable (force-push or rewrite)"
            }
        }
    }
}

/// What a collect invocation should do with this repository's history.
///
/// Test: `tests::plan_skips_an_unchanged_head`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WalkPlan {
    /// The database is already current for these tips; do not walk.
    Skip,
    /// Walk only what is reachable from the current tips and not from
    /// `base_sha`.
    Incremental {
        /// The previously walked commit to hide from the revwalk.
        base_sha: String,
    },
    /// Walk the full history, for the named reason.
    Full {
        /// Why the incremental path was unavailable.
        reason: FullWalkReason,
    },
}

/// Decide between skipping, walking incrementally, and walking in full.
///
/// Why: keeping the decision pure means every branch is unit-testable without
/// a repository or a database, and the caller stays a thin dispatcher.
/// What: no recorded state, or an incomplete one, forces a full walk; a scope
/// that differs from the recorded one forces a full walk, because the recorded
/// tip then describes refs this run does not walk (or refs a narrower run
/// never walked); identical head AND ref digest skips; anything else goes
/// incremental when the recorded sha is still reachable, and full when it is
/// not.
/// Test: `tests::{plan_skips_an_unchanged_head,
/// plan_goes_incremental_when_the_head_advanced,
/// plan_names_every_full_walk_reason, a_narrower_scope_is_a_different_walk}`.
pub fn plan(
    recorded: Option<&WalkState>,
    current: &WalkTips,
    scope: &WalkScope,
    base_reachable: bool,
) -> WalkPlan {
    let Some(state) = recorded else {
        return WalkPlan::Full {
            reason: FullWalkReason::NeverWalked,
        };
    };
    if !state.walk_complete {
        return WalkPlan::Full {
            reason: FullWalkReason::PreviousWalkIncomplete,
        };
    }
    // #6073 review: the recorded tip is only comparable against a walk that
    // covered the same refs and commit kinds. A different scope is a different
    // walk, whichever direction it narrows.
    if state.walk_scope != scope.as_key() {
        return WalkPlan::Full {
            reason: FullWalkReason::ScopeChanged,
        };
    }
    if state.head_sha == current.head_sha && state.tips_digest == current.tips_digest {
        return WalkPlan::Skip;
    }
    if base_reachable && !state.head_sha.is_empty() {
        return WalkPlan::Incremental {
            base_sha: state.head_sha.clone(),
        };
    }
    WalkPlan::Full {
        reason: FullWalkReason::BaseUnreachable,
    }
}

/// Read this repository's current tips.
///
/// What: resolves `HEAD` (empty strings when unborn) and digests every
/// `refs/heads/*` and `refs/remotes/*` `(name, oid)` pair in sorted order.
/// Test: `tests::a_moved_side_branch_changes_the_digest`.
///
/// # Errors
///
/// Propagates [`CollectError::Git`] when the ref iterator fails.
pub fn current_walk_tips(repo: &Repository) -> Result<WalkTips> {
    let (head_sha, head_ref) = match repo.head() {
        Ok(head) => {
            let sha = head.target().map(|oid| oid.to_string()).unwrap_or_default();
            (sha, head.name().unwrap_or_default().to_string())
        }
        // An unborn HEAD (a freshly initialised repository) is not an error
        // here — it simply has nothing to walk yet.
        Err(_) => (String::new(), String::new()),
    };

    let mut pairs: Vec<String> = Vec::new();
    for r in repo.references()?.flatten() {
        let Some(name) = r.name() else { continue };
        if !name.starts_with("refs/heads/") && !name.starts_with("refs/remotes/") {
            continue;
        }
        let Some(oid) = r.target() else { continue };
        pairs.push(format!("{name}\t{oid}"));
    }
    pairs.sort();
    let tips_digest = blake3::hash(pairs.join("\n").as_bytes())
        .to_hex()
        .to_string();

    Ok(WalkTips {
        head_sha,
        head_ref,
        tips_digest,
    })
}

/// True when `base_sha` is still reachable from the repository's current head.
///
/// Why: a force-push or history rewrite leaves the recorded tip either absent
/// from the object database or off the current graph. Hiding such a commit
/// would silently drop everything the rewrite replaced, so the caller must
/// fall back to a full walk instead.
/// What: `true` when the sha parses, resolves to a commit, and is either the
/// head itself or an ancestor of it. Any failure reads as unreachable.
/// Test: `crate::collect::git::extractor::tests::unreachable_base_forces_a_full_rewalk`.
pub fn base_is_reachable(repo: &Repository, base_sha: &str, head_sha: &str) -> bool {
    if base_sha.is_empty() || head_sha.is_empty() {
        return false;
    }
    if base_sha == head_sha {
        return true;
    }
    let (Ok(base), Ok(head)) = (Oid::from_str(base_sha), Oid::from_str(head_sha)) else {
        return false;
    };
    if repo.find_commit(base).is_err() {
        return false;
    }
    repo.graph_descendant_of(head, base).unwrap_or(false)
}

/// Read the recorded walk state for `repository`, if any.
///
/// Why: a database predating migration v25 gains an empty `repo_walk_state`
/// table, so "no row" is the documented never-walked signal for both a fresh
/// database and an upgraded one.
/// Test: `crate::collect::git::extractor::tests::a_pre_v25_database_reads_as_never_walked`.
///
/// # Errors
///
/// Propagates [`CollectError::Db`] when the query fails.
pub fn load(conn: &Connection, repository: &str) -> Result<Option<WalkState>> {
    let mut stmt = conn.prepare(
        "SELECT head_sha, head_ref, tips_digest, walk_scope, walk_complete \
         FROM repo_walk_state WHERE repository = ?1",
    )?;
    let mut rows = stmt.query(params![repository])?;
    match rows.next()? {
        Some(row) => Ok(Some(WalkState {
            head_sha: row.get(0)?,
            head_ref: row.get(1)?,
            tips_digest: row.get(2)?,
            walk_scope: row.get(3)?,
            walk_complete: row.get::<_, i64>(4)? != 0,
        })),
        None => Ok(None),
    }
}

/// Mark this repository's walk as in flight without disturbing its last base.
///
/// Why: the caller runs this before the walk so a run interrupted mid-walk
/// leaves `walk_complete = 0` and the next run re-walks in full rather than
/// skipping on partial data. Overwriting `head_sha` and `tips_digest` at the
/// same time — which is what a `record(…, false)` does — would replace the
/// last KNOWN-GOOD base with tips nothing has walked yet, so an interrupted
/// run would lose the base it could otherwise have hidden.
/// What: inserts an empty incomplete row when none exists, and otherwise only
/// clears `walk_complete` and refreshes `walked_at`.
/// Test: `crate::collect::git::extractor::tests::an_interrupted_walk_keeps_its_last_good_base`.
///
/// # Errors
///
/// Propagates [`CollectError::Db`] when the write fails.
pub fn mark_in_flight(conn: &Connection, repository: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO repo_walk_state \
         (repository, head_sha, head_ref, tips_digest, walk_scope, walk_complete, walked_at) \
         VALUES (?1, '', '', '', '', 0, ?2) \
         ON CONFLICT(repository) DO UPDATE SET \
           walk_complete = 0, \
           walked_at = excluded.walked_at",
        params![repository, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(CollectError::Db)?;
    debug!(repo = repository, "marked repo walk in flight");
    Ok(())
}

/// Record `tips` and `scope` as this repository's COMPLETED walk state.
///
/// Why: only a walk that ran to completion may license a later skip, so this
/// is the one write that sets `walk_complete = 1`; [`mark_in_flight`] is its
/// counterpart before the walk.
/// What: an upsert on the `repository` primary key, refreshing every column
/// including `walked_at`.
/// Test: `crate::collect::git::extractor::tests::unchanged_head_skips_the_walk`.
///
/// # Errors
///
/// Propagates [`CollectError::Db`] when the write fails.
pub fn record_complete(
    conn: &Connection,
    repository: &str,
    tips: &WalkTips,
    scope: &WalkScope,
) -> Result<()> {
    conn.execute(
        "INSERT INTO repo_walk_state \
         (repository, head_sha, head_ref, tips_digest, walk_scope, walk_complete, walked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6) \
         ON CONFLICT(repository) DO UPDATE SET \
           head_sha = excluded.head_sha, \
           head_ref = excluded.head_ref, \
           tips_digest = excluded.tips_digest, \
           walk_scope = excluded.walk_scope, \
           walk_complete = 1, \
           walked_at = excluded.walked_at",
        params![
            repository,
            tips.head_sha,
            tips.head_ref,
            tips.tips_digest,
            scope.as_key(),
            chrono::Utc::now().to_rfc3339(),
        ],
    )
    .map_err(CollectError::Db)?;
    debug!(
        repo = repository,
        head = %tips.head_sha,
        scope = %scope.as_key(),
        "recorded completed repo walk state"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tips(head: &str, digest: &str) -> WalkTips {
        WalkTips {
            head_sha: head.to_string(),
            head_ref: "refs/heads/main".to_string(),
            tips_digest: digest.to_string(),
        }
    }

    /// The scope every legacy case in this module walked under: no branch
    /// filter, no head-only, merges kept.
    fn full_scope() -> WalkScope {
        WalkScope::default()
    }

    fn state(head: &str, digest: &str, complete: bool) -> WalkState {
        WalkState {
            head_sha: head.to_string(),
            head_ref: "refs/heads/main".to_string(),
            tips_digest: digest.to_string(),
            walk_scope: full_scope().as_key(),
            walk_complete: complete,
        }
    }

    #[test]
    fn plan_skips_an_unchanged_head() {
        let s = state("aaa", "d1", true);
        assert_eq!(
            plan(Some(&s), &tips("aaa", "d1"), &full_scope(), true),
            WalkPlan::Skip
        );
    }

    #[test]
    fn plan_goes_incremental_when_the_head_advanced() {
        let s = state("aaa", "d1", true);
        assert_eq!(
            plan(Some(&s), &tips("bbb", "d2"), &full_scope(), true),
            WalkPlan::Incremental {
                base_sha: "aaa".to_string()
            }
        );
    }

    #[test]
    fn a_moved_side_branch_forces_a_walk_despite_an_unchanged_head() {
        // The digest covers every ref, so a side branch moving under a
        // stationary HEAD must not produce a skip — those commits are new.
        let s = state("aaa", "d1", true);
        assert_ne!(
            plan(Some(&s), &tips("aaa", "d2"), &full_scope(), true),
            WalkPlan::Skip
        );
    }

    #[test]
    fn an_incomplete_previous_walk_forces_a_full_rewalk() {
        let s = state("aaa", "d1", false);
        assert_eq!(
            plan(Some(&s), &tips("aaa", "d1"), &full_scope(), true),
            WalkPlan::Full {
                reason: FullWalkReason::PreviousWalkIncomplete
            }
        );
    }

    /// (#6073 review) git permits a comma inside a ref name, so a
    /// comma-joined branch list gave `a,b` and `a` + `b` one key — and a run
    /// scoped to either would then skip the other's walk entirely.
    #[test]
    fn a_comma_in_a_branch_name_is_a_distinct_scope() {
        // Sorted, these two produce the identical `a,b` under a comma join.
        let one_odd_branch = WalkScope {
            branches: vec!["a,b".to_string()],
            ..WalkScope::default()
        };
        let two_branches = WalkScope {
            branches: vec!["a".to_string(), "b".to_string()],
            ..WalkScope::default()
        };
        assert_ne!(
            one_odd_branch.as_key(),
            two_branches.as_key(),
            "a comma inside a ref name must not read as a separator"
        );

        // The recorded key of one scope must not license skipping the other.
        let mut recorded = state("aaa", "d1", true);
        recorded.walk_scope = two_branches.as_key();
        assert_eq!(
            plan(Some(&recorded), &tips("aaa", "d1"), &one_odd_branch, true),
            WalkPlan::Full {
                reason: FullWalkReason::ScopeChanged
            }
        );

        // Order and duplicates still collapse to one key.
        let reordered = WalkScope {
            branches: vec!["b".to_string(), "a".to_string(), "b".to_string()],
            ..WalkScope::default()
        };
        assert_eq!(two_branches.as_key(), reordered.as_key());
    }

    /// (#6073 review) A narrower walk records a tip over refs it never walked,
    /// so a later full-scope run must re-walk rather than trust it — and the
    /// mirror case (a narrow run after a full one) must not trust it either.
    #[test]
    fn a_narrower_scope_is_a_different_walk() {
        let narrow = WalkScope {
            branches: vec!["main".to_string()],
            ..WalkScope::default()
        };
        let mut recorded = state("aaa", "d1", true);
        recorded.walk_scope = narrow.as_key();

        assert_eq!(
            plan(Some(&recorded), &tips("aaa", "d1"), &full_scope(), true),
            WalkPlan::Full {
                reason: FullWalkReason::ScopeChanged
            },
            "a full-scope run must not skip on a scoped run's recorded tip"
        );
        assert_eq!(
            plan(Some(&recorded), &tips("aaa", "d1"), &narrow, true),
            WalkPlan::Skip,
            "the same scope still skips"
        );

        let head_only = WalkScope {
            head_only: true,
            ..WalkScope::default()
        };
        assert_eq!(
            plan(Some(&recorded), &tips("aaa", "d1"), &head_only, true),
            WalkPlan::Full {
                reason: FullWalkReason::ScopeChanged
            }
        );
        // Branch order is not a scope difference.
        let reordered = WalkScope {
            branches: vec!["b".to_string(), "a".to_string()],
            ..WalkScope::default()
        };
        let ordered = WalkScope {
            branches: vec!["a".to_string(), "b".to_string()],
            ..WalkScope::default()
        };
        assert_eq!(reordered.as_key(), ordered.as_key());
        // skip_merges narrows what is WRITTEN, not what is seeded, and is a
        // scope difference all the same.
        let no_merges = WalkScope {
            skip_merges: true,
            ..WalkScope::default()
        };
        assert_ne!(no_merges.as_key(), full_scope().as_key());
    }

    #[test]
    fn plan_names_every_full_walk_reason() {
        assert_eq!(
            plan(None, &tips("aaa", "d1"), &full_scope(), true),
            WalkPlan::Full {
                reason: FullWalkReason::NeverWalked
            }
        );
        let s = state("aaa", "d1", true);
        assert_eq!(
            plan(Some(&s), &tips("bbb", "d2"), &full_scope(), false),
            WalkPlan::Full {
                reason: FullWalkReason::BaseUnreachable
            }
        );
        assert!(FullWalkReason::BaseUnreachable
            .as_str()
            .contains("force-push"));
        assert!(FullWalkReason::NeverWalked
            .as_str()
            .contains("no completed"));
        assert!(FullWalkReason::Forced.as_str().contains("--force"));
        assert!(FullWalkReason::ScopeChanged.as_str().contains("scope"));
    }

    #[test]
    fn a_moved_side_branch_changes_the_digest() {
        // Two ref sets differing only in a side branch's oid must digest
        // differently, which is what makes the skip sound.
        let a = blake3::hash(b"refs/heads/main\taaa\nrefs/heads/side\tbbb");
        let b = blake3::hash(b"refs/heads/main\taaa\nrefs/heads/side\tccc");
        assert_ne!(a, b);
    }
}
