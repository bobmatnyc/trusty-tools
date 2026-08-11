//! The #2178 root-move trust gate, as one decision both callers share.
//!
//! Why: the gate has two call sites — `runner::run_reindex` (at spawn time) and
//! `server::reindex_handlers::reindex_handler` (at request time, added by
//! #4951). Each carried its own copy of the read-compare-refuse sequence, and
//! both copies degraded to "trusted" when either input failed to read:
//! `read_indexed_root().unwrap_or(None)` turned a redb error into "no prior
//! root", and `load_index_registry().ok()` turned an unparseable
//! `indexes.toml` into "no persisted entry", which
//! [`validate::root_move_is_trusted`] answers `true` for. Refusing costs a
//! retry; proceeding costs the corpus — this is the same class as the #402
//! root-hijack incident (#5357).
//!
//! What: [`evaluate_root_move`] takes both reads as `Result`s and returns
//! either a [`TrustedRoot`] or a [`RootMoveRefusal`] carrying the
//! operator-facing reason. A read failure is a refusal, never a `None`.
//! [`restore_indexer_root_after_refusal`] repairs the divergence a refused
//! move leaves behind.
//!
//! Test: `root_gate::tests` covers the four decision arms directly;
//! `service::reindex::root_hijack_tests` and `service::server::tests_5357`
//! drive them end-to-end through the runner and the HTTP handler.

use std::path::{Path, PathBuf};

use crate::core::registry::IndexHandle;
use crate::service::persistence::PersistedIndex;

use super::validate::{needs_path_relativization, root_move_is_trusted};

/// An accepted candidate root.
///
/// `moved` is the #602/#1073 "root moved between runs" signal the caller uses
/// to decide whether the hash cache must be cleared; `indexed_root` is the root
/// the corpus's chunk paths are currently relative to.
pub(crate) struct TrustedRoot {
    pub(crate) moved: bool,
    pub(crate) indexed_root: Option<PathBuf>,
}

/// A refused candidate root.
///
/// Why: both callers report the same refusal differently — the runner marks the
/// reindex failed and pushes a fatal SSE event, the handler returns `409` — so
/// the decision hands back the reason plus the two roots rather than rendering
/// either shape itself.
/// What: `reason` is operator-facing prose naming the durable alternative;
/// `indexed_root` and `persisted_root` are `None` when the corresponding read
/// is what failed.
/// Test: `root_gate::tests`.
pub(crate) struct RootMoveRefusal {
    pub(crate) reason: String,
    pub(crate) indexed_root: Option<PathBuf>,
    pub(crate) persisted_root: Option<PathBuf>,
}

/// Decide whether `candidate_root` may be walked (and later pruned) against.
///
/// Why: see the module docs. The gate exists because
/// `POST /indexes/:id/reindex` can repoint an index's in-memory `root_path`
/// (#63) without ever writing `indexes.toml`, so the in-memory value alone is
/// not evidence of a legitimate relocation (#2178).
/// What: refuses when the corpus's own last-indexed root cannot be read;
/// returns `moved: false` when the candidate matches that root; otherwise
/// refuses when the persisted registry cannot be read or names a different
/// root, and accepts with `moved: true` when it agrees (or has no entry for
/// this index at all — a fresh or test-only index has nothing durable to
/// validate against). `load_registry` is only called when a move was detected,
/// so the unmoved fast path still does no registry I/O.
/// Test: `refuses_when_the_indexed_root_read_fails`,
/// `refuses_when_the_registry_read_fails`, `refuses_an_untrusted_move`,
/// `accepts_a_move_that_matches_the_persisted_entry`,
/// `unmoved_root_is_accepted_without_reading_the_registry`.
pub(crate) fn evaluate_root_move(
    index_id: &str,
    indexed_root: anyhow::Result<Option<PathBuf>>,
    candidate_root: &Path,
    load_registry: impl FnOnce() -> anyhow::Result<Vec<PersistedIndex>>,
) -> Result<TrustedRoot, RootMoveRefusal> {
    // #5357: a failed read is not proof that this index has no prior root.
    let indexed_root = match indexed_root {
        Ok(root) => root,
        Err(e) => {
            return Err(RootMoveRefusal {
                reason: format!(
                    "refusing reindex of index '{index_id}': reading the corpus's \
                     last-indexed root failed ({e}), so the candidate root {} cannot \
                     be validated. A read failure is not proof that this index has no \
                     prior root, and walking an unvalidated root prunes the existing \
                     corpus against the wrong tree — retry once the corpus is \
                     readable (issues #2178, #5357)",
                    candidate_root.display(),
                ),
                indexed_root: None,
                persisted_root: None,
            })
        }
    };

    if !needs_path_relativization(indexed_root.as_deref(), candidate_root) {
        return Ok(TrustedRoot {
            moved: false,
            indexed_root,
        });
    }

    // #5357: same reasoning as above — an unparseable `indexes.toml` must not
    // read back as "this index has no persisted entry", which is the one input
    // `root_move_is_trusted` answers `true` for.
    let persisted_root = match load_registry() {
        Ok(entries) => entries
            .into_iter()
            .find(|e| e.id == index_id)
            .map(|e| e.root_path),
        Err(e) => {
            return Err(RootMoveRefusal {
                reason: format!(
                    "refusing reindex of index '{index_id}': the candidate root {} \
                     disagrees with the corpus's last-indexed root {indexed_root:?}, \
                     and reading the persisted indexes.toml entry that would settle \
                     it failed ({e}). Inspect or restore indexes.toml, then retry \
                     (issues #2178, #5357)",
                    candidate_root.display(),
                ),
                indexed_root,
                persisted_root: None,
            })
        }
    };

    if !root_move_is_trusted(persisted_root.as_deref(), candidate_root) {
        return Err(RootMoveRefusal {
            reason: format!(
                "refusing reindex of index '{index_id}': candidate root {} disagrees \
                 with the corpus's last-indexed root {indexed_root:?} and with this \
                 index's persisted indexes.toml root_path {persisted_root:?}. That is \
                 the #2178 root-hijack signature (an unpersisted root_path override or \
                 a stale in-memory handle), not a legitimate relocation — a reindex \
                 cannot durably move an index. Use POST /indexes/{index_id}/relocate \
                 for an explicit, persisted move (issues #2178, #4951)",
                candidate_root.display(),
            ),
            indexed_root,
            persisted_root,
        });
    }

    Ok(TrustedRoot {
        moved: true,
        indexed_root,
    })
}

/// Move the indexer onto the candidate root now that the gate has trusted it.
///
/// Why: `reindex_handler` used to apply the override's `set_root_path` at
/// request time, minutes before this gate re-read `indexes.toml` behind the
/// reindex semaphores (#4951). A registry write landing in between let the
/// handler accept a move the runner then refused, leaving the indexer on a root
/// the corpus was never relativized against (#5357). Deferring the move to here
/// means one read of the registry drives both the decision and the mutation, so
/// that window cannot produce a half-applied override at all. The handler still
/// syncs when NO move was detected — the corpus already agrees with the
/// candidate there, so there is nothing for this gate to decide.
/// What: no-op unless the indexer is somewhere other than the handle's root;
/// otherwise takes the indexer write lock and moves it. Called only on the
/// `moved: true` accepted path, before Phase 1 walks anything.
/// Test: `root_hijack_tests::reindex_moves_a_stale_indexer_onto_the_trusted_new_root`
/// — starts the indexer at the STALE root and proves this call is what moves
/// it; `root_hijack_tests::reindex_accepts_root_move_that_matches_persisted_config`
/// builds the indexer already at the target root, so the call is a no-op there.
pub(crate) async fn sync_indexer_root_after_trusted_move(handle: &IndexHandle) {
    let target = handle.root_path.clone();
    let mut indexer = handle.indexer.write().await;
    if indexer.root_path == target {
        return;
    }
    tracing::info!(
        "reindex[{}]: trusted root move — pointing the indexer from {} at {} so the \
         re-relativized chunks resolve against the root this run is walking (#5357)",
        handle.id.0,
        indexer.root_path.display(),
        target.display(),
    );
    indexer.set_root_path(target);
}

/// Point the indexer back at the root the corpus is actually relative to after
/// a refusal.
///
/// Why: the deferred sync above removes the handler as a source of this
/// divergence, but not every route to it — a stale in-memory handle, or any
/// other path that repointed the indexer, reaches the same state. Left there,
/// the indexer resolves root-relative chunk paths against the refused root while
/// the corpus is still relative to the old one, and `file_is_within_root` is a
/// lexical prefix test, so those dangling paths pass it and `stale_index_root`
/// reads `false`: a wrong answer reported as healthy. Restoring the indexer root
/// makes the same state fail closed (empty results, `stale_index_root: true`).
/// What: no-op when the indexer is already there, or when the refusal could not
/// determine the corpus's root — only the corpus-read-failure arm leaves
/// `indexed_root: None`, since the registry-read-failure arm reuses the root it
/// already read. Otherwise takes the indexer write lock and moves it back.
/// Test: `root_hijack_tests::reindex_refuses_untrusted_root_move_and_preserves_corpus`.
pub(crate) async fn restore_indexer_root_after_refusal(
    handle: &IndexHandle,
    refusal: &RootMoveRefusal,
) {
    let Some(indexed_root) = refusal.indexed_root.as_deref() else {
        return;
    };
    let mut indexer = handle.indexer.write().await;
    if indexer.root_path == indexed_root {
        return;
    }
    tracing::warn!(
        "reindex[{}]: restoring the indexer root to the corpus's own last-indexed \
         root {} after refusing {} — leaving it on the refused root would resolve \
         every chunk against a tree the corpus was never relativized against (#5357)",
        handle.id.0,
        indexed_root.display(),
        indexer.root_path.display(),
    );
    indexer.set_root_path(indexed_root.to_path_buf());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, root: &Path) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: root.to_path_buf(),
            ..Default::default()
        }
    }

    /// Why (#5357): the fail-open this issue exists to close — a redb read error
    /// must not read back as "no prior root", which skips the gate entirely.
    /// Test: this test.
    #[test]
    fn refuses_when_the_indexed_root_read_fails() {
        let refusal = evaluate_root_move(
            "idx",
            Err(anyhow::anyhow!("redb: open _meta table failed")),
            Path::new("/tmp/candidate"),
            || panic!("the registry must not be consulted once the corpus read failed"),
        )
        .err()
        .expect("a failed indexed_root read must refuse");
        assert!(refusal.reason.contains("last-indexed root failed"));
        assert!(refusal.indexed_root.is_none());
    }

    /// Why (#5357): the second fail-open — an unparseable `indexes.toml` must
    /// not read back as "no persisted entry", the one input
    /// `root_move_is_trusted` answers `true` for.
    /// Test: this test.
    #[test]
    fn refuses_when_the_registry_read_fails() {
        let refusal = evaluate_root_move(
            "idx",
            Ok(Some(PathBuf::from("/tmp/old"))),
            Path::new("/tmp/candidate"),
            || anyhow::bail!("indexes.toml could not be parsed"),
        )
        .err()
        .expect("a failed registry read must refuse");
        assert!(refusal.reason.contains("indexes.toml"));
        assert_eq!(refusal.indexed_root.as_deref(), Some(Path::new("/tmp/old")));
        assert!(refusal.persisted_root.is_none());
    }

    /// Why: the #2178 case itself — a move contradicted by the persisted entry.
    /// Test: this test.
    #[test]
    fn refuses_an_untrusted_move() {
        let refusal = evaluate_root_move(
            "idx",
            Ok(Some(PathBuf::from("/tmp/old"))),
            Path::new("/tmp/candidate"),
            || Ok(vec![entry("idx", Path::new("/tmp/old"))]),
        )
        .err()
        .expect("a move the registry contradicts must refuse");
        assert!(refusal.reason.contains("relocate"));
        assert_eq!(
            refusal.persisted_root.as_deref(),
            Some(Path::new("/tmp/old"))
        );
    }

    /// Why: the legitimate `POST /indexes/:id/relocate` shape must keep working
    /// — it persists the new root BEFORE the handle is swapped.
    /// Test: this test.
    #[test]
    fn accepts_a_move_that_matches_the_persisted_entry() {
        let trusted = evaluate_root_move(
            "idx",
            Ok(Some(PathBuf::from("/tmp/old"))),
            Path::new("/tmp/new"),
            || Ok(vec![entry("idx", Path::new("/tmp/new"))]),
        )
        .ok()
        .expect("a move matching the persisted entry must be trusted");
        assert!(trusted.moved);
    }

    /// Why: the overwhelmingly common path — no move, so no registry read and
    /// no hash-cache clear.
    /// Test: this test.
    #[test]
    fn unmoved_root_is_accepted_without_reading_the_registry() {
        let trusted = evaluate_root_move(
            "idx",
            Ok(Some(PathBuf::from("/tmp/same"))),
            Path::new("/tmp/same"),
            || panic!("an unmoved root must not read the registry"),
        )
        .ok()
        .expect("an unmoved root is always trusted");
        assert!(!trusted.moved);
    }
}
