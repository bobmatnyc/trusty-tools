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
//! What: four pure helpers —
//! - [`reindex_outcome`] decides Ready vs. Failed from the vector/file counters
//!   (the #601 non-empty gate), honouring the lexical-only exception.
//! - [`canonical_walk_root`] canonicalizes a root exactly as the walker does so
//!   `strip_prefix` reliably yields root-relative (portable) chunk paths (#602).
//! - [`needs_path_relativization`] decides whether a root change between reindex
//!   runs should re-trigger path relativization (#602).
//! - [`root_move_is_trusted`] decides whether a detected root move is backed by
//!   durable, persisted config before the caller may walk/prune against it (#2178).
//!
//! Test: `super::validate::tests` covers every branch of all four.

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
/// embedded nothing (#601, #868).
///
/// Why: before this gate, the orchestrator marked semantic + graph `Ready`
/// unconditionally after the batch loop drained. When every embed batch failed
/// (sidecar crash, OOM, model-load stall), the index flipped to `ready` with
/// `chunk_count == 0` and `/health` served a dead index as green. Embedding
/// failure must be LOUD: a full-pipeline index that walked files but produced
/// zero vectors is broken, not ready.
///
/// The `skipped_files` fix (#868): `walked_files` is the raw filesystem-walker
/// count BEFORE hash-skip/minified-skip filtering. On a no-change incremental
/// reindex every file is hash-skipped, so `walked_files > 0` but zero files
/// are actually submitted to the embedder — zero vectors is EXPECTED, not a
/// crash. The old guard misfired on this case, rolling back the staging corpus
/// and degrading the index to BM25-only until the next forced reindex. The fix
/// computes `newly_submitted = walked_files.saturating_sub(skipped_files)` and
/// only fires `Failed` when files were actually sent to the embedder.
///
/// What: returns [`ReindexOutcome::Failed`] iff the index is **not**
/// lexical-only, an embedder is wired (`embedder_present`), at least one file
/// was walked, at least one file was newly submitted for embedding (i.e. not
/// hash-skipped), and `total_vector_count == 0`. A `lexical_only` index, any
/// index with no embedder configured (BM25-only / test indexer), an index
/// that walked zero files (empty repo / over-aggressive filter), or an
/// incremental reindex where all files were hash-skipped (`newly_submitted ==
/// 0`) are all `Ready` — none of those is an embedder failure.
/// Test: `reindex_outcome_*` below — covers the lexical-only exception, the
/// no-embedder exception, the zero-files exception, the all-hash-skipped
/// warm-boot case (#868 regression), the genuine crash, partial-skip crash,
/// partial-skip success, and the healthy path.
pub(crate) fn reindex_outcome(
    lexical_only: bool,
    defer_embed: bool,
    embedder_present: bool,
    walked_files: usize,
    skipped_files: usize,
    total_vector_count: usize,
) -> ReindexOutcome {
    if lexical_only {
        // Lexical-only indexes never embed; zero vectors is expected.
        return ReindexOutcome::Ready;
    }
    if defer_embed {
        // Issue #923: in deferred-embed mode the fast pass intentionally
        // produces zero vectors — embedding runs as a separate background job.
        // The zero-vector gate does not apply here.
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
    // #868: files actually submitted to the embedder after hash-skip / minified
    // filtering. On a warm no-change reindex this is 0 — zero vectors is then
    // EXPECTED, not an embedder crash. Only fire Failed when the embedder was
    // genuinely invoked but produced nothing.
    let newly_submitted = walked_files.saturating_sub(skipped_files);
    if newly_submitted == 0 {
        // All files were hash-skipped (or minified-skipped); the embedder was
        // never called. Zero vectors is the correct outcome — not a failure.
        return ReindexOutcome::Ready;
    }
    if total_vector_count == 0 {
        return ReindexOutcome::Failed {
            reason: format!(
                "embedding produced zero vectors for {newly_submitted} submitted file(s) \
                 ({walked_files} walked, {skipped_files} hash-skipped) — \
                 the embedder backend likely failed for every batch (sidecar crash, \
                 OOM, or model-load stall). The previous index was preserved; \
                 check the embedderd logs and retry."
            ),
        };
    }
    ReindexOutcome::Ready
}

/// Issue #2211: decide whether the semantic stage may be marked `Ready`
/// immediately after the C1 fast pass completes, or must wait for a
/// deferred-embed background pass to actually finish first.
///
/// Why: `defer_embed` (issue #923) intentionally embeds nothing synchronously
/// during the fast pass — the real embedding runs in the background job
/// spawned by `spawn_deferred_embed_pass` right after `finish_reindex`
/// returns. Before this gate, `finish_reindex` called
/// `mark_semantic_ready_graph_in_progress` unconditionally after the batch
/// loop, which flipped `stages.semantic.status` to `Ready` — carrying
/// whatever tiny `total_vector_count` happened to be embedded synchronously
/// before deferral kicked in (observed in production: `embedded: 512` out of
/// `total: 48742`) — regardless of `defer_embed`. The deferred background
/// pass (`defer_embed.rs`) never resets the status back to `InProgress`, so
/// `semantic.status` stayed `"ready"` for the ENTIRE duration of the real
/// background embed — a false-green signal that callers treating `status`
/// as authoritative (health checks) trusted immediately after a reindex was
/// triggered.
/// What: returns `true` (safe to mark Ready now) whenever embedding actually
/// ran to completion as part of THIS pass — i.e. not deferred, or no
/// embedder is wired at all (in which case `defer_embed` is a no-op and
/// `total_vector_count` is definitionally final). Returns `false` only when
/// a real deferred background pass is about to be spawned; in that case the
/// semantic stage is left `InProgress` (already set earlier by
/// `mark_lexical_ready_semantic_in_progress`) until
/// `spawn_deferred_embed_pass` itself flips it to `Ready` once embedding
/// truly completes.
/// Test: `semantic_ready_now_*` below.
pub(crate) fn semantic_ready_now(defer_embed: bool, embedder_present: bool) -> bool {
    !(defer_embed && embedder_present)
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

/// Decide whether a detected root move is safe to walk/prune against (#2178).
///
/// Why: incident #2178 — a live daemon ran `reindex -i cto`, and the #402/#1073
/// "colocated root moved" heuristic (see [`needs_path_relativization`]) decided
/// `cto`'s root had moved from its real, persisted location to an unrelated git
/// worktree that merely happened to also have colocated `.trusty-search/`
/// storage (this very workspace self-indexes). The heuristic's only legitimacy
/// check was `has_colocated_storage(candidate)` — trivially satisfied by ANY
/// colocated project, not just the right one. The daemon walked the unrelated
/// worktree (2,506 files) and the post-loop prune pass then deleted every one
/// of the real corpus's 369,568 chunks that weren't seen in that walk. The
/// root cause: `POST /indexes/:id/reindex` accepts a caller-supplied
/// `root_path` override (issue #63) that is swapped into the in-memory
/// registry entry but is **never** written to `indexes.toml` — so the
/// in-memory `IndexHandle::root_path` can silently diverge from the durably
/// persisted source of truth, and the old heuristic trusted the in-memory
/// value unconditionally.
///
/// What: returns `true` iff the candidate root should be trusted enough to
/// walk and (eventually) prune the corpus against — either there is no
/// persisted `indexes.toml` entry for this index at all (a fresh or
/// test-only index has nothing durable to validate against, so the existing
/// in-memory value is trusted by default), or the candidate's canonical form
/// matches the persisted entry's canonical `root_path`. Returns `false` when
/// a persisted entry exists and disagrees — the caller MUST refuse to
/// walk/prune against the candidate in that case (abort with a clear error
/// rather than silently reindexing/pruning the wrong tree). This intentionally
/// narrows the #402/#1073 auto-detected-move convenience: an operator-driven
/// relocation must go through `POST /indexes/:id/relocate`, which persists the
/// new `root_path` to `indexes.toml` BEFORE the handle is swapped, so it
/// always passes this check.
/// Test: `root_move_is_trusted_*` below.
pub(crate) fn root_move_is_trusted(persisted_root: Option<&Path>, candidate_root: &Path) -> bool {
    let Some(persisted) = persisted_root else {
        return true;
    };
    canonical_walk_root(persisted) == canonical_walk_root(candidate_root)
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
        let outcome = reindex_outcome(true, false, true, 100, 0, 0);
        assert!(outcome.is_ready());
        assert_eq!(outcome.failure_reason(), None);
    }

    /// Why: a non-lexical index with NO embedder wired (BM25-only / test
    /// indexer) legitimately produces zero vectors — it must not be flagged as
    /// failed, or every embedder-less reindex would break.
    /// Test: this test.
    #[test]
    fn reindex_outcome_no_embedder_is_ready_with_zero_vectors() {
        let outcome = reindex_outcome(false, false, false, 100, 0, 0);
        assert!(outcome.is_ready());
        assert_eq!(outcome.failure_reason(), None);
    }

    /// Why: an empty repo (or an over-aggressive filter) walks zero files; that
    /// is an empty-but-valid corpus, not an embedder failure, so it must not be
    /// marked failed (which would block the index forever).
    /// Test: this test.
    #[test]
    fn reindex_outcome_zero_files_is_ready() {
        assert!(reindex_outcome(false, false, true, 0, 0, 0).is_ready());
        assert!(reindex_outcome(true, false, true, 0, 0, 0).is_ready());
    }

    /// Why: the healthy path — files walked, vectors produced — must be Ready.
    /// Test: this test.
    #[test]
    fn reindex_outcome_healthy_is_ready() {
        assert!(reindex_outcome(false, false, true, 42, 0, 1337).is_ready());
    }

    /// Why: a single embedded vector for many files is still "the embedder
    /// worked" — the gate only fires on *zero* vectors, never on a partial
    /// embed (partial embeds are surfaced via `embed_failure_count`, not the
    /// hard gate).
    /// Test: this test.
    #[test]
    fn reindex_outcome_single_vector_is_ready() {
        assert!(reindex_outcome(false, false, true, 1000, 0, 1).is_ready());
    }

    /// Why: regression test for #868 — a warm-boot incremental reindex where
    /// all files are hash-skipped produces zero new vectors by design. The old
    /// guard misfired here, rolling back the staging corpus and degrading the
    /// index to BM25-only. With `skipped_files == walked_files`, `newly_submitted
    /// == 0`, so the guard must return Ready.
    /// Test: this test.
    #[test]
    fn reindex_outcome_all_hash_skipped_is_ready() {
        // #868 scenario: 24 files walked, all 24 hash-skipped, 0 new vectors.
        let outcome = reindex_outcome(false, false, true, 24, 24, 0);
        assert!(
            outcome.is_ready(),
            "all-hash-skipped warm reindex must be Ready, got: {:?}",
            outcome
        );
        assert_eq!(outcome.failure_reason(), None);
    }

    /// Why: when files were actually submitted to the embedder (not all skipped)
    /// and zero vectors came back, that is a genuine embedder crash — must fail.
    /// Test: this test.
    #[test]
    fn reindex_outcome_genuine_crash_fails() {
        // 24 walked, 0 skipped → 24 submitted, 0 vectors → crash.
        let outcome = reindex_outcome(false, false, true, 24, 0, 0);
        assert!(!outcome.is_ready());
        let reason = outcome.failure_reason().expect("must carry a reason");
        assert!(reason.contains("zero vectors"), "reason: {reason}");
        assert!(
            reason.contains("24"),
            "reason should cite submitted count: {reason}"
        );
    }

    /// Why: partial skip with a crash — some files were submitted but none
    /// embedded. Must fail (the embedder was invoked and produced nothing).
    /// Test: this test.
    #[test]
    fn reindex_outcome_partial_skip_crash_fails() {
        // 24 walked, 20 skipped → 4 submitted, 0 vectors → crash.
        let outcome = reindex_outcome(false, false, true, 24, 20, 0);
        assert!(!outcome.is_ready());
        let reason = outcome.failure_reason().expect("must carry a reason");
        assert!(reason.contains("zero vectors"), "reason: {reason}");
    }

    /// Why: partial skip where the submitted files were successfully embedded.
    /// Must be Ready (the embedder worked for the files it was given).
    /// Test: this test.
    #[test]
    fn reindex_outcome_partial_skip_success_is_ready() {
        // 24 walked, 20 skipped → 4 submitted, 12 vectors → healthy.
        let outcome = reindex_outcome(false, false, true, 24, 20, 12);
        assert!(outcome.is_ready());
        assert_eq!(outcome.failure_reason(), None);
    }

    /// Why: the core #601 bug — a full-pipeline index WITH an embedder that
    /// walked files but embedded nothing is broken and must be marked failed.
    /// This covers the case with no hash-skips (all files newly submitted).
    /// Test: this test.
    #[test]
    fn reindex_outcome_full_pipeline_zero_vectors_fails() {
        let outcome = reindex_outcome(false, false, true, 42, 0, 0);
        assert!(!outcome.is_ready());
        let reason = outcome.failure_reason().expect("must carry a reason");
        assert!(reason.contains("zero vectors"), "reason: {reason}");
        assert!(
            reason.contains("42"),
            "reason should cite submitted count: {reason}"
        );
    }

    /// Why (issue #923): in defer-embed mode the fast pass intentionally
    /// produces zero vectors — the zero-vector gate must not fire.
    /// Test: this test.
    #[test]
    fn reindex_outcome_defer_embed_is_ready_with_zero_vectors() {
        // defer_embed=true, embedder present, files walked, zero vectors — must be Ready.
        let outcome = reindex_outcome(false, true, true, 50, 0, 0);
        assert!(
            outcome.is_ready(),
            "defer_embed fast pass must be Ready despite zero vectors"
        );
        assert_eq!(outcome.failure_reason(), None);
    }

    /// Issue #2211 regression: when `defer_embed` is active AND an embedder
    /// is wired, the semantic stage must NOT be marked Ready right after the
    /// fast pass — the real embedding happens in a background pass that
    /// hasn't started yet.
    /// Test: this test.
    #[test]
    fn semantic_ready_now_false_when_deferring_with_embedder() {
        assert!(
            !semantic_ready_now(true, true),
            "must NOT mark semantic Ready synchronously when a real deferred embed pass \
             is about to be spawned"
        );
    }

    /// Issue #2211: without `defer_embed`, embedding always ran synchronously
    /// as part of this pass, so semantic Ready reflects the true, final state.
    /// Test: this test.
    #[test]
    fn semantic_ready_now_true_when_not_deferring() {
        assert!(semantic_ready_now(false, true));
        assert!(semantic_ready_now(false, false));
    }

    /// Issue #2211: `defer_embed=true` with NO embedder wired is a no-op —
    /// there is no background pass to wait for (BM25-only / test indexer),
    /// so it is safe to mark semantic Ready immediately (it stays vacuously
    /// "ready" with nothing to embed).
    /// Test: this test.
    #[test]
    fn semantic_ready_now_true_when_deferring_without_embedder() {
        assert!(
            semantic_ready_now(true, false),
            "defer_embed without an embedder has no background pass to wait for"
        );
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

    /// Why: a fresh/test index has no persisted `indexes.toml` entry to check
    /// against — nothing to validate, so the existing in-memory root is
    /// trusted by default (must not regress indexes that never persist).
    /// Test: this test.
    #[test]
    fn root_move_is_trusted_no_persisted_entry_is_trusted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(root_move_is_trusted(None, tmp.path()));
    }

    /// Why: the legitimate case — the candidate root IS the persisted
    /// `indexes.toml` root_path (e.g. after `POST /indexes/:id/relocate`,
    /// which persists before swapping the handle). Must be trusted.
    /// Test: this test.
    #[test]
    fn root_move_is_trusted_matches_persisted_is_trusted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(root_move_is_trusted(Some(tmp.path()), tmp.path()));
    }

    /// Why: the #2178 incident — the in-memory candidate root diverges from
    /// the persisted `indexes.toml` root_path (e.g. an unpersisted `root_path`
    /// override on `POST /indexes/:id/reindex`, or a CWD-derived accident).
    /// Must NOT be trusted — this is exactly the corpus-wipe trigger.
    /// Test: this test.
    #[test]
    fn root_move_is_trusted_diverges_from_persisted_is_untrusted() {
        let real_root = tempfile::tempdir().expect("tempdir a");
        let hijacked_root = tempfile::tempdir().expect("tempdir b");
        assert!(!root_move_is_trusted(
            Some(real_root.path()),
            hijacked_root.path()
        ));
    }

    /// Why: a pure symlink-alias against the persisted root (same real target)
    /// must not be treated as a divergence — canonicalization collapses both
    /// sides.
    /// Test: this test.
    #[cfg(unix)]
    #[test]
    fn root_move_is_trusted_symlink_alias_of_persisted_is_trusted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(root_move_is_trusted(Some(&real), &link));
    }
}
