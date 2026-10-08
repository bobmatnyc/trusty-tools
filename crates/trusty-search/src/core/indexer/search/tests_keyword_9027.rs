//! `Keyword` routes like `Unknown` in the file-type filter (#9027).
//!
//! Why: #9116 gave a bare one-word query its own `Keyword` intent and kept
//! `Unknown`'s routing at every site, but no test exercised the soft doc
//! downrank for `Keyword`. A change to one intent's arm could then split the
//! two silently.
//! What: calls `apply_archive_downrank` directly with mode `Code`, the only
//! mode in which the soft downrank applies (search upgrades a balanced intent
//! out of `Code` before it gets here), and checks `hard_file_filter`, which
//! decides whether materialisation drops the doc instead (#9404).
//! Test: this module.

use super::drops::SearchDrops;
use super::hard_file_filter;
use crate::core::chunker::ChunkType;
use crate::core::classifier::QueryIntent;
use crate::core::indexer::{CodeChunk, CodeIndexer, SearchMode};

fn chunk(id: &str, file: &str) -> CodeChunk {
    CodeChunk {
        id: id.into(),
        file: file.into(),
        path: None,
        language: None,
        start_line: 1,
        end_line: 1,
        content: "connection pooling".into(),
        function_name: None,
        score: 1.0,
        compact_snippet: None,
        match_reason: "test".into(),
        chunk_type: ChunkType::Code,
        calls: vec![],
        inherits_from: vec![],
        chunk_depth: 0,
        index_id: None,
        on_branch: false,
        archive_reason: None,
    }
}

/// The ids and scores `apply_archive_downrank` keeps for `intent` in `Code`.
fn kept(intent: QueryIntent) -> Vec<(String, f32)> {
    let idx = CodeIndexer::new("kw-9027", "/tmp/kw-9027");
    let mut results = vec![
        chunk("doc", "/tmp/kw-9027/docs/pooling.md"),
        chunk("src", "/tmp/kw-9027/src/pool.rs"),
    ];
    idx.apply_archive_downrank(
        &mut results,
        &intent,
        SearchMode::Code,
        &SearchDrops::default(),
    );
    results.into_iter().map(|c| (c.id, c.score)).collect()
}

/// #9027: in `Code` mode a `Keyword` query keeps a doc hit, down-ranked rather
/// than dropped, exactly as `Unknown` does; `BugDebt` still drops it.
#[test]
fn keyword_downranks_docs_in_code_mode_like_unknown() {
    let keyword = kept(QueryIntent::Keyword);
    assert_eq!(keyword, kept(QueryIntent::Unknown));
    let doc = keyword
        .iter()
        .find(|(id, _)| id == "doc")
        .expect("doc kept");
    assert!(doc.1 < 1.0, "the doc hit is down-ranked: {keyword:?}");
    assert_eq!(
        hard_file_filter(&QueryIntent::Keyword, SearchMode::Code),
        None
    );
    assert_eq!(
        hard_file_filter(&QueryIntent::Unknown, SearchMode::Code),
        None
    );
    assert_eq!(
        hard_file_filter(&QueryIntent::BugDebt, SearchMode::Code),
        Some(SearchMode::Code),
        "a non-balanced intent still hard-filters docs"
    );
}
