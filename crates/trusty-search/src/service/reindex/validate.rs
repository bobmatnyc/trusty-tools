//! Pure reindex decision logic: non-empty validation (#601) and path /
//! migration portability decisions (#602).
//!
//! Why: the reindex orchestrator (`super`) is a 3000-line side-effect-heavy
//! tokio task that is impossible to unit-test end-to-end without a live
//! embedder daemon. The *decisions* it makes — "did the embedder silently
//! produce zero vectors?", "is `ctx.root` aligned with the walker's
//! canonical root so chunk paths stay portable?", "must the path-relativization
//! migration re-run because the root changed?" — are pure functions of a few
//! counters and paths. Extracting them here makes each decision independently
//! testable and keeps the monolith from growing.
//!
//! What: three pure helpers —
//! - [`reindex_outcome`] decides Ready vs. Failed from the vector/file counters
//!   (the #601 non-empty gate), honouring the lexical-only exception.
//! - [`canonical_walk_root`] canonicalizes a root exactly as the walker does so
//!   `strip_prefix` reliably yields root-relative (portable) chunk paths (#602).
//! - [`needs_path_relativization`] decides whether a root change between reindex
//!   runs should re-trigger path relativization (#602).
//!
//! Test: `super::validate::tests` covers every branch of all three.

use std::path::{Path, PathBuf};

/// Terminal classification of a finished batch loop, before any durable swap.
///
/// Why: the orchestrator needs a single value that captures *both* "is the
/// rebuilt corpus healthy enough to promote?" and "what reason do we surface
/// if not?". Folding the decision into one enum means the swap-vs-rollback
/// branch (#603) and the status-marking branch (#601) read the same verdict,
/// so they can never disagree (e.g. promote a corpus we also marked failed).
/// What: `Ready` when the corpus should be promoted and marked ready; `Failed`
/// when the embedder produced no vectors despite files being walked on a
/// full-pipeline index — the staging corpus must be discarded and the index
/// marked failed with `reason`.
/// Test: `reindex_outcome_*` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReindexOutcome {
    /// The rebuilt corpus is healthy: promote staging (if any) and mark ready.
    Ready,
    /// Embedding failed for every batch: discard staging, mark the index
    /// failed, and surface `reason` on the SSE `error` event / status.
    Failed { reason: String },
}

impl ReindexOutcome {
    /// True when the corpus should be promoted / marked ready.
    pub(crate) fn is_ready(&self) -> bool {
        matches!(self, ReindexOutcome::Ready)
    }

    /// The failure reason, if this is a `Failed` outcome.
    pub(crate) fn failure_reason(&self) -> Option<&str> {
        match self {
            ReindexOutcome::Failed { reason } => Some(reason.as_str()),
            ReindexOutcome::Ready => None,
        }
    }
}

/// Decide whether a finished reindex produced a usable corpus or silently
/// embedded nothing (#601).
///
/// Why: before this gate, the orchestrator marked semantic + graph `Ready`
/// unconditionally after the batch loop drained. When every embed batch failed
/// (sidecar crash, OOM, model-load stall), the index flipped to `ready` with
/// `chunk_count == 0` and `/health` served a dead index as green. Embedding
/// failure must be LOUD: a full-pipeline index that walked files but produced
/// zero vectors is broken, not ready.
/// What: returns [`ReindexOutcome::Failed`] iff the index is **not**
/// lexical-only, an embedder is wired (`embedder_present`), at least one file
/// was walked, and `total_vector_count == 0`. A `lexical_only` index, or any
/// index with no embedder configured (BM25-only / test indexer), legitimately
/// has zero vectors, so it is always `Ready`. An index that walked zero files
/// (empty repo / over-aggressive filter) is `Ready` too — that is an
/// empty-but-valid corpus, not an embedder failure, and is reported separately
/// via walk diagnostics.
/// Test: `reindex_outcome_*` below — covers the lexical-only exception, the
/// no-embedder exception, the zero-files exception, the zero-vector failure,
/// and the healthy path.
pub(crate) fn reindex_outcome(
    lexical_only: bool,
    embedder_present: bool,
    walked_files: usize,
    total_vector_count: usize,
) -> ReindexOutcome {
    if lexical_only {
        // Lexical-only indexes never embed; zero vectors is expected.
        return ReindexOutcome::Ready;
    }
    if !embedder_present {
        // No embedder wired (BM25-only / test indexer): zero vectors is the
        // expected, healthy steady state — not a failure.
        return ReindexOutcome::Ready;
    }
    if walked_files == 0 {
        // Nothing to embed: an empty (but valid) corpus, not a failure.
        return ReindexOutcome::Ready;
    }
    if total_vector_count == 0 {
        return ReindexOutcome::Failed {
            reason: format!(
                "embedding produced zero vectors for {walked_files} walked file(s) — \
                 the embedder backend likely failed for every batch (sidecar crash, \
                 OOM, or model-load stall). The previous index was preserved; \
                 check the embedderd logs and retry."
            ),
        };
    }
    ReindexOutcome::Ready
}

/// Canonicalize `root` exactly as the walker does (#602).
///
/// Why: `walk_source_files_with_options` canonicalizes its root via
/// `std::fs::canonicalize` and returns every file path *under that canonical
/// root*. The reindex orchestrator, however, built `ctx.root` from the raw
/// `handle.root_path`. When `root_path` carried a symlink alias (macOS
/// `/var` → `/private/var`, a developer symlinked checkout, a different mount
/// on the serving host) the raw root did **not** prefix the canonical walked
/// paths, so `path.strip_prefix(&ctx.root)` failed and the `#402` fallback
/// stored an **absolute** path. Those absolute paths then fail to resolve on a
/// serving host with a different mount. Canonicalizing the strip-prefix root
/// the same way the walker does makes `strip_prefix` succeed, so chunk paths
/// are always root-relative and portable.
/// What: returns `std::fs::canonicalize(root)` on success, falling back to the
/// input `root` when canonicalization fails (TOCTOU unlink, permission error)
/// — identical to the walker's fallback so the two never diverge.
/// Test: `canonical_walk_root_*` below (resolves a symlinked root; falls back
/// on a non-existent path).
pub(crate) fn canonical_walk_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Decide whether path-relativization must re-run because the index root moved
/// between reindex runs (#602).
///
/// Why: chunk `file` fields are stored relative to the root that was current
/// when they were written. If an operator re-registers the same index under a
/// new `root_path` (project moved on disk, served from a different mount) and
/// then runs an *incremental* reindex (force=false), the content-hash cache
/// skips unchanged files, so their stored paths are never rewritten — they stay
/// relative to the *old* root and silently resolve wrong. Detecting a root
/// change lets the orchestrator force every file through the rewrite path
/// (clear the hash cache) so the whole corpus is relativized against the new
/// root.
/// What: returns `true` iff `previous_root` is `Some` and its canonical form
/// differs from the canonical form of `current_root`. A first-ever reindex
/// (`previous_root == None`) returns `false` — there is nothing to relativize
/// against a prior root. Both sides are canonicalized so a pure symlink-alias
/// change (same target) is **not** treated as a move.
/// Test: `needs_path_relativization_*` below.
pub(crate) fn needs_path_relativization(previous_root: Option<&Path>, current_root: &Path) -> bool {
    let Some(prev) = previous_root else {
        return false;
    };
    canonical_walk_root(prev) != canonical_walk_root(current_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a lexical-only index never embeds, so zero vectors is the correct,
    /// healthy steady state — failing it would break every BM25-only deployment.
    /// Test: this test.
    #[test]
    fn reindex_outcome_lexical_only_is_ready_with_zero_vectors() {
        // lexical_only=true, embedder irrelevant.
        let outcome = reindex_outcome(true, true, 100, 0);
        assert!(outcome.is_ready());
        assert_eq!(outcome.failure_reason(), None);
    }

    /// Why: a non-lexical index with NO embedder wired (BM25-only / test
    /// indexer) legitimately produces zero vectors — it must not be flagged as
    /// failed, or every embedder-less reindex would break.
    /// Test: this test.
    #[test]
    fn reindex_outcome_no_embedder_is_ready_with_zero_vectors() {
        let outcome = reindex_outcome(false, false, 100, 0);
        assert!(outcome.is_ready());
        assert_eq!(outcome.failure_reason(), None);
    }

    /// Why: the core #601 bug — a full-pipeline index WITH an embedder that
    /// walked files but embedded nothing is broken and must be marked failed.
    /// Test: this test.
    #[test]
    fn reindex_outcome_full_pipeline_zero_vectors_fails() {
        let outcome = reindex_outcome(false, true, 42, 0);
        assert!(!outcome.is_ready());
        let reason = outcome.failure_reason().expect("must carry a reason");
        assert!(reason.contains("zero vectors"), "reason: {reason}");
        assert!(
            reason.contains("42"),
            "reason should cite file count: {reason}"
        );
    }

    /// Why: an empty repo (or an over-aggressive filter) walks zero files; that
    /// is an empty-but-valid corpus, not an embedder failure, so it must not be
    /// marked failed (which would block the index forever).
    /// Test: this test.
    #[test]
    fn reindex_outcome_zero_files_is_ready() {
        assert!(reindex_outcome(false, true, 0, 0).is_ready());
        assert!(reindex_outcome(true, true, 0, 0).is_ready());
    }

    /// Why: the healthy path — files walked, vectors produced — must be Ready.
    /// Test: this test.
    #[test]
    fn reindex_outcome_healthy_is_ready() {
        assert!(reindex_outcome(false, true, 42, 1337).is_ready());
    }

    /// Why: a single embedded vector for many files is still "the embedder
    /// worked" — the gate only fires on *zero* vectors, never on a partial
    /// embed (partial embeds are surfaced via `embed_failure_count`, not the
    /// hard gate).
    /// Test: this test.
    #[test]
    fn reindex_outcome_single_vector_is_ready() {
        assert!(reindex_outcome(false, true, 1000, 1).is_ready());
    }

    /// Why: confirms the strip-prefix root resolves a real symlinked directory
    /// to the same canonical path the walker uses, so `strip_prefix` succeeds.
    /// Test: this test.
    #[test]
    fn canonical_walk_root_resolves_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        std::fs::create_dir(&link).unwrap();

        let canonical = canonical_walk_root(&link);
        let real_canonical = std::fs::canonicalize(&real).unwrap();
        #[cfg(unix)]
        assert_eq!(
            canonical, real_canonical,
            "symlinked root must canonicalize to the real target"
        );
        #[cfg(not(unix))]
        let _ = real_canonical;
    }

    /// Why: a non-existent path cannot be canonicalized; the helper must fall
    /// back to the input rather than panic (matches the walker's fallback).
    /// Test: this test.
    #[test]
    fn canonical_walk_root_falls_back_on_missing_path() {
        let missing = PathBuf::from("/this/path/does/not/exist/anywhere/xyz");
        assert_eq!(canonical_walk_root(&missing), missing);
    }

    /// Why: a first-ever reindex has no prior root, so there is nothing to
    /// relativize against — must return false.
    /// Test: this test.
    #[test]
    fn needs_path_relativization_first_reindex_is_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!needs_path_relativization(None, tmp.path()));
    }

    /// Why: re-registering the same index under a genuinely different root must
    /// re-trigger relativization so stored paths track the new root.
    /// Test: this test.
    #[test]
    fn needs_path_relativization_root_moved_is_true() {
        let a = tempfile::tempdir().expect("tempdir a");
        let b = tempfile::tempdir().expect("tempdir b");
        assert!(needs_path_relativization(Some(a.path()), b.path()));
    }

    /// Why: an unchanged root (same canonical target) must NOT force a full
    /// rewrite — that would defeat the incremental-reindex fast path on every
    /// run.
    /// Test: this test.
    #[test]
    fn needs_path_relativization_same_root_is_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        assert!(!needs_path_relativization(Some(root), root));
    }

    /// Why: a pure symlink-alias change that points at the same real directory
    /// is not a move — canonicalization collapses both sides, so no rewrite.
    /// Test: this test.
    #[cfg(unix)]
    #[test]
    fn needs_path_relativization_symlink_alias_is_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // prev = symlink alias, current = real target → same canonical root.
        assert!(!needs_path_relativization(Some(&link), &real));
    }
}
