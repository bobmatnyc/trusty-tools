//! The durable M005 recovery plan — what an interrupted re-chunk needs to
//! finish, held outside the corpus it is about to clear (#6581).
//!
//! Why: M005 clears the corpus and rebuilds it from source, so every input the
//! pass depends on is destroyed by its own first step. The file list, the
//! text-hash→vector-id map and the old id list all come from the chunks the
//! clear deletes, which makes the corpus useless as a record of "was this pass
//! finished". Inferring completion from corpus contents instead — "does any
//! pre-#6581 named id remain" — answers `false` for a corpus the clear emptied
//! and for one that finished cleanly alike, so a crash anywhere between the
//! clear and the final flush read as success and let `run_migrations` stamp
//! `schema_version = 5` over a partially or wholly empty index.
//!
//! What: [`M005Plan`] is written durably into the corpus `_meta` table BEFORE
//! the clear and removed only after the pass fully succeeds. Its presence is
//! the authority on "a pass started and did not finish"; its absence plus no
//! legacy ids means there is nothing to do. `_meta` survives
//! `clear_corpus_for_rechunk` (which clears the chunk, entity and KG tables),
//! and redb commits it in its own ACID transaction, so the marker can never be
//! half-written.
//!
//! Test: `core::migration::m005::tests::{m005_resumes_after_a_crash_before_the_first_batch,
//! m005_resumes_after_a_crash_between_the_remap_and_the_flush,
//! m005_never_advances_the_schema_over_missing_chunks}`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::corpus::CorpusStore;

/// Everything M005 needs to finish a pass, captured before the clear destroys
/// its source (#6581).
///
/// Why: see module doc. Each field is derived from the pre-clear corpus and is
/// unrecoverable afterwards — `files` says what to re-chunk, `vector_by_text`
/// is what lets a re-chunked chunk inherit its existing vector instead of being
/// re-embedded (the ruling's zero budget), and `old_ids` is what identifies the
/// sidecar entries no new chunk claimed.
/// What: a plain serde record. `vector_by_text` is a `Vec` of pairs rather than
/// a map because a `[u8; 32]` key has no JSON string form;
/// [`Self::vector_by_text_map`] restores the lookup shape.
/// Test: `m005_plan_roundtrips_through_the_corpus_meta_table`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct M005Plan {
    /// Root-relative paths to re-chunk from source.
    pub files: BTreeSet<String>,
    /// `sha256(chunk text) → the id that text was stored under before the pass`.
    pub vector_by_text: Vec<([u8; 32], String)>,
    /// Every chunk id the corpus held before the pass.
    pub old_ids: Vec<String>,
    /// How many chunks the corpus held before the pass, for the operator log.
    pub old_count: usize,
}

impl M005Plan {
    /// The `text hash → old id` lookup, rebuilt from the serialized pairs.
    pub(super) fn vector_by_text_map(&self) -> HashMap<[u8; 32], String> {
        self.vector_by_text.iter().cloned().collect()
    }

    /// Read the plan a previous, unfinished pass left behind.
    ///
    /// Why: this is M005's idempotency guard. A plan that is present means a
    /// pass started and did not reach its final step, whatever the corpus now
    /// looks like.
    /// What: `Ok(None)` when no pass is outstanding. A blob that fails to parse
    /// is treated as an outstanding pass rather than an error — re-running a
    /// deterministic re-chunk is always safe, and refusing to boot over an
    /// unreadable marker would be worse than repeating work.
    /// Test: `m005_plan_roundtrips_through_the_corpus_meta_table`,
    /// `m005_resumes_after_a_crash_before_the_first_batch`.
    pub(super) async fn load(corpus: &Arc<CorpusStore>) -> Result<Option<Self>> {
        let corpus = Arc::clone(corpus);
        let read = tokio::task::spawn_blocking(move || corpus.read_m005_plan_sync())
            .await
            .context("M005: recovery-plan read task panicked")?;
        let Some(bytes) = read.context("M005: could not read the recovery plan")? else {
            return Ok(None);
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(plan) => Ok(Some(plan)),
            Err(e) => {
                tracing::warn!(
                    "M005: the recovery plan is unreadable ({e}) — re-running the pass from \
                     the corpus as it stands rather than assuming it finished"
                );
                Ok(Some(Self::default_for_unreadable()))
            }
        }
    }

    /// The stand-in for a plan that will not parse: empty, so the caller
    /// re-derives from whatever the corpus still holds.
    fn default_for_unreadable() -> Self {
        Self {
            files: BTreeSet::new(),
            vector_by_text: Vec::new(),
            old_ids: Vec::new(),
            old_count: 0,
        }
    }

    /// `true` when this plan carries nothing to act on.
    pub(super) fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Persist this plan, durably, before the clear runs.
    ///
    /// Test: `m005_plan_roundtrips_through_the_corpus_meta_table`.
    pub(super) async fn store(&self, corpus: &Arc<CorpusStore>) -> Result<()> {
        let bytes = serde_json::to_vec(self).context("M005: could not serialize the plan")?;
        let corpus = Arc::clone(corpus);
        tokio::task::spawn_blocking(move || corpus.write_m005_plan_sync(&bytes))
            .await
            .context("M005: recovery-plan write task panicked")?
            .context("M005: could not persist the recovery plan")
    }

    /// Remove the marker — the pass finished.
    ///
    /// Why: this is the ONLY thing that makes a later boot treat M005 as done,
    /// so it runs after every other step has succeeded and never before.
    /// Test: `m005_is_a_no_op_on_an_already_migrated_corpus`.
    pub(super) async fn clear(corpus: &Arc<CorpusStore>) -> Result<()> {
        let corpus = Arc::clone(corpus);
        tokio::task::spawn_blocking(move || corpus.clear_m005_plan_sync())
            .await
            .context("M005: recovery-plan clear task panicked")?
            .context("M005: could not clear the recovery plan")
    }
}
