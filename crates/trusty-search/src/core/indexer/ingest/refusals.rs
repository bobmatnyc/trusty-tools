//! The durable record of chunks whose embedding the store refused (#8884).
//!
//! Why: the store refuses a NaN or all-zero embedding (#764), so that chunk
//! stays without a vector on every pass. A pass excuses the refusal in memory
//! and settles `Ready`, but a restore compares chunks against vectors. With no
//! durable record, every restart demoted `semantic` and queued a backfill that
//! refused the same chunks again.
//! What: [`RefusalRecord`] maps a chunk id to the SHA-256 of the content whose
//! embedding was refused, stored in the corpus `_meta` table.
//! [`CodeIndexer::record_vector_refusals`] folds each vector commit into it;
//! [`CodeIndexer::refusals_behind_vector_gap`] is the restore-time question.
//! The excuse is keyed by content, so a chunk re-ingested with new content is
//! owed an embedding again. An unreadable record never excuses anything.
//! Test: `a_restore_after_a_refused_embedding_does_not_demote_the_stage`,
//! `an_unreadable_refusal_record_is_treated_as_a_real_gap`,
//! `a_refused_chunk_whose_content_changed_is_backfilled`,
//! `a_new_chunk_beside_a_refused_one_is_backfilled`,
//! `the_refusal_record_is_capped_and_drops_accepted_chunks`.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::corpus::CorpusStore;

use super::super::CodeIndexer;
use super::deferred::VectorCoverage;

/// The record format this build reads and writes.
const RECORD_VERSION: u32 = 1;

/// The most refusals the record holds (#8884).
///
/// Why: the record is rewritten whole on every commit that changes it. An
/// embedder that refuses everything would otherwise grow it to the corpus size.
/// A refusal past the cap goes unrecorded, and a restore then treats that
/// chunk as a real gap, the behaviour before the record existed.
pub(crate) const MAX_RECORDED_REFUSALS: usize = 1024;

/// Chunk id to the SHA-256 hex of the content whose embedding was refused.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RefusalRecord {
    version: u32,
    refused: BTreeMap<String, String>,
}

impl Default for RefusalRecord {
    fn default() -> Self {
        Self {
            version: RECORD_VERSION,
            refused: BTreeMap::new(),
        }
    }
}

/// The fingerprint a refusal is keyed by.
pub(crate) fn content_fingerprint(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

impl RefusalRecord {
    /// Parse stored bytes. An unknown version is an error, never a guess.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let record: Self =
            serde_json::from_slice(bytes).context("parse the vector-refusal record")?;
        if record.version != RECORD_VERSION {
            bail!("unknown vector-refusal record version {}", record.version);
        }
        Ok(record)
    }

    /// Fold one commit in: an accepted id loses its excuse, a refused one gains
    /// it, up to [`MAX_RECORDED_REFUSALS`]. Returns whether the record changed.
    pub(crate) fn apply(&mut self, accepted: &[String], refused: &[(String, String)]) -> bool {
        let before = self.refused.len();
        for id in accepted {
            self.refused.remove(id);
        }
        let mut changed = self.refused.len() != before;
        for (id, fingerprint) in refused {
            if self.refused.get(id) == Some(fingerprint) {
                continue;
            }
            if self.refused.len() >= MAX_RECORDED_REFUSALS && !self.refused.contains_key(id) {
                continue;
            }
            self.refused.insert(id.clone(), fingerprint.clone());
            changed = true;
        }
        changed
    }

    /// Whether the record excuses `id` while it holds `content`.
    pub(crate) fn excuses(&self, id: &str, content: &str) -> bool {
        self.refused
            .get(id)
            .is_some_and(|fp| *fp == content_fingerprint(content))
    }
}

/// Read, fold and write back the record under one corpus.
///
/// What: an unreadable or unparseable stored record is replaced rather than
/// merged into. Dropping its entries only makes more gaps read as real.
fn update_record(
    corpus: &CorpusStore,
    accepted: &[String],
    refused: &[(String, String)],
) -> Result<bool> {
    let (mut record, mut changed) = match corpus.read_vector_refusals_sync() {
        Ok(None) => (RefusalRecord::default(), false),
        Ok(Some(bytes)) => match RefusalRecord::decode(&bytes) {
            Ok(record) => (record, false),
            Err(e) => {
                tracing::warn!("vector refusals: replacing an unreadable record ({e:#})");
                (RefusalRecord::default(), true)
            }
        },
        Err(e) => {
            tracing::warn!("vector refusals: replacing a record that could not be read ({e:#})");
            (RefusalRecord::default(), true)
        }
    };
    changed |= record.apply(accepted, refused);
    if !changed {
        return Ok(false);
    }
    let bytes = if record.refused.is_empty() {
        None
    } else {
        Some(serde_json::to_vec(&record).context("encode the vector-refusal record")?)
    };
    corpus.write_vector_refusals_sync(bytes.as_deref())?;
    Ok(true)
}

impl CodeIndexer {
    /// Fold one vector commit into the durable refusal record (#8884).
    ///
    /// Why: see the module doc.
    /// What: `accepted` are the ids whose vector this commit upserts; each
    /// loses any excuse, since its content now embeds. `refused` pairs each
    /// refused id with its content fingerprint. A no-op with no corpus wired
    /// or nothing to fold. Best-effort: a failed write is logged, and a lost
    /// refusal only makes a restore treat that chunk as a real gap.
    pub(crate) async fn record_vector_refusals(
        &self,
        accepted: Vec<String>,
        refused: Vec<(String, String)>,
    ) {
        let Some(corpus) = self.corpus.clone() else {
            return;
        };
        if accepted.is_empty() && refused.is_empty() {
            return;
        }
        let outcome =
            tokio::task::spawn_blocking(move || update_record(&corpus, &accepted, &refused)).await;
        let failure = match outcome {
            Ok(Ok(_)) => return,
            Ok(Err(e)) => format!("{e:#}"),
            Err(e) => format!("the write task did not finish ({e})"),
        };
        tracing::warn!(
            "index '{}': the vector-refusal record was not updated ({failure}); a restore \
             treats any refusal it lacks as a real gap (#8884)",
            self.index_id,
        );
    }

    /// How many refused chunks explain the whole vector gap, if they do (#8884).
    ///
    /// Why: a restore must not demote `semantic` or queue a backfill for
    /// chunks whose unchanged content the store already refused.
    /// What: `Ok(Some(n))` only when the durable record is readable and every
    /// corpus chunk without a vector (measured by id) is recorded as refused
    /// with its current content; `n` is that count. `Ok(None)` when there is
    /// no corpus, no record, no gap, or any missing chunk is not so excused.
    /// `Err` when the record or the coverage cannot be read, which the caller
    /// must treat as a real gap. Reads the chunk ids only when a record exists.
    /// Test: see the module doc.
    pub(crate) async fn refusals_behind_vector_gap(&self) -> Result<Option<usize>> {
        let Some(corpus) = self.corpus.clone() else {
            return Ok(None);
        };
        let reader = Arc::clone(&corpus);
        let stored = tokio::task::spawn_blocking(move || reader.read_vector_refusals_sync())
            .await
            .context("the vector-refusal record read did not finish")??;
        let Some(bytes) = stored else {
            return Ok(None);
        };
        let record = RefusalRecord::decode(&bytes)?;
        if record.refused.is_empty() {
            return Ok(None);
        }
        let missing = match self.vector_coverage().await {
            VectorCoverage::Measured { missing, .. } => missing,
            VectorCoverage::NotApplicable => return Ok(None),
            VectorCoverage::Unreadable(why) => bail!("{why}"),
        };
        if missing.is_empty() || missing.iter().any(|id| !record.refused.contains_key(id)) {
            return Ok(None);
        }
        let count = missing.len();
        let current = tokio::task::spawn_blocking(move || {
            let ids: Vec<&str> = missing.iter().map(String::as_str).collect();
            corpus.get_chunks(&ids)
        })
        .await
        .context("the refused-chunk content read did not finish")?
        .context("read the content of the refused chunks")?;
        let all_excused = current.len() == count
            && current
                .iter()
                .all(|chunk| record.excuses(&chunk.id, &chunk.content));
        Ok(all_excused.then_some(count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(id: &str, content: &str) -> (String, String) {
        (id.to_owned(), content_fingerprint(content))
    }

    /// Why (#8884): a chunk that embeds must lose its excuse, or a later lost
    /// vector over the same content reads as refused; and an embedder that
    /// refuses everything must not grow the record to the corpus size.
    /// Test: this test.
    #[test]
    fn the_refusal_record_is_capped_and_drops_accepted_chunks() {
        let mut record = RefusalRecord::default();
        assert!(record.apply(&[], &[refusal("a", "x")]));
        assert!(record.excuses("a", "x"));
        assert!(!record.excuses("a", "y"), "changed content is not excused");
        assert!(!record.apply(&[], &[refusal("a", "x")]), "no-op fold");

        assert!(record.apply(&["a".to_owned()], &[]));
        assert!(
            !record.excuses("a", "x"),
            "an accepted chunk loses its excuse"
        );

        let flood: Vec<_> = (0..MAX_RECORDED_REFUSALS + 5)
            .map(|i| refusal(&format!("c{i}"), "z"))
            .collect();
        record.apply(&[], &flood);
        assert_eq!(record.refused.len(), MAX_RECORDED_REFUSALS);

        let bytes = serde_json::to_vec(&record).expect("encode");
        assert_eq!(RefusalRecord::decode(&bytes).expect("decode"), record);
        assert!(RefusalRecord::decode(b"not json").is_err());
        assert!(RefusalRecord::decode(br#"{"version":2,"refused":{}}"#).is_err());
    }
}
