//! What one `index_file` write did to the corpus (#8976).
//!
//! Why: `index_file` answered `Ok(())` whether the file landed as chunks or
//! produced none, so `POST /index-file` reported `"indexed": true` for a file
//! that stayed unsearchable. The outcome names the zero-chunk cases so every
//! transport reports them as not indexed.
//! What: [`IndexFileOutcome`], its zero-chunk classifier, the response
//! fields the HTTP and socket transports add to their body, and the corpus
//! check the batch reindex runs before it records a file's content hash.
//! Test: `zero_chunk_outcomes_are_never_reported_as_indexed` in this file;
//! `reindex_withholds_the_hash_of_a_file_whose_chunks_did_not_land` in
//! `service::reindex::tests`.

use std::collections::HashSet;

use super::super::CodeIndexer;

impl CodeIndexer {
    /// The files in `expected` whose every chunk id is in the corpus.
    ///
    /// Why: the batch reindex recorded a content hash for every file it sent
    /// to the commit, so a file whose chunks never landed kept a hash with
    /// zero chunks and every later reindex skipped it (#8976).
    /// What: one corpus read lock; a file with no expected ids is never
    /// returned, since it has nothing durable to vouch for.
    /// Test: `reindex_withholds_the_hash_of_a_file_whose_chunks_did_not_land`.
    pub(crate) async fn files_with_all_chunks(
        &self,
        expected: &[(String, Vec<String>)],
    ) -> HashSet<String> {
        self.ensure_chunks_loaded().await;
        let corpus = self.chunks.read().await;
        expected
            .iter()
            .filter(|(_, ids)| !ids.is_empty() && ids.iter().all(|id| corpus.contains_key(id)))
            .map(|(file, _)| file.clone())
            .collect()
    }
}

/// The result of one successful `CodeIndexer::index_file_outcome` call.
///
/// Why: a write that produced no chunks is not the write the caller asked
/// for, and the caller has no other way to learn it (#8976).
/// What: `Indexed` carries how many chunks reached the corpus; `Empty` and
/// `NoChunks` mean nothing was written; `Removed` is a tombstone write.
/// Test: `zero_chunk_outcomes_are_never_reported_as_indexed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexFileOutcome {
    /// Every chunk the file parsed into reached the corpus.
    Indexed { chunks: usize },
    /// The content is empty or whitespace-only, so it has nothing to index.
    Empty,
    /// The content is not blank, yet the chunker produced no chunks.
    NoChunks,
    /// The content was a tombstone; the file's chunks were removed.
    Removed,
}

impl IndexFileOutcome {
    /// Classify a write whose chunker produced `chunks` chunks for `content`.
    ///
    /// Why: blank content legitimately has no chunks; anything else with none
    /// is a chunker gap that must surface instead of passing as success.
    /// What: `chunks > 0` is `Indexed`, blank content `Empty`, else `NoChunks`.
    /// Test: `zero_chunk_outcomes_are_never_reported_as_indexed`.
    pub fn classify(content: &str, chunks: usize) -> Self {
        if chunks > 0 {
            Self::Indexed { chunks }
        } else if content.trim().is_empty() {
            Self::Empty
        } else {
            Self::NoChunks
        }
    }

    /// The fields a transport adds to its `index-file` response body.
    ///
    /// Why: HTTP and the socket must answer the same write the same way, and
    /// `indexed` keeps its name so existing callers read it unchanged.
    /// What: `indexed` and `chunks` always; `reason` when nothing was
    /// indexed; `removed` for a tombstone, which keeps its old `indexed: true`.
    /// Test: `zero_chunk_outcomes_are_never_reported_as_indexed`.
    pub fn report_fields(&self) -> serde_json::Map<String, serde_json::Value> {
        let (indexed, chunks, reason) = match self {
            Self::Indexed { chunks } => (true, *chunks, None),
            Self::Empty => (false, 0, Some("empty_file")),
            Self::NoChunks => (false, 0, Some("no_chunks")),
            Self::Removed => (true, 0, None),
        };
        let mut fields = serde_json::Map::new();
        fields.insert("indexed".into(), indexed.into());
        fields.insert("chunks".into(), chunks.into());
        if let Some(reason) = reason {
            fields.insert("reason".into(), reason.into());
        }
        if *self == Self::Removed {
            fields.insert("removed".into(), true.into());
        }
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::IndexFileOutcome;

    /// #8976 fail-open check: a write with zero chunks must never read as
    /// `indexed: true`, whether the content was blank or the chunker failed.
    #[test]
    fn zero_chunk_outcomes_are_never_reported_as_indexed() {
        let cases = [
            ("{\"a\": 1}", 1, true, None),
            ("", 0, false, Some("empty_file")),
            ("  \n\t\n", 0, false, Some("empty_file")),
            ("{\"a\": 1}", 0, false, Some("no_chunks")),
        ];
        for (content, chunks, indexed, reason) in cases {
            let fields = IndexFileOutcome::classify(content, chunks).report_fields();
            assert_eq!(fields["indexed"], indexed, "{content:?}/{chunks}");
            assert_eq!(fields["chunks"], chunks, "{content:?}/{chunks}");
            assert_eq!(
                fields.get("reason").and_then(|r| r.as_str()),
                reason,
                "{content:?}/{chunks}"
            );
        }
        let removed = IndexFileOutcome::Removed.report_fields();
        assert_eq!(removed["indexed"], true);
        assert_eq!(removed["removed"], true);
    }
}
