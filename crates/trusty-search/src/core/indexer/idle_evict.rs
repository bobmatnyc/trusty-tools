//! Idle eviction + lazy rehydration for the BM25 corpus and per-file
//! entities (issue #2162).
//!
//! Why: heap-profiling a production daemon (77 indexes, 3.9 GB on-disk vs.
//! ~13 GB resident) found two structures that are permanently resident once
//! loaded, independent of query activity: `BM25Index::doc_terms` (a full
//! second, tokenized copy of every chunk's text — see
//! `crates/trusty-common/src/bm25.rs`) and the per-file entity map. Both are
//! rebuilt in-heap on every warm-boot (`persist::load_chunks_from_redb`) and
//! neither has an eviction path today — only the chunk-text map does
//! (`CodeIndexer::evict_chunks_if_idle` in `mod.rs`). Since both are 100%
//! recoverable from the durable redb corpus, they can follow the exact same
//! idle-evict / lazy-rehydrate shape, just for a different pair of fields.
//! What: [`CodeIndexer::evict_bm25_entities_if_idle`] clears `self.bm25` and
//! `self.entities` once the index has been idle past a threshold (shares the
//! same `TRUSTY_CHUNKS_IDLE_EVICT_SECS` window and the same 60s ticker as
//! chunk eviction — see `service::server::tickers::spawn_idle_chunk_eviction_ticker`).
//! [`CodeIndexer::ensure_bm25_entities_loaded`] rehydrates both from redb the
//! next time a query lane or ingest-commit path needs them, mirroring
//! `ensure_chunks_loaded`'s guard-flag-then-rebuild shape so concurrent
//! readers never observe a half-populated structure.
//! Test: `bm25_entities_idle_eviction_drops_and_lazily_rehydrates` and
//! `bm25_entities_idle_eviction_skips_indexers_without_corpus` in
//! `indexer::tests`.
//!
//! Also home to [`CodeIndexer::demote_vector_store_if_idle`] (issue #2164),
//! which rides the exact same idle sweep to re-view (mmap-demote) an idle,
//! write-promoted HNSW vector store back to `Index::view` — see that
//! method's doc comment and `UsearchStore::try_demote_to_view` for the full
//! design.

use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;

use crate::core::bm25::Bm25Index;
use crate::core::chunker::RawChunk;
use crate::core::entity::RawEntity;

use super::CodeIndexer;

/// Restored (chunks, per-file entities) pair read from the durable corpus.
/// Named for readability at the `spawn_blocking` closure boundary, mirroring
/// `persist::RestoredCorpus`.
type RestoredCorpus = (Vec<RawChunk>, Vec<(String, Vec<RawEntity>)>);

impl CodeIndexer {
    /// Drop the in-memory BM25 corpus and per-file entity map when the index
    /// has been idle longer than `idle_threshold` and a durable corpus can
    /// repopulate them.
    ///
    /// Why: see the module docs — BM25's `doc_terms` is a full tokenized
    /// second copy of every chunk's text, permanently resident regardless of
    /// query activity once an index has been queried or ingested once.
    /// What: a no-op when `idle_threshold` is zero, no durable corpus is
    /// wired, BM25 is already empty (nothing to reclaim — also covers the
    /// "already evicted" case since eviction empties it), or the index was
    /// recently active. Otherwise replaces `self.bm25` with a fresh empty
    /// index and clears `self.entities`, marks `bm25_entities_evicted`, and
    /// logs an `info` with the reclaimed BM25 document count. Chunks and the
    /// symbol graph are untouched by this method — they have their own
    /// (chunks) or no (symbol graph) eviction path.
    /// Returns the number of BM25 documents evicted (0 when skipped).
    /// Test: `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`.
    pub async fn evict_bm25_entities_if_idle(&self, idle_threshold: Duration) -> usize {
        if idle_threshold.is_zero() {
            return 0;
        }
        if self.idle_duration() < idle_threshold {
            return 0;
        }
        let evicted = self.clear_bm25_entities().await;
        if evicted > 0 {
            tracing::info!(
                "index '{}': evicted {} in-memory BM25 documents + entities after {}s idle \
                 (durable corpus retained; lazily rehydrates on next access)",
                self.index_id,
                evicted,
                idle_threshold.as_secs(),
            );
        }
        evicted
    }

    /// Unconditionally drop the in-memory BM25 corpus + per-file entity map (no
    /// idle guard, no logging). Shared by the idle-evict path
    /// ([`Self::evict_bm25_entities_if_idle`]) and the memory-pressure reclaim
    /// path ([`Self::reclaim_memory_now`], issue #2846).
    ///
    /// Why: `BM25Index::doc_terms` is a full tokenized second copy of every
    /// chunk's text and, with the entity map, one of the largest anonymous-heap
    /// consumers a warm-booted index carries. Both callers need the identical
    /// clear-and-mark; factoring it here keeps the two paths from drifting.
    /// What: a no-op returning 0 when no durable corpus is wired or BM25 is
    /// already empty. Otherwise replaces `self.bm25` with a fresh empty index,
    /// clears `self.entities`, marks `bm25_entities_evicted`, and returns the
    /// reclaimed BM25 document count. Callers own any logging.
    /// Test: exercised by `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`
    /// (idle path) and `memory_pressure_reclaim_now_clears_caches` (pressure path).
    async fn clear_bm25_entities(&self) -> usize {
        if self.corpus.is_none() {
            return 0;
        }
        let mut bm25 = self.bm25.write().await;
        if bm25.is_empty() {
            return 0;
        }
        let evicted = bm25.len();
        *bm25 = Bm25Index::new();
        drop(bm25);

        let mut entities = self.entities.write().await;
        entities.clear();
        entities.shrink_to_fit();
        drop(entities);

        self.bm25_entities_evicted.store(true, Ordering::Relaxed);
        evicted
    }

    /// Force-reclaim this index's evictable in-memory heap **regardless of idle
    /// state** — the steady-state memory-limit enforcement path (issue #2846).
    ///
    /// Why: the idle-evict ticker only reclaims indexes that have been quiet
    /// past the idle window, so a daemon under genuine memory pressure whose
    /// indexes are all "recently active" keeps growing until the OS OOM-killer
    /// intervenes — exactly the production failure #2846 reports (RSS reached
    /// 2.2× the configured 12 GB soft ceiling). When the pressure ticker sees
    /// RSS cross the high-water mark it calls this on every resident index to
    /// shed the largest anonymous-heap consumers immediately. Every structure
    /// cleared here is 100% recoverable from the durable redb corpus and lazily
    /// rehydrates on next access, so a pressure reclaim is non-destructive to
    /// the DATA. It is, however, a genuine race against any concurrent query:
    /// firing under active load (unlike idle-evict, which only ever raced this
    /// window after 60s of quiet) can land strictly between a racing
    /// `bm25_search` / `grep_fallback_search` call's own `ensure_*_loaded()`
    /// check and its read-lock acquisition. Both of those lanes now detect and
    /// retry that exact window (issue #2846 PR review — see their doc
    /// comments in `search/lanes.rs`), so the worst case for a racing query is
    /// a re-load latency spike (redb rehydrate before the retry's read),
    /// not a silently-lost lexical/grep lane — strictly preferable to an
    /// OOM-kill either way.
    /// What: unconditionally clears the in-memory chunk map, the BM25 corpus,
    /// and the per-file entity map, then demotes the HNSW vector store back to
    /// its mmap view (best-effort — a demote failure is logged at debug and
    /// never fatal). Returns the total in-memory entry count reclaimed (the sum
    /// of cleared chunks and BM25 documents). An index without a durable corpus
    /// (BM25-only / test) reclaims nothing (returns 0) — it has no source to
    /// rehydrate from, so clearing it would be data loss, not cache eviction.
    /// Test: `memory_pressure_reclaim_now_clears_caches` and
    /// `memory_pressure_reclaim_now_is_noop_without_corpus` in `indexer::tests`.
    pub async fn reclaim_memory_now(&self) -> usize {
        let mut reclaimed = self.clear_in_memory_chunks().await;
        reclaimed += self.clear_bm25_entities().await;
        if let Some(store) = &self.store {
            // Deliberately bypasses `hnsw_review_idle_enabled()` (unlike
            // `demote_vector_store_if_idle`): under active memory pressure,
            // reclaiming heap takes priority over an operator's idle-demotion
            // opt-out — `demote_to_view` itself still refuses to demote a
            // store with unpersisted writes, so this stays safe either way.
            if let Err(e) = store.demote_to_view().await {
                tracing::debug!(
                    "index '{}': memory-pressure HNSW demote-to-view failed ({e}); \
                     leaving heap-resident",
                    self.index_id
                );
            }
        }
        reclaimed
    }

    /// Repopulate the BM25 corpus and per-file entity map from the durable
    /// corpus if they were previously evicted while idle.
    ///
    /// Why: query lanes (`bm25_search`, `entity_exact_match`, `entities_for`)
    /// and ingest commit paths (`commit_parsed_batch`, `add_chunk_inner`)
    /// read or mutate `self.bm25` / `self.entities` directly; after an idle
    /// eviction both are empty and `bm25_entities_evicted` is set. Every call
    /// site guards itself the same way `ensure_chunks_loaded` call sites do,
    /// rather than relying on a single upstream gate.
    /// What: a fast no-op (single relaxed atomic load) when never evicted.
    /// When evicted, reloads every chunk + entity row from `CorpusStore` on a
    /// blocking worker, rebuilds BM25 via `Self::bm25_doc_text` (identical
    /// rebuild logic to `persist::load_chunks_from_redb`), refills entities,
    /// then clears the flag. A `None` corpus (shouldn't happen — eviction
    /// requires one — but defensive) just clears the flag.
    /// Test: `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`.
    pub(super) async fn ensure_bm25_entities_loaded(&self) {
        if !self.bm25_entities_evicted.load(Ordering::Relaxed) {
            return;
        }
        let Some(corpus) = self.corpus.clone() else {
            self.bm25_entities_evicted.store(false, Ordering::Relaxed);
            return;
        };
        let index_id = self.index_id.clone();
        let loaded = tokio::task::spawn_blocking(move || -> Result<RestoredCorpus> {
            let chunks = corpus.load_all_chunks()?;
            let entities = corpus.load_all_entities()?;
            Ok((chunks, entities))
        })
        .await;
        match loaded {
            Ok(Ok((chunks, entities))) => {
                let n_docs = chunks.len();
                {
                    let mut bm25 = self.bm25.write().await;
                    for chunk in &chunks {
                        let text = Self::bm25_doc_text(chunk);
                        bm25.upsert_document(&chunk.id, &text);
                    }
                }
                let n_files = entities.len();
                {
                    let mut emap = self.entities.write().await;
                    for (file, ents) in entities {
                        emap.insert(file, ents);
                    }
                }
                self.bm25_entities_evicted.store(false, Ordering::Relaxed);
                tracing::info!(
                    "index '{index_id}': rehydrated {n_docs} BM25 documents + \
                     {n_files} file entity lists from redb after idle eviction"
                );
            }
            Ok(Err(e)) => tracing::warn!(
                "index '{index_id}': failed to rehydrate BM25/entities from redb ({e}); \
                 will retry on next access"
            ),
            Err(e) => tracing::warn!(
                "index '{index_id}': BM25/entities rehydration task panicked ({e}); \
                 will retry on next access"
            ),
        }
    }

    /// Demote this index's HNSW vector store back to mmap-view mode when it
    /// has been idle longer than `idle_threshold` (issue #2164).
    ///
    /// Why: the #709 mmap-view optimization keeps a warm-booted HNSW index
    /// pageable, but any write promotes it to a full heap copy
    /// (`UsearchStore::ensure_mutable`) with no path back — so in practice
    /// almost every index a daemon ever writes to stays heap-resident
    /// forever, even long after it goes idle. This reclaims that heap the
    /// same way `evict_chunks_if_idle` / `evict_bm25_entities_if_idle`
    /// reclaim theirs, riding the same idle window and the same ticker tick
    /// so there is exactly one idle sweep, not a competing second one.
    /// What: a no-op when `idle_threshold` is zero, the
    /// [`crate::core::store_config::hnsw_review_idle_enabled`] env gate is
    /// off, no vector store is wired (BM25-only mode), or the index was
    /// recently active. Otherwise delegates to
    /// [`crate::core::store::VectorStore::demote_to_view`], which is the
    /// authoritative gate on safety (only demotes a store that is currently
    /// mutable AND has no unpersisted writes — see
    /// `UsearchStore::try_demote_to_view`). Logs an `info` on an actual
    /// demotion, a `warn` on failure (never fatal — demotion is an
    /// optimization). Returns `true` when a demotion happened.
    /// Test: `hnsw_idle_demotion_reviews_clean_promoted_store` and
    /// `hnsw_idle_demotion_skips_when_disabled_via_env` in `indexer::tests`.
    pub async fn demote_vector_store_if_idle(&self, idle_threshold: Duration) -> bool {
        if idle_threshold.is_zero() {
            return false;
        }
        if !crate::core::store_config::hnsw_review_idle_enabled() {
            return false;
        }
        let Some(store) = &self.store else {
            return false;
        };
        if self.idle_duration() < idle_threshold {
            return false;
        }
        match store.demote_to_view().await {
            Ok(true) => {
                tracing::info!(
                    "index '{}': demoted HNSW vector store to mmap-view after {}s idle",
                    self.index_id,
                    idle_threshold.as_secs(),
                );
                true
            }
            Ok(false) => false,
            Err(e) => {
                tracing::warn!(
                    "index '{}': HNSW demote-to-view failed ({e}); leaving heap-resident",
                    self.index_id
                );
                false
            }
        }
    }
}
