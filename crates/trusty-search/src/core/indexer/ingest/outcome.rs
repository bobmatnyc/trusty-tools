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
use crate::core::chunker::json_exceeds_window_ceiling;

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
/// What: `Indexed` carries how many chunks reached the corpus; `Empty`,
/// `TooLarge` and `NoChunks` mean nothing was written; `Removed` is a
/// tombstone write; `SopsEncrypted` was refused and its old chunks dropped.
/// Test: `zero_chunk_outcomes_are_never_reported_as_indexed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexFileOutcome {
    /// Every chunk the file parsed into reached the corpus.
    Indexed { chunks: usize },
    /// The content is empty or whitespace-only, so it has nothing to index.
    Empty,
    /// A JSON file above the chunker's window ceiling (owner ruling 232).
    TooLarge,
    /// The content is not blank, yet the chunker produced no chunks.
    NoChunks,
    /// The content was a tombstone; the file's chunks were removed.
    Removed,
    /// #8922: the content is a sops-encrypted file. Nothing was indexed and
    /// any chunks an earlier write left for the file were removed.
    SopsEncrypted,
}

impl IndexFileOutcome {
    /// Classify a write whose chunker produced `chunks` chunks for `content`.
    ///
    /// Why: blank content and over-ceiling JSON legitimately have no chunks;
    /// anything else with none is a chunker gap that must surface instead of
    /// passing as success.
    /// What: sops-encrypted content is `SopsEncrypted` whatever the count
    /// (#8922); otherwise `chunks > 0` is `Indexed`, blank content `Empty`,
    /// JSON above the window ceiling `TooLarge`, else `NoChunks`.
    /// Test: `zero_chunk_outcomes_are_never_reported_as_indexed`.
    pub fn classify(file: &str, content: &str, chunks: usize) -> Self {
        if crate::core::sops::is_sops_encrypted(content) {
            Self::SopsEncrypted
        } else if chunks > 0 {
            Self::Indexed { chunks }
        } else if content.trim().is_empty() {
            Self::Empty
        } else if json_exceeds_window_ceiling(file, content) {
            Self::TooLarge
        } else {
            Self::NoChunks
        }
    }

    /// Whether zero chunks is the final answer for this content.
    ///
    /// Why: the batch reindex keeps the content hash of a file whose zero
    /// chunks are final, so it is not re-read on every reindex (#8976).
    /// What: `true` for `Empty`, `TooLarge` and `SopsEncrypted`.
    /// Test: `zero_chunk_outcomes_are_never_reported_as_indexed`.
    pub fn zero_chunks_is_final(&self) -> bool {
        matches!(self, Self::Empty | Self::TooLarge | Self::SopsEncrypted)
    }

    /// The fields a transport adds to its `index-file` response body.
    ///
    /// Why: HTTP and the socket must answer the same write the same way, and
    /// `indexed` keeps its name so existing callers read it unchanged.
    /// What: `indexed` and `chunks` always; `reason` when nothing was
    /// indexed; `removed` for a tombstone. A tombstone keeps `indexed: true`
    /// with zero chunks, the one exemption (owner ruling item 232, Q2).
    /// Test: `zero_chunk_outcomes_are_never_reported_as_indexed`.
    pub fn report_fields(&self) -> serde_json::Map<String, serde_json::Value> {
        let (indexed, chunks, reason) = match self {
            Self::Indexed { chunks } => (true, *chunks, None),
            Self::Empty => (false, 0, Some("empty_file")),
            Self::TooLarge => (false, 0, Some("too_large")),
            Self::NoChunks => (false, 0, Some("no_chunks")),
            Self::Removed => (true, 0, None),
            Self::SopsEncrypted => (false, 0, Some("sops_encrypted")),
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
    /// `indexed: true`, whether the content was blank, too large, or the
    /// chunker failed. The tombstone is the one exemption (ruling 232, Q2).
    #[test]
    fn zero_chunk_outcomes_are_never_reported_as_indexed() {
        let huge = "  \"k\": 1,\n".repeat(10_001);
        let cases = [
            ("a.json", "{\"a\": 1}", 1, true, None, false),
            ("a.json", "", 0, false, Some("empty_file"), true),
            ("a.json", "  \n\t\n", 0, false, Some("empty_file"), true),
            ("a.json", huge.as_str(), 0, false, Some("too_large"), true),
            ("a.txt", huge.as_str(), 0, false, Some("no_chunks"), false),
            ("a.json", "{\"a\": 1}", 0, false, Some("no_chunks"), false),
        ];
        for (file, content, chunks, indexed, reason, is_final) in cases {
            let outcome = IndexFileOutcome::classify(file, content, chunks);
            let fields = outcome.report_fields();
            let at = format!("{file}/{}/{chunks}", content.len());
            assert_eq!(fields["indexed"], indexed, "{at}");
            assert_eq!(fields["chunks"], chunks, "{at}");
            assert_eq!(
                fields.get("reason").and_then(|r| r.as_str()),
                reason,
                "{at}"
            );
            assert_eq!(outcome.zero_chunks_is_final(), is_final, "{at}");
        }
        // #8922: sops content is refused whatever the chunker would produce,
        // and its zero chunks are final. Built from parts so this file is not
        // itself a sops document.
        let sops = format!(
            "k: ENC[AES256{}data:eA==,type:str]\nsops:\n    version: 3.8.1\n",
            "_GCM,"
        );
        let refused = IndexFileOutcome::classify("s.yaml", &sops, 3);
        assert_eq!(refused, IndexFileOutcome::SopsEncrypted);
        let fields = refused.report_fields();
        assert_eq!(fields["indexed"], false);
        assert_eq!(fields["reason"], "sops_encrypted");
        assert!(refused.zero_chunks_is_final());
        let removed = IndexFileOutcome::Removed.report_fields();
        assert_eq!(removed["indexed"], true);
        assert_eq!(removed["chunks"], 0);
        assert_eq!(removed["removed"], true);
        assert!(!IndexFileOutcome::Removed.zero_chunks_is_final());
    }
}
