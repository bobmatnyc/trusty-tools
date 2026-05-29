//! File-level operations on [`CodeIndexer`]: removal, lookup, and entity access.
//!
//! Why: chunk removal (single id or whole file) and entity lookups are
//! orthogonal to the search/ingest hot paths. Lifting them out keeps each
//! `impl` block focused on a single concern.
//! What: `remove_file`, `remove_chunk`, the shared `remove_chunks_from_stores`
//! helper, `find_chunk_id`, `entities_for`, and `entity_exact_match`.
//! Test: covered by `test_remove_chunk_removes_from_results`,
//! `test_entity_exact_match_*` in `indexer::tests`.

use anyhow::Result;

use crate::core::chunker::RawChunk;
use crate::core::entity::EntityType;

use super::{build_compact_snippet, raw_to_code_chunk, CodeChunk, CodeIndexer};

impl CodeIndexer {
    /// Find a chunk whose `file` ends with `file_suffix` and (optionally) whose
    /// `function_name` equals `function`. When `function` is `None`, returns
    /// the lowest-line-numbered chunk in the matching file. Returns the chunk
    /// id, or `None` when nothing matches.
    pub async fn find_chunk_id(&self, file_suffix: &str, function: Option<&str>) -> Option<String> {
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        let matching: Vec<&RawChunk> = chunks
            .values()
            .filter(|c| c.file.ends_with(file_suffix))
            .filter(|c| match function {
                Some(f) => c.function_name.as_deref() == Some(f),
                None => true,
            })
            .collect();
        // Pick the earliest chunk in the file for stability.
        matching
            .into_iter()
            .min_by_key(|c| c.start_line)
            .map(|c| c.id.clone())
    }

    /// Snapshot every chunk in the corpus as a `CodeChunk`. Used by the
    /// quality / complexity endpoints (issue #32) which need to materialize
    /// per-chunk metrics without going through the search pipeline.
    pub async fn all_chunks(&self) -> Vec<CodeChunk> {
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        let root = self.root_path.clone();
        chunks
            .values()
            .map(|raw| raw_to_code_chunk(raw, 0.0, "all", None, &root))
            .collect()
    }

    /// Snapshot every `RawChunk` in the corpus (issue #76).
    ///
    /// Why: the `get_call_chain` tool needs the full source body and doc
    /// comments of every candidate function, not the projected `CodeChunk`
    /// shape returned by [`Self::all_chunks`]. Returning `RawChunk` clones
    /// keeps the read lock window tiny and lets the caller process chunks
    /// without holding any indexer lock.
    /// What: clones every `RawChunk` while briefly holding the read lock.
    /// Test: covered by `service::call_chain::tests`.
    pub async fn raw_chunks_snapshot(&self) -> Vec<RawChunk> {
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        chunks.values().cloned().collect()
    }

    /// Paginated snapshot of chunks in a stable order (file path, then
    /// `start_line`). Used by `GET /indexes/:id/chunks?offset=&limit=` and the
    /// `list_chunks` MCP tool for batch iteration over the corpus.
    ///
    /// Why: clients (sidecar analyzers, external tooling) need to page through
    /// every chunk without loading the entire corpus into memory at once.
    /// Deterministic ordering is required so successive pages don't overlap or
    /// skip rows when the underlying `HashMap` re-shuffles between calls.
    /// What: collects every `RawChunk`, sorts by `(file, start_line, end_line)`
    /// for a total order, slices `[offset .. offset+limit]`, and materializes
    /// each into a `CodeChunk` (same shape as `all_chunks`). Returns
    /// `(total_chunks, page)` so the caller can serialize the `total` field
    /// without a second pass.
    /// Test: `test_enumerate_chunks_paginates_stable_order` indexes a couple of
    /// files, pages through them, and asserts no overlap and full coverage.
    pub async fn enumerate_chunks(&self, offset: usize, limit: usize) -> (usize, Vec<CodeChunk>) {
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        let total = chunks.len();
        if limit == 0 || offset >= total {
            return (total, Vec::new());
        }
        let mut ordered: Vec<&RawChunk> = chunks.values().collect();
        ordered.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.end_line.cmp(&b.end_line))
        });
        let end = (offset + limit).min(total);
        let root = self.root_path.clone();
        let page: Vec<CodeChunk> = ordered[offset..end]
            .iter()
            .map(|raw| raw_to_code_chunk(raw, 0.0, "enumerate", None, &root))
            .collect();
        (total, page)
    }

    /// Run an HNSW-only similarity search against a precomputed embedding,
    /// excluding `exclude_id` (typically the seed chunk). Returns up to
    /// `top_k` `CodeChunk`s with `match_reason = "vector"`.
    pub async fn similar_by_embedding(
        &self,
        embedding: &[f32],
        top_k: usize,
        exclude_id: Option<&str>,
    ) -> Result<Vec<CodeChunk>> {
        let want = top_k.saturating_add(1).max(top_k);
        let hits = self.vector_search(embedding, want).await?;
        self.ensure_chunks_loaded().await;
        let chunks = self.chunks.read().await;
        let mut out = Vec::with_capacity(top_k);
        for (id, score) in hits {
            if Some(id.as_str()) == exclude_id {
                continue;
            }
            let Some(raw) = chunks.get(&id) else { continue };
            let snippet = Some(build_compact_snippet(&raw.content));
            out.push(raw_to_code_chunk(
                raw,
                score,
                "vector",
                snippet,
                &self.root_path,
            ));
            if out.len() >= top_k {
                break;
            }
        }
        Ok(out)
    }

    /// Read-only access to the entity list for a file (None if never indexed).
    pub async fn entities_for(
        &self,
        file_path: &str,
    ) -> Option<Vec<crate::core::entity::RawEntity>> {
        self.entities.read().await.get(file_path).cloned()
    }

    /// Issue #20: exact-name entity lookup. Scans the in-memory entity index
    /// for an entry whose text matches `query` (case-insensitive, trimmed) and
    /// returns the chunk_id of a chunk in that entity's file whose source line
    /// range contains the entity. Returns the first match found — fine for
    /// rank-1 BM25 injection where we just need a strong anchor.
    ///
    /// Restricted to `NamedType` and `ModulePath` entities — these are the
    /// taxonomy members that behave like symbol names. Other entity types
    /// (string literals, annotations, error variants) are noisier and should
    /// not anchor an exact-match boost.
    pub(super) async fn entity_exact_match(&self, query: &str) -> Option<String> {
        let needle = query.trim();
        if needle.is_empty() || needle.contains(' ') {
            // Multi-word queries are not symbol names; skip the exact-match path.
            return None;
        }
        self.ensure_chunks_loaded().await;
        let entities = self.entities.read().await;
        let chunks = self.chunks.read().await;
        for (file, ents) in entities.iter() {
            for ent in ents {
                if !matches!(
                    ent.entity_type,
                    EntityType::NamedType | EntityType::ModulePath
                ) {
                    continue;
                }
                if ent.text.eq_ignore_ascii_case(needle) {
                    // Find a chunk in `file` whose [start_line, end_line] contains ent.line.
                    if let Some(c) = chunks
                        .values()
                        .filter(|c| c.file == *file)
                        .find(|c| ent.line >= c.start_line && ent.line <= c.end_line)
                    {
                        return Some(c.id.clone());
                    }
                }
            }
        }
        None
    }

    /// Remove every chunk belonging to a file, plus its entity list.
    ///
    /// Why: `index-file` re-indexes a file in place, but file deletion (and
    /// `FileWatcher` rename/remove events) needs to drop all of a file's
    /// chunks at once. Returns the number of chunks removed.
    pub async fn remove_file(&self, file_path: &str) -> Result<usize> {
        // Rehydrate so an idle-evicted map still yields the file's chunk ids to
        // remove (the redb delete below is keyed by those ids).
        self.ensure_chunks_loaded().await;
        let ids: Vec<String> = {
            let chunks = self.chunks.read().await;
            chunks
                .values()
                .filter(|c| c.file == file_path)
                .map(|c| c.id.clone())
                .collect()
        };
        let removed = ids.len();
        self.remove_chunks_from_stores(&ids).await;
        self.entities.write().await.remove(file_path);
        // Issue #28: evict the file's entity list from the durable redb store
        // too, or a restart would resurrect it into the symbol graph.
        self.delete_entities_from_redb(file_path).await;
        self.rebuild_symbol_graph().await;
        Ok(removed)
    }

    /// Delete a file's entity list from the durable redb corpus (issue #28).
    ///
    /// Why: `remove_file` drops the in-memory entity list; the redb store must
    /// follow or a restart would rebuild a stale symbol graph. No-op when no
    /// `CorpusStore` is wired (test / BM25-only indexers).
    /// What: runs `CorpusStore::delete_entities` on a blocking worker (redb's
    /// API is sync). Errors are logged at `warn`, never propagated —
    /// persistence cleanup must not fail a live in-memory removal.
    /// Test: covered by `tests::test_corpus_store_roundtrip` deletion paths.
    async fn delete_entities_from_redb(&self, file_path: &str) {
        let Some(corpus) = self.corpus.clone() else {
            return;
        };
        let file = file_path.to_string();
        let index_id = self.index_id.clone();
        match tokio::task::spawn_blocking(move || corpus.delete_entities(&file)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("index '{index_id}': redb entity delete failed ({e})")
            }
            Err(e) => {
                tracing::warn!("index '{index_id}': redb entity delete task panicked ({e})")
            }
        }
    }

    /// Delete a set of chunk ids from the durable redb corpus (issue #28).
    ///
    /// Why: `remove_chunk` / `remove_file` evict chunks from every in-memory
    /// structure; the redb store must follow or a restart resurrects them.
    /// What: runs `CorpusStore::delete_chunks` on a blocking worker. Errors are
    /// logged, never propagated.
    /// Test: covered by `tests::test_corpus_store_roundtrip` deletion paths.
    async fn delete_chunks_from_redb(&self, ids: &[String]) {
        let Some(corpus) = self.corpus.clone() else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        let ids = ids.to_vec();
        let index_id = self.index_id.clone();
        match tokio::task::spawn_blocking(move || corpus.delete_chunks(&ids)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("index '{index_id}': redb chunk delete failed ({e})")
            }
            Err(e) => {
                tracing::warn!("index '{index_id}': redb chunk delete task panicked ({e})")
            }
        }
    }

    /// Remove every chunk id from the HNSW store, corpus, embedding cache,
    /// and BM25 index.
    ///
    /// Why: shared between `remove_file` (bulk per-file deletion) and could
    /// be reused for future bulk-deletion paths. Each lock is acquired once
    /// for the whole batch to bound write-lock contention.
    /// What: best-effort `store.remove` per id (swallows store errors —
    /// HNSW deletion is non-fatal in this codebase), then drops the id from
    /// each in-memory structure under a single write lock per structure.
    /// Test: covered indirectly by `test_remove_chunk_removes_from_results`.
    async fn remove_chunks_from_stores(&self, ids: &[String]) {
        if let Some(store) = &self.store {
            for id in ids {
                store.remove(id).await.ok();
            }
        }
        {
            let mut chunks = self.chunks.write().await;
            for id in ids {
                chunks.remove(id);
            }
        }
        {
            let mut emb = self.chunk_embeddings.write().await;
            for id in ids {
                emb.pop(id);
            }
        }
        {
            let mut bm25 = self.bm25.write().await;
            for id in ids {
                bm25.remove_document(id);
            }
        }
        // Issue #28: mirror the deletion into the durable redb corpus.
        self.delete_chunks_from_redb(ids).await;
    }

    /// Remove a chunk from the corpus and its vector from the HNSW store.
    pub async fn remove_chunk(&self, chunk_id: &str) -> Result<()> {
        if let Some(store) = &self.store {
            store.remove(chunk_id).await.ok();
        }
        self.chunks.write().await.remove(chunk_id);
        self.chunk_embeddings.write().await.pop(chunk_id);
        self.bm25.write().await.remove_document(chunk_id);
        // Issue #28: mirror the deletion into the durable redb corpus.
        self.delete_chunks_from_redb(&[chunk_id.to_string()]).await;
        self.rebuild_symbol_graph().await;
        Ok(())
    }
}
