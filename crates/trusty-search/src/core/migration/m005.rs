//! M005 — clear the corpus and re-chunk it so every named chunk id carries its
//! end line (issue #6581).
//!
//! Why: before #6581 a named chunk id was `{file}::{type}::{name}::{start}` and
//! omitted the end line, so two declarations sharing a name and a start line
//! shared an id. A minified bundle is one physical line, so
//! `app/hiring/static/assets/index.js` produced 225 distinct ids for its 2,299
//! declarations and #6571's dedupe dropped the other 2,070 — they were simply
//! absent from search. Carrying the end line changes the primary key of every
//! named chunk in redb, the BM25 document map, the HNSW key sidecar and the KG
//! tables, so an existing index cannot just start emitting the new shape.
//!
//! Why a re-chunk and not a key rewrite: the chunks that collided are not in the
//! store to rewrite. The chunker dropped them before any store was touched, so
//! recovering them means re-parsing the file — which is what the owner ruling of
//! 2026-09-05 settles: "corpus clear + full re-chunk".
//!
//! Why it costs nothing to embed: the ruling's re-embed budget is zero, and the
//! text is unchanged, so a re-chunked chunk's vector already exists — under the
//! OLD id. usearch keys its vectors by `u64` labels that never encode a chunk
//! id, so handing a stored vector to its new id is a rewrite of the JSON sidecar
//! alone ([`CodeIndexer::remap_vector_store_keys`]). Nothing is embedded; only
//! text this corpus has never held would need to be, and that is left to the
//! ordinary embed catch-up rather than paid for here.
//!
//! Why it is crash-recoverable: the clear destroys the very evidence a
//! contents-based guard would read, so an interrupted pass used to look
//! identical to a finished one and `run_migrations` stamped `schema_version = 5`
//! over a partial or empty corpus. A durable [`plan::M005Plan`] written BEFORE
//! the clear and removed only after Step 7 is the evidence instead: present
//! means "a pass started and did not finish", whatever the corpus now holds.
//! Any failure leaves the marker, returns `Err`, and pins the version at 4.
//!
//! What: `apply` loads the corpus, resumes an outstanding plan or (absent one)
//! returns early unless the corpus still holds a pre-#6581 named id, records
//! `text hash → current id` durably, clears every
//! chunk-keyed structure (leaving the vector store alone), re-chunks every file
//! it knew about through the ordinary commit path — so `TRUSTY_MAX_CHUNKS`
//! applies exactly as it does on a fresh index — then re-points the sidecar from
//! old ids to the new ids that carry the same text and drops the entries nothing
//! claimed.
//!
//! Test: `m005::tests`.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::core::chunk_id;
use crate::core::chunker::chunk_ast;
use crate::core::indexer::ParsedBatch;
use crate::core::registry::IndexHandle;

use super::Migration;

mod plan;

#[cfg(test)]
mod tests;

/// Files re-chunked per commit, bounding per-batch memory. Mirrors M001.
const BATCH_SIZE: usize = 64;

/// Migration M005: re-chunk the corpus onto end-line-bearing named chunk ids
/// (issue #6581).
///
/// Why: see module-level doc.
/// What: a unit struct; all the work is in [`Migration::apply`].
/// Test: `m005_advances_exactly_one_version`, `m005_rechunks_the_whole_corpus`,
/// `m005_reuses_the_existing_vectors`.
pub struct M005ChunkIdEndLine;

#[async_trait]
impl Migration for M005ChunkIdEndLine {
    /// Why: M005 starts at schema_version 4 (after M004 has run).
    fn source_version(&self) -> u32 {
        4
    }

    /// Why: M005 advances the index to schema_version 5 — the marker the path
    /// filter reads to stop accepting the pre-#6581 named shape for this index.
    fn target_version(&self) -> u32 {
        5
    }

    fn description(&self) -> &'static str {
        "M005: clear + re-chunk so named chunk ids carry the end line (issue #6581)"
    }

    /// Apply M005 to `index`.
    ///
    /// Why: see module-level doc.
    /// What: the seven steps below. Idempotent AND crash-recoverable: step 2
    /// reads the durable plan rather than the corpus, so an interrupted pass is
    /// resumed instead of mistaken for a finished one, and the sidecar remap in
    /// step 4 rewrites nothing when every id already maps to itself. Step 7
    /// retires the marker and is the only thing that records the pass as done.
    /// Test: `m005_is_a_no_op_on_an_already_migrated_corpus`,
    /// `m005_rechunks_the_whole_corpus`, `m005_reuses_the_existing_vectors`,
    /// `m005_resumes_after_a_crash_before_the_first_batch`,
    /// `m005_never_advances_the_schema_over_missing_chunks`.
    async fn apply(&self, index: &IndexHandle) -> Result<(), anyhow::Error> {
        let (corpus, root_path) = {
            let indexer = index.indexer.read().await;
            (indexer.corpus_store(), index.root_path.clone())
        };
        let Some(corpus) = corpus else {
            tracing::debug!(index_id = %index.id, "M005: no durable corpus, skipping");
            return Ok(());
        };

        // ── Step 1: read the corpus as it stands ──────────────────────────
        let old_chunks = tokio::task::spawn_blocking({
            let corpus = std::sync::Arc::clone(&corpus);
            move || corpus.load_all_chunks()
        })
        .await
        .context("M005: load_all_chunks task panicked")?
        .context("M005: failed to load chunks from corpus")?;

        // ── Step 2: is a pass outstanding? ────────────────────────────────
        // #6581: the corpus cannot answer this. Step 3 clears it, so after any
        // interruption no pre-#6581 id remains to find, and a contents-based
        // guard reports "already migrated" for an index whose chunks are
        // missing — `run_migrations` would then stamp schema_version = 5 over
        // the loss. The durable plan is the evidence instead, and it is written
        // before the clear so every crashable state already carries it.
        let plan = match plan::M005Plan::load(&corpus)
            .await?
            .filter(|p| !p.is_empty())
        {
            Some(plan) => {
                tracing::warn!(
                    index_id = %index.id,
                    files = plan.files.len(),
                    chunks_before = plan.old_count,
                    chunks_now = old_chunks.len(),
                    "M005: a previous pass did not finish — resuming from the durable plan \
                     rather than from the corpus, which that pass had already cleared"
                );
                drop(old_chunks);
                plan
            }
            None => {
                // No pass outstanding, so a corpus with no pre-#6581 named id
                // has nothing this migration can improve: a second run — and a
                // fresh index — does no work.
                let holds_legacy = old_chunks
                    .iter()
                    .any(|c| chunk_id::parse(&c.id).is_some_and(|p| p.is_legacy_named()));
                if !holds_legacy {
                    tracing::info!(
                        index_id = %index.id,
                        chunks = old_chunks.len(),
                        "M005: no pre-#6581 named chunk ids present, nothing to re-chunk"
                    );
                    return Ok(());
                }
                // `text hash → the id that text is stored under today`. First
                // wins: two chunks with identical text can share one vector, and
                // which of them keeps it is arbitrary because the vector is
                // identical either way.
                let mut vector_by_text: HashMap<[u8; 32], String> = HashMap::new();
                let mut files: BTreeSet<String> = BTreeSet::new();
                for chunk in &old_chunks {
                    files.insert(chunk.file.clone());
                    vector_by_text
                        .entry(text_hash(&chunk.content))
                        .or_insert_with(|| chunk.id.clone());
                }
                let plan = plan::M005Plan {
                    files,
                    vector_by_text: vector_by_text.into_iter().collect(),
                    old_ids: old_chunks.iter().map(|c| c.id.clone()).collect(),
                    old_count: old_chunks.len(),
                };
                drop(old_chunks);
                plan.store(&corpus).await?;
                plan
            }
        };

        let vector_by_text = plan.vector_by_text_map();
        let old_count = plan.old_count;

        tracing::info!(
            index_id = %index.id,
            chunks = old_count,
            files = plan.files.len(),
            "M005: clearing the corpus and re-chunking (#6581)"
        );

        // ── Step 3: clear, then re-chunk through the ordinary commit path ──
        // #6581: a delete signalled before this pass touches anything. Stopping
        // HERE rather than at the first batch boundary is what keeps the corpus
        // intact — the clear is the next thing that happens — so a delete that
        // is later refused (#6380) or abandoned leaves a whole index behind.
        if crate::service::reindex::index_cancel_flag(&index.id)
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(cancel_stop(index, 0));
        }
        let indexer_arc = std::sync::Arc::clone(&index.indexer);
        // #6581: from here until this guard drops, the corpus is empty or
        // partial. A query landing in that window must be told so rather than
        // served an empty result set that reads as "nothing matched". The guard
        // is RAII because Steps 4 and 5 propagate with `?` — a manual clear
        // would leave a failed migration refusing every later query.
        let _window = crate::core::indexer::MigrationWindow::open({
            let indexer = indexer_arc.read().await;
            indexer.migration_flag()
        });
        {
            let indexer = indexer_arc.read().await;
            indexer.set_suppress_vector_eviction(true);
        }
        let outcome = rechunk_all(
            index,
            &indexer_arc,
            &root_path,
            &plan.files,
            &vector_by_text,
            old_count,
        )
        .await;
        {
            let indexer = indexer_arc.read().await;
            indexer.set_suppress_vector_eviction(false);
        }
        let Rechunked {
            remap,
            committed,
            dropped_by_cap,
            unembedded,
        } = outcome?;

        // ── Step 4: hand every reused vector to the id that now holds its text
        let hnsw_path = resolve_hnsw_path(index)?;
        let rekeyed = {
            let indexer = indexer_arc.read().await;
            indexer
                .remap_vector_store_keys(&hnsw_path, &|id| remap.get(id).cloned())
                .await
                .context("M005: could not re-point the HNSW key sidecar")?
        };

        // ── Step 5: drop sidecar entries nothing claimed ──────────────────
        // An old id no new chunk inherited names a vector for text this corpus
        // no longer holds. Left in place it would rank into results and then
        // resolve to no corpus row — the `unresolved_corpus` drop.
        let orphans: Vec<String> = plan
            .old_ids
            .iter()
            .filter(|id| !remap.contains_key(*id))
            .cloned()
            .collect();
        if !orphans.is_empty() {
            let indexer = indexer_arc.read().await;
            indexer.remove_vectors(&orphans).await;
            indexer
                .save_vector_store(&hnsw_path)
                .await
                .context("M005: could not flush the HNSW sidecar after dropping orphans")?;
        }

        // ── Step 6: rebuild the graph the clear emptied ───────────────────
        {
            let indexer = indexer_arc.read().await;
            let _ = indexer.rebuild_symbol_graph_now().await;
        }
        index.indexer.read().await.set_chunk_ids_migrated();

        // ── Step 7: the pass finished — retire the marker ─────────────────
        // #6581: this is the ONLY thing that lets a later boot treat M005 as
        // done. It runs after every other step has succeeded, so any earlier
        // failure leaves the marker in place, `apply` returns Err, and
        // `run_migrations` never advances schema_version past 4.
        plan::M005Plan::clear(&corpus).await?;

        if dropped_by_cap > 0 {
            // Reported, not fatal — returning `Err` leaves schema_version at 4
            // and re-runs the whole pass at every boot, dropping the same tail.
            tracing::warn!(
                index_id = %index.id,
                dropped = dropped_by_cap,
                cap = crate::core::indexer::max_chunks_per_index(),
                "M005: the chunk cap discarded {dropped_by_cap} re-chunked chunks — this index \
                 is migrated but NOT fully re-chunked. Raise TRUSTY_MAX_CHUNKS and reindex."
            );
        }
        tracing::info!(
            index_id = %index.id,
            before = old_count,
            after = committed,
            rekeyed,
            orphans = orphans.len(),
            unembedded,
            "M005: re-chunk complete (#6581) — no text was re-embedded"
        );
        Ok(())
    }
}

/// M005 stopped at a checkpoint because this index is being deleted (#6581).
///
/// Why: `unregister_index` signals the #3049 cancel flag and then waits
/// `DELETE_QUIESCE_TIMEOUT` for the teardown lock's EXCLUSIVE side —
/// `run_migrations_exclusive` holds the SHARED side for the pass's whole run.
/// With no checkpoint, M005 re-chunks the entire corpus before releasing, the
/// wait expires, and a `delete_data=true` DELETE takes the #3049 abandon branch:
/// the operator is told to re-issue a delete that will abandon again for as long
/// as the migration lasts. `reindex::runner`'s consumer loop polls the same flag
/// at its own batch boundaries for exactly this reason; this is that checkpoint.
/// What: a distinct type, so a caller can tell an abort from a failure. Raised
/// BEFORE Step 7, so the durable plan marker survives and the next boot resumes
/// the pass; `run_migrations` applies `?` to `apply` before it calls
/// `write_schema_version`, so the schema stays at 4 either way.
/// Test: `m005_stops_before_the_clear_when_a_delete_signals_cancel`,
/// `rechunk_all_stops_at_a_batch_boundary_when_a_delete_signals_cancel`.
#[derive(Debug, thiserror::Error)]
#[error(
    "index '{index_id}': M005 stopped at a checkpoint because this index is being deleted \
     (#3049 cancel). Nothing was recorded as migrated: the durable plan marker is kept and \
     the schema stays at 4, so the pass resumes from it if the index survives (#6581)."
)]
pub struct M005Cancelled {
    /// The index whose delete signalled the cancel.
    pub index_id: String,
}

/// Log the stop and build the [`M005Cancelled`] error for `index`.
///
/// Why/What/Test: see [`M005Cancelled`]. Factored out because `rechunk_all`
/// checks the flag at two points and both owe the same operator log.
fn cancel_stop(index: &IndexHandle, committed: usize) -> anyhow::Error {
    tracing::warn!(
        index_id = %index.id,
        committed,
        "M005: this index is being deleted — stopping at a checkpoint so the delete's \
         quiesce wait succeeds instead of expiring (#6581)"
    );
    anyhow::Error::new(M005Cancelled {
        index_id: index.id.to_string(),
    })
}

/// What one re-chunk pass produced.
///
/// Why: `apply` needs four numbers back from the pass and the pass has to run
/// between setting and clearing the eviction guard, so they travel as one value
/// rather than four out-parameters.
/// What: `remap` is `old id → new id` for every chunk whose text is unchanged;
/// the counts are for the operator log.
/// Test: covered through `apply` by `m005_rechunks_the_whole_corpus`.
#[derive(Debug)]
struct Rechunked {
    remap: HashMap<String, String>,
    committed: usize,
    dropped_by_cap: usize,
    unembedded: usize,
}

/// Clear the corpus and re-chunk `files` from disk, committing in batches.
///
/// Why: split out of `apply` so the eviction guard is set and cleared around one
/// call, and so the file-size cap is not spent on a single 200-line method.
/// What: clears every chunk-keyed structure (leaving the vector store alone),
/// then for each batch reads the files, chunks them, and commits with NO
/// embeddings — the ruling's zero re-embed budget. A file that cannot be read is
/// skipped with a warn: it was deleted since the last index, and its chunks are
/// correctly gone.
///
/// Polls the #3049 cancel flag at every batch boundary and returns
/// [`M005Cancelled`] when it is set — see that type for why a pass with no
/// checkpoint makes a `delete_data=true` DELETE unservable for its duration.
/// `apply` takes the same check once more before it calls in, where stopping
/// still costs the corpus nothing.
/// Test: `m005_rechunks_the_whole_corpus`, `m005_honours_the_chunk_cap`,
/// `rechunk_all_stops_at_a_batch_boundary_when_a_delete_signals_cancel`.
async fn rechunk_all(
    index: &IndexHandle,
    indexer_arc: &std::sync::Arc<tokio::sync::RwLock<crate::core::indexer::CodeIndexer>>,
    root_path: &std::path::Path,
    files: &BTreeSet<String>,
    vector_by_text: &HashMap<[u8; 32], String>,
    old_count: usize,
) -> Result<Rechunked> {
    // #6581: fetched once, and AFTER `run_migrations_exclusive` took the index
    // permit — `index_cancel_flag`'s contract, so a flag left over from a delete
    // that has already finished is gone by now and this one is correctly false.
    let cancel = crate::service::reindex::index_cancel_flag(&index.id);
    {
        let indexer = indexer_arc.read().await;
        let cleared = indexer
            .clear_corpus_for_rechunk()
            .await
            .context("M005: could not clear the corpus")?;
        tracing::debug!(index_id = %index.id, cleared, expected = old_count, "M005: corpus cleared");
    }

    let ordered: Vec<&String> = files.iter().collect();
    let mut remap: HashMap<String, String> = HashMap::new();
    let mut committed = 0usize;
    let mut dropped_by_cap = 0usize;
    let mut unembedded = 0usize;

    for batch in ordered.chunks(BATCH_SIZE) {
        // #6581: the same checkpoint `reindex::runner`'s consumer loop takes.
        // Without it a large corpus holds the teardown lock's shared side for the
        // whole pass, and the delete waiting on the exclusive side abandons.
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(cancel_stop(index, committed));
        }
        let mut chunks = Vec::new();
        let mut entities_by_file = Vec::new();
        for file in batch {
            let abs = root_path.join(file.as_str());
            let content = match tokio::fs::read_to_string(&abs).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        index_id = %index.id,
                        path = %abs.display(),
                        "M005: cannot read file, its chunks stay dropped ({e})"
                    );
                    continue;
                }
            };
            let (file_chunks, entities) = chunk_ast(file.as_str(), &content);
            entities_by_file.push((file.to_string(), entities));
            chunks.extend(file_chunks);
        }
        if chunks.is_empty() {
            let indexer = indexer_arc.read().await;
            indexer.commit_entities(entities_by_file).await;
            continue;
        }
        for chunk in &chunks {
            match vector_by_text.get(&text_hash(&chunk.content)) {
                Some(old_id) => {
                    remap.insert(old_id.clone(), chunk.id.clone());
                }
                // Text this corpus has never held — the recovered chunks #6571
                // dropped. Nothing to reuse, and the ruling's budget forbids
                // embedding it here; the ordinary catch-up pass covers it.
                None => unembedded += 1,
            }
        }
        let embeddings = vec![None; chunks.len()];
        let parsed = ParsedBatch {
            chunks,
            embeddings,
            entities_by_file,
            parse_ms: 0,
            embed_ms: 0,
            vector_count: 0,
        };
        let indexer = indexer_arc.read().await;
        let timings = indexer
            .commit_parsed_batch(parsed, /* defer_graph_rebuild */ true)
            .await
            .context("M005: commit_parsed_batch failed")?;
        committed += timings.chunks;
        dropped_by_cap += timings.chunks_dropped_by_cap;
    }

    Ok(Rechunked {
        remap,
        committed,
        dropped_by_cap,
        unembedded,
    })
}

/// SHA-256 of a chunk's text — the identity M005 reuses a vector by.
///
/// Why: the ruling reuses an embedding when the text hash is unchanged. Content
/// equality is the only thing that makes an existing vector still correct, and
/// the id cannot express it because the id is positional.
/// What: the raw 32-byte digest, used as a map key (never rendered).
/// Test: `identical_text_hashes_equal`.
fn text_hash(text: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

/// Resolve the HNSW snapshot path for `index`, colocated first (issue #403).
///
/// Why: identical requirement to M003's — the sidecar lives in one of two places
/// and neither migration should re-implement `service::persistence`'s branch.
/// What: the colocated path when it exists, else the legacy global path.
/// Test: covered through `apply` by `m005_reuses_the_existing_vectors`.
fn resolve_hnsw_path(index: &IndexHandle) -> Result<std::path::PathBuf> {
    let colocated = index.root_path.join(".trusty-search").join("hnsw.usearch");
    if colocated.exists() {
        return Ok(colocated);
    }
    crate::service::persistence::hnsw_path(&index.id.0)
        .context("M005: could not resolve legacy hnsw path")
}
