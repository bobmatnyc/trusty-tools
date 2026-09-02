//! Per-repository full-history walk bookkeeping (issue #6073).
//!
//! Why: the fully-unbounded collect path — no `since_date`, no `--weeks` —
//! has no week-level `collection_runs` bookkeeping, so it re-walked the entire
//! history on every invocation. trusty-audit retries a failed repository by
//! re-running all nine sweep stages, which meant any render-stage failure paid
//! a full re-collect; the issue measured that against a 594 MB extract
//! database. Recording the tip each completed walk reached lets the next run
//! skip the walk outright, or hide the recorded tip and walk only what is new.
//! What: [`WalkTips`] (what the repository looks like now), [`WalkState`]
//! (what the last completed walk recorded), [`WalkPlan`] and [`plan`] (the
//! pure decision between the two), and the `repo_walk_state` load/record
//! helpers.
//! Test: the `tests` module below, plus
//! `crate::collect::git::extractor::tests::{unchanged_head_skips_the_walk,
//! advanced_head_walks_only_the_new_commits,
//! unreachable_base_forces_a_full_rewalk,
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
    /// No `repo_walk_state` row — a fresh database, or one predating v25.
    NeverWalked,
    /// A row exists but its walk never finished.
    PreviousWalkIncomplete,
    /// The recorded sha is no longer reachable from the current head — a
    /// force-push or history rewrite.
    BaseUnreachable,
}

impl FullWalkReason {
    /// The reason as it appears in the log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeverWalked => "no completed walk recorded for this repository",
            Self::PreviousWalkIncomplete => "the previously recorded walk did not complete",
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
/// What: no recorded state, or an incomplete one, forces a full walk;
/// identical head AND ref digest skips; anything else goes incremental when
/// the recorded sha is still reachable, and full when it is not.
/// Test: `tests::{plan_skips_an_unchanged_head,
/// plan_goes_incremental_when_the_head_advanced,
/// plan_names_every_full_walk_reason}`.
pub fn plan(recorded: Option<&WalkState>, current: &WalkTips, base_reachable: bool) -> WalkPlan {
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
        "SELECT head_sha, head_ref, tips_digest, walk_complete \
         FROM repo_walk_state WHERE repository = ?1",
    )?;
    let mut rows = stmt.query(params![repository])?;
    match rows.next()? {
        Some(row) => Ok(Some(WalkState {
            head_sha: row.get(0)?,
            head_ref: row.get(1)?,
            tips_digest: row.get(2)?,
            walk_complete: row.get::<_, i64>(3)? != 0,
        })),
        None => Ok(None),
    }
}

/// Record `tips` as this repository's walk state.
///
/// Why: the caller writes `walk_complete = false` before the walk and `true`
/// after it succeeds, so a run interrupted mid-walk leaves a row that forces a
/// full re-walk rather than a skip built on partial data.
/// What: an upsert on the `repository` primary key, refreshing every column
/// including `walked_at`.
/// Test: `crate::collect::git::extractor::tests::unchanged_head_skips_the_walk`.
///
/// # Errors
///
/// Propagates [`CollectError::Db`] when the write fails.
pub fn record(
    conn: &Connection,
    repository: &str,
    tips: &WalkTips,
    walk_complete: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO repo_walk_state \
         (repository, head_sha, head_ref, tips_digest, walk_complete, walked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(repository) DO UPDATE SET \
           head_sha = excluded.head_sha, \
           head_ref = excluded.head_ref, \
           tips_digest = excluded.tips_digest, \
           walk_complete = excluded.walk_complete, \
           walked_at = excluded.walked_at",
        params![
            repository,
            tips.head_sha,
            tips.head_ref,
            tips.tips_digest,
            walk_complete as i64,
            chrono::Utc::now().to_rfc3339(),
        ],
    )
    .map_err(CollectError::Db)?;
    debug!(
        repo = repository,
        head = %tips.head_sha,
        complete = walk_complete,
        "recorded repo walk state"
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

    fn state(head: &str, digest: &str, complete: bool) -> WalkState {
        WalkState {
            head_sha: head.to_string(),
            head_ref: "refs/heads/main".to_string(),
            tips_digest: digest.to_string(),
            walk_complete: complete,
        }
    }

    #[test]
    fn plan_skips_an_unchanged_head() {
        let s = state("aaa", "d1", true);
        assert_eq!(plan(Some(&s), &tips("aaa", "d1"), true), WalkPlan::Skip);
    }

    #[test]
    fn plan_goes_incremental_when_the_head_advanced() {
        let s = state("aaa", "d1", true);
        assert_eq!(
            plan(Some(&s), &tips("bbb", "d2"), true),
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
        assert_ne!(plan(Some(&s), &tips("aaa", "d2"), true), WalkPlan::Skip);
    }

    #[test]
    fn an_incomplete_previous_walk_forces_a_full_rewalk() {
        let s = state("aaa", "d1", false);
        assert_eq!(
            plan(Some(&s), &tips("aaa", "d1"), true),
            WalkPlan::Full {
                reason: FullWalkReason::PreviousWalkIncomplete
            }
        );
    }

    #[test]
    fn plan_names_every_full_walk_reason() {
        assert_eq!(
            plan(None, &tips("aaa", "d1"), true),
            WalkPlan::Full {
                reason: FullWalkReason::NeverWalked
            }
        );
        let s = state("aaa", "d1", true);
        assert_eq!(
            plan(Some(&s), &tips("bbb", "d2"), false),
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
