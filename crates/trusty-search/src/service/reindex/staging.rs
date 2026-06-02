//! Non-destructive reindex staging decisions (#603).
//!
//! Why: before this change, only a `--force` reindex staged its rebuilt corpus
//! in `index.redb.tmp` and atomically swapped it in on success. The standard
//! (non-force) reindex wrote chunks straight into the live `index.redb`, so a
//! reindex that failed mid-way — or, post-#601, embedded zero vectors — destroyed
//! the only searchable copy with no way to roll back. During the Duetto P0 this
//! turned a transient embedder outage into a permanently dead index.
//!
//! What: the actual redb staging/swap plumbing (open-fresh tmp, swap onto the
//! indexer, rename-on-commit, discard-on-abort) lives in `super` because it
//! needs private indexer internals. This module owns the *pure decision* that
//! drives that plumbing: given whether a durable corpus exists and the terminal
//! [`super::validate::ReindexOutcome`], decide whether to (a) stage at all and
//! (b) commit the swap or roll it back. Keeping the decision pure makes the
//! safety-critical "never promote a failed/empty corpus" rule unit-testable
//! without a live daemon.
//!
//! Test: `super::staging::tests` covers every branch of both helpers.

use super::validate::ReindexOutcome;

/// Whether a reindex should stage into `index.redb.tmp` (non-destructive) or
/// write directly into the live `index.redb`.
///
/// Why: staging is now the default safety net for *every* reindex with a
/// durable corpus, not just `--force`. Indexes without a durable corpus
/// (BM25-only, ephemeral test indexers) cannot stage — there is no file to
/// swap — so they fall back to the legacy direct-write path.
/// What: returns `true` (stage) iff the index has a durable corpus store.
/// `force` no longer gates staging: a clean non-force reindex is just as
/// entitled to a rollback-safe rebuild as a forced one.
/// Test: `should_stage_*` below.
pub(crate) fn should_stage(has_durable_corpus: bool) -> bool {
    has_durable_corpus
}

/// Resolution of a finished, staged reindex: promote the staging corpus or
/// discard it and keep the live one.
///
/// Why: the orchestrator must make exactly one of two mutually-exclusive moves
/// on a staged reindex — atomically rename the tmp over the live file
/// (`Commit`) or delete the tmp and re-open the untouched live file
/// (`Rollback`). Modelling it as an enum guarantees the call site handles both
/// and never silently does neither.
/// What: `Commit` promotes; `Rollback { reason }` discards and surfaces why.
/// Test: `resolve_staging_*` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StagingResolution {
    /// Atomically swap the staging corpus over the live corpus.
    Commit,
    /// Discard the staging corpus; the live corpus is preserved untouched.
    Rollback { reason: String },
}

impl StagingResolution {
    /// True when the staging corpus should be promoted to live.
    pub(crate) fn is_commit(&self) -> bool {
        matches!(self, StagingResolution::Commit)
    }
}

/// Decide whether a staged reindex commits its swap or rolls back (#603 + #601).
///
/// Why: this is the single chokepoint that ties the non-empty validation gate
/// (#601) and memory-abort handling to the atomic swap (#603). A reindex only
/// promotes its freshly-staged corpus when it is (1) not memory-aborted and
/// (2) classified [`ReindexOutcome::Ready`]. Any other terminal state rolls
/// back, leaving the previous live corpus intact — exactly the safety net the
/// P0 needed.
/// What: returns `Rollback` when `memory_aborted` is true (partial corpus must
/// not be promoted) or when `outcome` is `Failed` (zero-vector embed failure);
/// otherwise `Commit`. The reason string is forwarded so the caller can log /
/// surface it.
/// Test: `resolve_staging_*` below — covers ready→commit, failed→rollback,
/// memory-abort→rollback, and the precedence (memory-abort wins over a Ready
/// outcome).
pub(crate) fn resolve_staging(memory_aborted: bool, outcome: &ReindexOutcome) -> StagingResolution {
    if memory_aborted {
        return StagingResolution::Rollback {
            reason: "reindex aborted on memory limit — staged corpus discarded, \
                     previous index preserved"
                .to_string(),
        };
    }
    match outcome {
        ReindexOutcome::Ready => StagingResolution::Commit,
        ReindexOutcome::Failed { reason } => StagingResolution::Rollback {
            reason: reason.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: an index with a durable corpus must always stage so it can roll back.
    /// Test: this test.
    #[test]
    fn should_stage_with_durable_corpus() {
        assert!(should_stage(true));
    }

    /// Why: a BM25-only / test index has no file to swap, so it cannot stage.
    /// Test: this test.
    #[test]
    fn should_stage_without_durable_corpus() {
        assert!(!should_stage(false));
    }

    /// Why: the healthy path — a Ready outcome with no memory abort promotes.
    /// Test: this test.
    #[test]
    fn resolve_staging_ready_commits() {
        let res = resolve_staging(false, &ReindexOutcome::Ready);
        assert!(res.is_commit());
    }

    /// Why: the #601 ↔ #603 link — a zero-vector failure must roll back so the
    /// live corpus survives the embedder outage.
    /// Test: this test.
    #[test]
    fn resolve_staging_failed_rolls_back() {
        let outcome = ReindexOutcome::Failed {
            reason: "zero vectors".to_string(),
        };
        let res = resolve_staging(false, &outcome);
        assert!(!res.is_commit());
        match res {
            StagingResolution::Rollback { reason } => assert!(reason.contains("zero vectors")),
            StagingResolution::Commit => unreachable!(),
        }
    }

    /// Why: a memory abort produced only a partial corpus; promoting it would
    /// publish a truncated index, so it must roll back even though the loop
    /// "completed".
    /// Test: this test.
    #[test]
    fn resolve_staging_memory_abort_rolls_back() {
        let res = resolve_staging(true, &ReindexOutcome::Ready);
        assert!(!res.is_commit());
    }

    /// Why: precedence — a memory abort wins over an otherwise-Ready outcome,
    /// because the partial corpus is the dominant safety concern.
    /// Test: this test.
    #[test]
    fn resolve_staging_memory_abort_beats_ready() {
        let res = resolve_staging(true, &ReindexOutcome::Ready);
        match res {
            StagingResolution::Rollback { reason } => assert!(reason.contains("memory limit")),
            StagingResolution::Commit => unreachable!("memory abort must roll back"),
        }
    }
}
