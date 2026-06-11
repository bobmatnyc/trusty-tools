/// Entity exact-match and virtual-terms tests for the indexer.
use super::*;

#[tokio::test]
async fn test_entity_exact_match_finds_chunk() {
    let idx = make_indexer();
    idx.index_file("e.rs", "pub struct MyType { x: u32 }\nfn f() {}\n")
        .await
        .unwrap();
    let hit = idx.entity_exact_match("MyType").await;
    assert!(hit.is_some(), "expected entity_exact_match to find MyType");
    let hit_id = hit.unwrap();
    let chunks = idx.chunks.read().await;
    assert!(
        chunks
            .get(&hit_id)
            .map(|c| c.file == "e.rs")
            .unwrap_or(false),
        "matched chunk should live in e.rs",
    );
}

#[tokio::test]
async fn test_entity_exact_match_struct_ranks_first() {
    let idx = CodeIndexer::new("ent-rank-1", "/tmp/test");
    idx.index_file(
        "src/types.rs",
        "pub struct FooBar { pub x: u32 }\n\nfn unrelated() { let _ = 1; }\n",
    )
    .await
    .unwrap();
    idx.index_file("src/other.rs", "fn other_thing() {}\n")
        .await
        .unwrap();

    let q = SearchQuery {
        text: "FooBar".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.expect("search");
    assert!(!results.is_empty(), "search must return at least one hit");
    assert_eq!(
        results[0].file,
        abs("src/types.rs"),
        "FooBar's defining file must rank first; got {:?}",
        results.iter().map(|r| &r.file).collect::<Vec<_>>(),
    );
    assert!(
        results[0].content.contains("FooBar"),
        "rank-1 chunk must contain the FooBar definition; got {:?}",
        results[0].content,
    );
}

#[tokio::test]
async fn test_entity_exact_match_skips_non_symbol_entities() {
    let idx = make_indexer();
    idx.index_file("lit.rs", "fn f() { let _ = \"this is a long literal\"; }\n")
        .await
        .unwrap();
    assert!(
        idx.entity_exact_match("literal").await.is_none(),
        "non-symbol entity types must not satisfy entity_exact_match"
    );
}

#[tokio::test]
async fn test_entity_exact_match_skips_multiword_query() {
    let idx = make_indexer();
    idx.index_file("e.rs", "use std::sync::Arc;\nfn f() {}\n")
        .await
        .unwrap();
    assert!(idx.entity_exact_match("Arc thing").await.is_none());
}

#[tokio::test]
async fn test_virtual_terms_populated_from_entities() {
    let idx = make_indexer();
    idx.index_file(
        "v.rs",
        "use std::sync::Arc;\nfn f() { let _x: Arc<String> = Arc::new(String::new()); }\n",
    )
    .await
    .unwrap();
    let chunks = idx.chunks.read().await;
    let f_chunk = chunks
        .values()
        .find(|c| c.function_name.as_deref() == Some("f"))
        .expect("f chunk");
    assert!(
        f_chunk.virtual_terms.iter().any(|t| t == "Arc"),
        "expected 'Arc' in virtual_terms, got {:?}",
        f_chunk.virtual_terms
    );
}
