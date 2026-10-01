//! #8976: `index_file` on a file that produces zero chunks.
//!
//! Why: a JSON file of 500+ lines produced zero chunks, and `index_file`
//! returned `Ok(())` anyway, so `POST /index-file` answered `indexed: true`
//! for a file that never became searchable.
//! What: a 600-line JSON file lands chunks; blank content reports `Empty`.
//! Test: `cargo test -p trusty-search -- zero_chunk_8976`

use super::super::{CodeIndexer, IndexFileOutcome};

/// A pretty-printed JSON object of `entries` lines plus its braces.
fn pretty_json(entries: usize) -> String {
    let body: Vec<String> = (0..entries)
        .map(|i| format!("  \"setting_{i}\": \"value_{i}\","))
        .collect();
    format!("{{\n{}\n  \"end\": true\n}}\n", body.join("\n"))
}

/// Fails against 889f555fc3: the large file left the corpus empty.
#[tokio::test]
async fn index_file_on_large_json_lands_chunks() {
    let idx = CodeIndexer::new("zero-chunk-8976", "/tmp/zero-chunk-8976");
    idx.index_file("config/big.json", &pretty_json(600))
        .await
        .expect("the write succeeds");
    let landed = idx.chunk_ids_for_file("config/big.json").await;
    assert!(
        !landed.is_empty(),
        "a 600-line JSON file must be searchable, not silently chunkless"
    );
    let outcome = idx
        .index_file_outcome("config/big.json", &pretty_json(600))
        .await
        .expect("the rewrite succeeds");
    assert_eq!(
        outcome,
        IndexFileOutcome::Indexed {
            chunks: landed.len()
        }
    );
}

/// Blank content has nothing to index; it is reported, never `Indexed`.
#[tokio::test]
async fn index_file_on_blank_content_reports_empty() {
    let idx = CodeIndexer::new("zero-chunk-8976-blank", "/tmp/zero-chunk-8976-blank");
    for content in ["", "\n  \n"] {
        let outcome = idx
            .index_file_outcome("config/empty.json", content)
            .await
            .expect("a blank write is not an error");
        assert_eq!(outcome, IndexFileOutcome::Empty, "{content:?}");
    }
    assert_eq!(idx.in_memory_chunk_count().await, 0);
}
