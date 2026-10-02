//! #8922: the indexer never indexes sops-encrypted content.
//!
//! Why: every ingest path — `index_file`, the watcher, both reconciles and
//! the reindex batch — reaches the corpus through `index_file_outcome` or
//! `parse_files_parallel`, so the content check lives there once.
//! What: a single write of sops content is refused and drops the chunks the
//! file's plaintext left; a batch write of it lands nothing.
//! Test: `cargo test -p trusty-search -- sops_8922`

use super::super::{CodeIndexer, IndexFileOutcome};
use crate::core::sops::sample_sops_yaml;

const PLAIN: &str = "db_password: hunter2\nhost: db.internal\n";

/// Fails with the sops branch in `index_file_outcome` removed: the write
/// lands chunks and the plaintext chunks stay searchable.
#[tokio::test]
async fn index_file_refuses_sops_content_and_drops_its_old_chunks() {
    let idx = CodeIndexer::new("sops-8922", "/tmp/sops-8922");
    idx.index_file("config/secrets.yaml", PLAIN)
        .await
        .expect("the plaintext write succeeds");
    assert!(
        !idx.chunk_ids_for_file("config/secrets.yaml")
            .await
            .is_empty(),
        "the fixture must index the plaintext first"
    );

    let outcome = idx
        .index_file_outcome("config/secrets.yaml", &sample_sops_yaml())
        .await
        .expect("a refused write is not an error");
    assert_eq!(outcome, IndexFileOutcome::SopsEncrypted);
    assert!(
        idx.chunk_ids_for_file("config/secrets.yaml")
            .await
            .is_empty(),
        "the file's plaintext chunks must not survive it becoming encrypted"
    );
}

/// The batch path every reindex and rescan uses lands no chunk for sops
/// content. Fails with the sops arm in `parse_files_parallel` removed.
#[tokio::test]
async fn batch_write_of_sops_content_lands_nothing() {
    let idx = CodeIndexer::new("sops-8922-batch", "/tmp/sops-8922-batch");
    let added = idx
        .index_files_batch(&[
            ("config/secrets.yaml".to_string(), sample_sops_yaml()),
            ("src/lib.rs".to_string(), "pub fn ok() {}\n".to_string()),
        ])
        .await
        .expect("the batch succeeds");
    assert!(added > 0, "the plain file in the batch still lands");
    assert!(idx
        .chunk_ids_for_file("config/secrets.yaml")
        .await
        .is_empty());
}
