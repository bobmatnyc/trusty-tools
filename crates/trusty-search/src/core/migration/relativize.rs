//! Shared absolute → root-relative corpus rewrite for M002 and M004 (#7923).
//!
//! Why: M002 and M004 carried byte-identical copies of a rewrite that lost
//! rows silently in two shapes. (1) A chunk stored under an absolute id whose
//! root-relative twin already existed was upserted onto the twin's key —
//! replacing the live row's content with the stale absolute row's — and then
//! its own key was deleted: one row gone per pair. (2) A chunk whose id does
//! not start with its stored `file` spelling (a symlink or mount alias, #674)
//! kept the same id after the rewrite, so the upsert and the delete hit the
//! same key and the row vanished. #7923 observed 19,639 chunks migrated and
//! 19,544 served, on a boot whose M003 relativized 112 absolute keys.
//! What: [`relativize_corpus_paths`] plans candidate rewrites from a load and
//! applies them in ONE redb write transaction
//! ([`crate::core::corpus::CorpusStore::apply_path_rewrites`]) that re-reads
//! every row it touches. A target id held at that moment — including by a row
//! written after the load — keeps the source row under its absolute id and is
//! reported with a count and a reason; a source row that changed in any field
//! or vanished since the load is skipped and reported; the row count is
//! checked inside the transaction and a mismatch aborts it. No committed state
//! exists between an insert and its remove, so a crash leaves the corpus as it
//! was and the retry converges.
//! Test: `relativize_preserves_every_chunk_for_m002_and_m004`,
//! `failed_rewrite_commits_nothing_and_retry_converges`,
//! `rows_written_after_the_load_are_never_overwritten`,
//! `rows_updated_in_place_after_the_load_keep_their_new_content`.

use std::path::Path;

use anyhow::{Context, Result};

use crate::core::chunker::RawChunk;
use crate::core::corpus::PathRewrite;
use crate::core::registry::IndexHandle;

/// Outcome of one relativization pass.
///
/// Why: a chunk the pass declines to rewrite must be visible as a count and a
/// reason, not only as a log line, so callers and tests can assert it.
/// What: `rewritten` rows moved to root-relative form; `kept_on_collision`
/// holds the absolute ids left in place because their root-relative id was
/// taken; `skipped_changed` counts planned rewrites whose source row changed
/// before the transaction; `outside_root` counts absolute files not under root.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RelativizeReport {
    pub rewritten: usize,
    pub kept_on_collision: Vec<String>,
    pub skipped_changed: usize,
    pub outside_root: usize,
}

/// Why the rewrite declines a colliding row; logged verbatim.
pub(crate) const COLLISION_REASON: &str =
    "its root-relative id is already held by another row, so rewriting it would \
     overwrite that row";

#[derive(Default)]
struct RewritePlan {
    rewrites: Vec<PathRewrite>,
    outside_root: Vec<String>,
}

/// Plan a rewrite for every absolute chunk under `root`; decisions about
/// collisions are left to the transaction.
fn plan_rewrites(chunks: Vec<RawChunk>, root: &Path) -> RewritePlan {
    let mut plan = RewritePlan::default();
    for mut chunk in chunks {
        if !Path::new(&chunk.file).is_absolute() {
            continue;
        }
        let Ok(rel) = Path::new(&chunk.file).strip_prefix(root) else {
            plan.outside_root.push(chunk.file);
            continue;
        };
        let rel_str = rel.to_string_lossy().into_owned();
        let old_id = chunk.id.clone();
        let old_file = std::mem::replace(&mut chunk.file, rel_str);
        // #7923: an id that does not embed its file spelling keeps its id; the
        // transaction rewrites `file` in place and removes nothing.
        chunk.id = reconstruct_id(&old_id, &old_file, &chunk.file);
        plan.rewrites.push(PathRewrite {
            old_id,
            old_file,
            chunk,
        });
    }
    plan
}

/// Rewrite absolute chunk `file`/`id` pairs under `index.root_path` to
/// root-relative form without losing or overwriting a row.
///
/// Why: see the module doc; M002 (#402) and M004 (#674) both call this.
/// What: loads the corpus, plans rewrites ([`plan_rewrites`]), applies them in
/// one transaction, logs out-of-root files, collisions and skipped rows with
/// counts, and refreshes the live BM25 + chunk map when anything was
/// rewritten. No corpus is a no-op; a failed transaction is `Err` with nothing
/// committed, so the schema version is not stamped. `label` prefixes logs.
/// Test: `relativize_preserves_every_chunk_for_m002_and_m004`,
/// `relativize_reports_collisions_and_is_idempotent`.
pub(crate) async fn relativize_corpus_paths(
    index: &IndexHandle,
    label: &'static str,
) -> Result<RelativizeReport> {
    let (corpus, root_path) = {
        let indexer = index.indexer.read().await;
        (indexer.corpus_store(), index.root_path.clone())
    };
    let Some(corpus) = corpus else {
        tracing::debug!(index_id = %index.id, "{label}: no durable corpus, skipping");
        return Ok(RelativizeReport::default());
    };

    let all_chunks = tokio::task::spawn_blocking({
        let corpus = std::sync::Arc::clone(&corpus);
        move || corpus.load_all_chunks()
    })
    .await
    .with_context(|| format!("{label}: load_all_chunks task panicked"))?
    .with_context(|| format!("{label}: failed to load chunks from corpus"))?;

    let plan = plan_rewrites(all_chunks, &root_path);
    let mut report = RelativizeReport {
        outside_root: plan.outside_root.len(),
        ..RelativizeReport::default()
    };
    if !plan.outside_root.is_empty() {
        tracing::warn!(
            index_id = %index.id,
            count = plan.outside_root.len(),
            root = %root_path.display(),
            sample = ?plan.outside_root.iter().take(5).collect::<Vec<_>>(),
            "{label}: chunk file is absolute but not under root_path; left unchanged"
        );
    }
    if plan.rewrites.is_empty() {
        tracing::info!(index_id = %index.id, "{label}: no chunk path to rewrite");
        return Ok(report);
    }

    let rewrites = plan.rewrites;
    let outcome = tokio::task::spawn_blocking(move || corpus.apply_path_rewrites(&rewrites))
        .await
        .with_context(|| format!("{label}: rewrite task panicked"))?
        .with_context(|| {
            format!(
                "{label}: path rewrite for '{}' failed; nothing was committed",
                index.id
            )
        })?;
    report.rewritten = outcome.rewritten;
    report.kept_on_collision = outcome.kept_on_collision;
    report.skipped_changed = outcome.skipped_changed;

    if !report.kept_on_collision.is_empty() {
        tracing::warn!(
            index_id = %index.id,
            count = report.kept_on_collision.len(),
            reason = COLLISION_REASON,
            sample = ?report.kept_on_collision.iter().take(5).collect::<Vec<_>>(),
            "{label}: kept chunk(s) under their absolute id; a reindex removes the \
             stale duplicates (#7923)"
        );
    }
    if report.skipped_changed > 0 {
        // #7923: a skipped row stays under its absolute id — a degradation to
        // report, never a silent overwrite.
        tracing::warn!(
            index_id = %index.id,
            count = report.skipped_changed,
            "{label}: skipped chunk(s) changed or removed since the load; they keep \
             their stored id and path until a reindex (#7923)"
        );
    }
    if report.rewritten > 0 {
        tracing::info!(
            index_id = %index.id,
            count = report.rewritten,
            "{label}: rewrote absolute chunk file paths to root-relative"
        );
        let indexer = index.indexer.read().await;
        if let Err(e) = indexer.refresh_live_indices_from_corpus().await {
            tracing::warn!(
                index_id = %index.id,
                "{label}: live-index refresh failed ({e}) — BM25 may be stale until restart"
            );
        }
    }
    Ok(report)
}

/// Replace the `old_file` prefix of `old_id` with `rel_file`.
///
/// Why: a chunk id embeds its file path, so the redb key must move with it.
/// What: returns `old_id` unchanged when it does not start with `old_file`.
/// Test: `test_m002_reconstruct_id_standard`, `test_m004_reconstruct_id_standard`.
pub(crate) fn reconstruct_id(old_id: &str, old_file: &str, rel_file: &str) -> String {
    match old_id.strip_prefix(old_file) {
        Some(suffix) => format!("{rel_file}{suffix}"),
        None => old_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunker::ChunkType;
    use crate::core::corpus::CorpusStore;
    use crate::core::indexer::CodeIndexer;
    use crate::core::migration::{
        M002AbsoluteToRelativePaths, M004RepairAbsoluteFilePaths, Migration,
    };
    use crate::core::registry::IndexId;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    const ROOT: &str = "/srv/apex";

    fn chunk(id: &str, file: &str, content: &str) -> RawChunk {
        RawChunk {
            id: id.to_string(),
            file: file.to_string(),
            start_line: 1,
            end_line: 3,
            content: content.to_string(),
            function_name: None,
            language: Some("rust".to_string()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        }
    }

    /// The #7923 losing shapes: a relative twin, a plain rewrite, and an id
    /// that does not embed its stored file spelling.
    fn fixture() -> Vec<RawChunk> {
        vec![
            chunk(
                "/srv/apex/src/a.rs:1:3",
                "/srv/apex/src/a.rs",
                "fn stale() {}",
            ),
            chunk("src/a.rs:1:3", "src/a.rs", "fn live() {}"),
            chunk("/srv/apex/src/b.rs:1:3", "/srv/apex/src/b.rs", "fn b() {}"),
            chunk("legacy-c", "/srv/apex/src/c.rs", "fn c() {}"),
        ]
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Id set after a successful pass over [`fixture`].
    fn expected_after() -> BTreeSet<String> {
        set(&[
            "/srv/apex/src/a.rs:1:3",
            "src/a.rs:1:3",
            "src/b.rs:1:3",
            "legacy-c",
        ])
    }

    fn handle(dir: &Path) -> (IndexHandle, Arc<CorpusStore>) {
        let corpus = Arc::new(CorpusStore::open(&dir.join("index.redb")).expect("open corpus"));
        corpus.upsert_chunks(&fixture()).expect("seed corpus");
        let mut indexer = CodeIndexer::new("relativize-7923", ROOT);
        indexer.set_corpus_store(Arc::clone(&corpus));
        let handle = IndexHandle::bare(
            IndexId::new("relativize-7923"),
            Arc::new(RwLock::new(indexer)),
            ROOT.into(),
        );
        (handle, corpus)
    }

    fn ids(corpus: &CorpusStore) -> BTreeSet<String> {
        corpus
            .load_all_chunks()
            .expect("load")
            .into_iter()
            .map(|c| c.id)
            .collect()
    }

    /// #7923: both migrations keep every row, by exact count and id set, and
    /// never overwrite the live relative row with the stale absolute one.
    #[tokio::test]
    async fn relativize_preserves_every_chunk_for_m002_and_m004() {
        let migrations: [&dyn Migration; 2] =
            [&M002AbsoluteToRelativePaths, &M004RepairAbsoluteFilePaths];
        for migration in migrations {
            let dir = tempfile::tempdir().unwrap();
            let (handle, corpus) = handle(dir.path());
            migration.apply(&handle).await.expect("migration applies");

            let label = migration.description();
            assert_eq!(corpus.chunk_count().unwrap(), 4, "{label}: row count");
            assert_eq!(ids(&corpus), expected_after(), "{label}: id set");
            let rows = corpus.get_chunks(&["src/a.rs:1:3", "legacy-c"]).unwrap();
            assert_eq!(rows[0].content, "fn live() {}", "{label}: twin overwritten");
            assert_eq!(rows[1].file, "src/c.rs", "{label}: in-place file rewrite");
        }
    }

    /// The declined arm is reported with a count and a reason, and a second
    /// pass reports the same collision without touching any row.
    #[tokio::test]
    async fn relativize_reports_collisions_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, corpus) = handle(dir.path());

        let first = relativize_corpus_paths(&handle, "test").await.unwrap();
        assert_eq!(first.rewritten, 2);
        assert_eq!(first.kept_on_collision, vec!["/srv/apex/src/a.rs:1:3"]);
        assert_eq!(first.skipped_changed, 0);
        assert_eq!(first.outside_root, 0);
        assert!(COLLISION_REASON.contains("already held"));

        let second = relativize_corpus_paths(&handle, "test").await.unwrap();
        assert_eq!(second.rewritten, 0);
        assert_eq!(second.kept_on_collision, first.kept_on_collision);
        assert_eq!(corpus.chunk_count().unwrap(), 4);
    }

    /// Error arm and crash shape: a fault after every move is staged fails the
    /// count check, commits nothing, and a retry converges on the full result.
    #[tokio::test]
    async fn failed_rewrite_commits_nothing_and_retry_converges() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, corpus) = handle(dir.path());
        let original = ids(&corpus);
        let plan = plan_rewrites(corpus.load_all_chunks().unwrap(), Path::new(ROOT));

        let err = corpus
            .apply_path_rewrites_with(&plan.rewrites, |table| {
                // A row the pass did not plan to touch goes missing mid-transaction.
                table.remove("src/a.rs:1:3").context("fault injection")?;
                Ok(())
            })
            .expect_err("a changed row count must abort the transaction");
        assert!(
            err.to_string().contains("aborted without committing"),
            "{err:#}"
        );
        assert_eq!(
            ids(&corpus),
            original,
            "nothing from the failed pass commits"
        );

        relativize_corpus_paths(&handle, "retry")
            .await
            .expect("retry converges");
        assert_eq!(ids(&corpus), expected_after());
        assert_eq!(corpus.chunk_count().unwrap(), 4);
    }

    /// Rows written or removed between the load and the transaction are
    /// neither overwritten nor reported as lost.
    #[tokio::test]
    async fn rows_written_after_the_load_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (_handle, corpus) = handle(dir.path());
        let plan = plan_rewrites(corpus.load_all_chunks().unwrap(), Path::new(ROOT));

        // Concurrent writers land after the plan was taken.
        corpus
            .upsert_chunks(&[
                chunk("src/b.rs:1:3", "src/b.rs", "fn b_live() {}"),
                chunk("src/new.rs:1:3", "src/new.rs", "fn new() {}"),
            ])
            .unwrap();
        corpus.delete_chunks(&["legacy-c".to_string()]).unwrap();

        let outcome = corpus
            .apply_path_rewrites(&plan.rewrites)
            .expect("concurrent writes must not produce a lost-rows error");
        assert_eq!(outcome.rewritten, 0);
        assert_eq!(
            outcome.kept_on_collision,
            vec!["/srv/apex/src/a.rs:1:3", "/srv/apex/src/b.rs:1:3"]
        );
        assert_eq!(outcome.skipped_changed, 1);
        assert_eq!(corpus.chunk_count().unwrap(), 5);
        let b = corpus.get_chunks(&["src/b.rs:1:3"]).unwrap();
        assert_eq!(b[0].content, "fn b_live() {}", "newer row overwritten");
    }

    /// A row whose content is updated under the same absolute id and `file`
    /// after the load keeps the new content, on both the moving and the
    /// in-place arm, and the skip is counted.
    #[tokio::test]
    async fn rows_updated_in_place_after_the_load_keep_their_new_content() {
        let dir = tempfile::tempdir().unwrap();
        let (_handle, corpus) = handle(dir.path());
        let plan = plan_rewrites(corpus.load_all_chunks().unwrap(), Path::new(ROOT));

        corpus
            .upsert_chunks(&[
                chunk(
                    "/srv/apex/src/b.rs:1:3",
                    "/srv/apex/src/b.rs",
                    "fn b_updated() {}",
                ),
                chunk("legacy-c", "/srv/apex/src/c.rs", "fn c_updated() {}"),
            ])
            .unwrap();

        let outcome = corpus.apply_path_rewrites(&plan.rewrites).unwrap();
        assert_eq!(outcome.skipped_changed, 2, "{outcome:?}");
        assert_eq!(outcome.rewritten, 0, "{outcome:?}");
        assert_eq!(corpus.chunk_count().unwrap(), 4);
        let rows = corpus
            .get_chunks(&["/srv/apex/src/b.rs:1:3", "legacy-c"])
            .unwrap();
        assert_eq!(rows.len(), 2, "an updated row was removed");
        assert_eq!(rows[0].content, "fn b_updated() {}", "newer content lost");
        assert_eq!(rows[1].content, "fn c_updated() {}", "newer content lost");
        assert!(corpus.get_chunks(&["src/b.rs:1:3"]).unwrap().is_empty());
    }
}
