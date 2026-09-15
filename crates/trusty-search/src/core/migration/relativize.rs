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
//! What: [`relativize_corpus_paths`] plans every rewrite against the full id
//! set first. A rewrite onto an id another row holds is not applied: the row
//! keeps its absolute id and is reported with a count and a reason. A rewrite
//! that leaves the id unchanged updates `file` in place and deletes nothing.
//! The corpus row count is verified unchanged before the live indices are
//! refreshed; a mismatch is an error, so the schema version is not stamped.
//! Test: `relativize::tests`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use crate::core::chunker::RawChunk;
use crate::core::registry::IndexHandle;

/// Outcome of one relativization pass.
///
/// Why: a chunk the pass declines to rewrite must be visible as a count and a
/// reason, not only as a log line, so callers and tests can assert it.
/// What: `rewritten` rows moved to root-relative form; `kept_on_collision`
/// holds the absolute ids left in place because their root-relative id was
/// already taken; `outside_root` counts absolute files not under the root.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RelativizeReport {
    pub rewritten: usize,
    pub kept_on_collision: Vec<String>,
    pub outside_root: usize,
}

/// Why the rewrite declines a colliding row; logged and documented verbatim.
pub(crate) const COLLISION_REASON: &str =
    "its root-relative id is already held by another row, so rewriting it would \
     overwrite that row";

#[derive(Default)]
struct RewritePlan {
    to_upsert: Vec<RawChunk>,
    ids_to_delete: Vec<String>,
    kept_on_collision: Vec<String>,
    outside_root: Vec<String>,
}

/// Decide, for every chunk, whether and how it is rewritten.
fn plan_rewrites(chunks: Vec<RawChunk>, root: &Path) -> RewritePlan {
    let existing: HashSet<String> = chunks.iter().map(|c| c.id.clone()).collect();
    let mut claimed: HashSet<String> = HashSet::new();
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
        let new_id = reconstruct_id(&chunk.id, &chunk.file, &rel_str);
        if new_id == chunk.id {
            // #7923: the id does not embed this file spelling. Deleting the old
            // id here would delete the row just upserted under the same key.
            chunk.file = rel_str;
            plan.to_upsert.push(chunk);
            continue;
        }
        if existing.contains(&new_id) || !claimed.insert(new_id.clone()) {
            // #7923: upserting onto a held id replaced that row and then the
            // delete below removed this one — a silent loss per collision.
            plan.kept_on_collision.push(chunk.id);
            continue;
        }
        plan.ids_to_delete
            .push(std::mem::replace(&mut chunk.id, new_id));
        chunk.file = rel_str;
        plan.to_upsert.push(chunk);
    }
    plan
}

/// Rewrite absolute chunk `file`/`id` pairs under `index.root_path` to
/// root-relative form without losing a row.
///
/// Why: see the module doc; M002 (#402) and M004 (#674) both call this.
/// What: loads the corpus, plans rewrites ([`plan_rewrites`]), logs collisions
/// and out-of-root files with counts, upserts then deletes, verifies the row
/// count is unchanged, and refreshes the live BM25 + chunk map. No corpus is a
/// no-op. `label` prefixes every log line.
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
    let before = all_chunks.len();

    let plan = plan_rewrites(all_chunks, &root_path);
    let report = RelativizeReport {
        rewritten: plan.to_upsert.len(),
        kept_on_collision: plan.kept_on_collision.clone(),
        outside_root: plan.outside_root.len(),
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
    if !plan.kept_on_collision.is_empty() {
        tracing::warn!(
            index_id = %index.id,
            count = plan.kept_on_collision.len(),
            reason = COLLISION_REASON,
            sample = ?plan.kept_on_collision.iter().take(5).collect::<Vec<_>>(),
            "{label}: kept chunk(s) under their absolute id; a reindex removes the \
             stale duplicates (#7923)"
        );
    }
    if plan.to_upsert.is_empty() {
        tracing::info!(index_id = %index.id, "{label}: no chunk path to rewrite");
        return Ok(report);
    }
    tracing::info!(
        index_id = %index.id,
        count = plan.to_upsert.len(),
        "{label}: rewriting absolute chunk file paths to root-relative"
    );

    // Upsert first: a crash between the two steps leaves both rows, never none.
    let RewritePlan {
        to_upsert,
        ids_to_delete,
        ..
    } = plan;
    let after = tokio::task::spawn_blocking(move || -> Result<usize> {
        corpus
            .upsert_chunks(&to_upsert)
            .context("upsert rewritten chunks")?;
        corpus
            .delete_chunks(&ids_to_delete)
            .context("delete old absolute-keyed rows")?;
        corpus.chunk_count()
    })
    .await
    .with_context(|| format!("{label}: rewrite task panicked"))??;
    // #7923: every planned move is one upsert plus one delete, so the count
    // cannot change. A difference means rows were lost; do not stamp.
    anyhow::ensure!(
        after == before,
        "{label}: corpus for '{}' holds {after} chunks after the path rewrite, \
         expected {before} (#7923)",
        index.id
    );

    let indexer = index.indexer.read().await;
    if let Err(e) = indexer.refresh_live_indices_from_corpus().await {
        tracing::warn!(
            index_id = %index.id,
            "{label}: live-index refresh failed ({e}) — BM25 may be stale until restart"
        );
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

            let expected: BTreeSet<String> = [
                "/srv/apex/src/a.rs:1:3",
                "src/a.rs:1:3",
                "src/b.rs:1:3",
                "legacy-c",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            let label = migration.description();
            assert_eq!(corpus.chunk_count().unwrap(), 4, "{label}: row count");
            assert_eq!(ids(&corpus), expected, "{label}: id set");
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
        assert_eq!(first.outside_root, 0);
        assert!(COLLISION_REASON.contains("already held"));

        let second = relativize_corpus_paths(&handle, "test").await.unwrap();
        assert_eq!(second.rewritten, 0);
        assert_eq!(second.kept_on_collision, first.kept_on_collision);
        assert_eq!(corpus.chunk_count().unwrap(), 4);
    }
}
