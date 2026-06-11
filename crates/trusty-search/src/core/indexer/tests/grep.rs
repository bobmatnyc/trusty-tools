/// Grep fallback and match-reason tests for the indexer.
use super::*;

#[test]
fn test_compute_match_reason_fallback_label() {
    // Why: the `(false,false,false)` arm must return `"fallback:ripgrep"`
    // (issue #75), not the bare `"fallback"` string it used to return.
    assert_eq!(
        compute_match_reason(false, false, false),
        "fallback:ripgrep"
    );
    assert_eq!(compute_match_reason(true, false, false), "vector");
    assert_eq!(compute_match_reason(false, true, false), "bm25");
    assert_eq!(compute_match_reason(true, true, false), "hybrid");
    assert_eq!(compute_match_reason(false, false, true), "hybrid+kg");
}

#[tokio::test]
async fn test_grep_fallback_returns_substring_hits() {
    // Why: when both primary lanes return nothing, an exact-substring scan
    // over the in-memory corpus should still surface relevant chunks.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn alpha_qwerty_unique() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn beta() {}"))
        .await
        .unwrap();
    let hits = idx.grep_fallback_search("alpha_qwerty_unique", 5).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "a");
    assert!(hits[0].1 < 0.01, "fallback score should be sub-0.01");
}

#[tokio::test]
async fn test_grep_fallback_treats_query_as_literal() {
    // Why: user input must never be treated as a regex. A query containing
    // regex metacharacters should match literally.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn foo() {} // literal: a.b.c"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn aXbYc() {}"))
        .await
        .unwrap();
    let hits = idx.grep_fallback_search("a.b.c", 5).await;
    let ids: Vec<&str> = hits.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"a"), "literal match in a missing: {ids:?}");
    assert!(
        !ids.contains(&"b"),
        "wildcard-style match leaked through regex escape"
    );
}

#[test]
fn test_merge_grep_lane_appends_new_ids() {
    // Why: merge_grep_lane must add brand-new ids to the fused list without
    // dropping any of the existing fused entries.
    use super::search::merge_grep_lane;
    let fused = vec![("a".to_string(), 0.05), ("b".to_string(), 0.04)];
    let grep_lane = vec![("c".to_string(), 0.001)];
    let out = merge_grep_lane(fused, &grep_lane, 0.5, 10);
    let ids: Vec<&str> = out.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"a"));
    assert!(ids.contains(&"b"));
    assert!(ids.contains(&"c"));
    assert_eq!(out[0].0, "a");
}
