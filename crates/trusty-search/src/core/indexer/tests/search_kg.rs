/// KG expansion, graph-stage, and refine_query tests for the indexer.
use super::*;

#[tokio::test]
async fn test_kg_expansion_marks_neighbours_with_hybrid_kg() {
    let idx = CodeIndexer::new("kg-test", "/tmp/test");
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 3,
        content: "fn login_handler() { /* dispatch to verifier */ }".to_string(),
        function_name: Some("login_handler".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["authenticate".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "a:1".to_string(),
        file: "a.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn authenticate() {}".to_string(),
        function_name: Some("authenticate".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let q = SearchQuery {
        text: "callers of authenticate".to_string(),
        top_k: 10,
        expand_graph: true,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let login = results
        .iter()
        .find(|c| c.id == "h:1")
        .expect("login_handler should surface via KG expansion");
    assert_eq!(
        login.match_reason, "hybrid+kg",
        "KG-expanded chunks must carry hybrid+kg marker, got {}",
        login.match_reason
    );
    let trigger = results
        .iter()
        .find(|c| c.id == "a:1")
        .expect("authenticate must appear directly");
    let expected = trigger.score * KG_EXPAND_SCORE_FACTOR;
    assert!(
        (login.score - expected).abs() < 1e-5,
        "expected KG score = 0.7 * {} = {}, got {}",
        trigger.score,
        expected,
        login.score
    );
}

#[tokio::test]
async fn test_kg_expansion_disabled_by_expand_graph_false() {
    let idx = make_indexer();
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn caller() { target(); }".to_string(),
        function_name: Some("caller".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["target".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "t:1".to_string(),
        file: "t.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn target() {}".to_string(),
        function_name: Some("target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let q = SearchQuery {
        text: "callers of target".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.iter().any(|c| c.match_reason.contains("kg")),
        "expand_graph=false must suppress KG expansion, got {results:#?}"
    );
}

/// Issue #138 — `SearchStage::Semantic` skips KG expansion even when the
/// query intent would otherwise enable it.
#[tokio::test]
async fn search_semantic_stage_skips_kg_expansion() {
    let idx = make_indexer();
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn caller() { /* dispatch */ }".to_string(),
        function_name: Some("caller".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["target".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "t:1".to_string(),
        file: "t.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn target() {}".to_string(),
        function_name: Some("target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    let q = SearchQuery {
        text: "callers of target".to_string(),
        top_k: 10,
        expand_graph: true,
        compact: false,
        stage: Some(super::SearchStage::Semantic),
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.iter().any(|c| c.match_reason.contains("kg")),
        "stage=Semantic must suppress KG expansion, got {results:#?}"
    );
}

/// Issue #138 — `SearchStage::Graph` forces KG expansion ON regardless
/// of the intent's `use_kg_first` weighting.
#[tokio::test]
async fn search_graph_stage_forces_kg_expansion_on_definition_query() {
    let idx = CodeIndexer::new("graph-stage-force", "/tmp/test");
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn caller() { /* dispatch to function */ }".to_string(),
        function_name: Some("caller".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["target".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "t:1".to_string(),
        file: "t.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn target() {}".to_string(),
        function_name: Some("target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let q = SearchQuery {
        text: "target".to_string(),
        top_k: 10,
        expand_graph: true,
        compact: false,
        stage: Some(super::SearchStage::Graph),
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let caller = results
        .iter()
        .find(|c| c.id == "h:1")
        .unwrap_or_else(|| panic!("caller must surface via KG, got {results:#?}"));
    assert!(
        caller.match_reason.contains("kg"),
        "stage=Graph must force KG expansion on caller, got match_reason={}",
        caller.match_reason
    );
}

#[tokio::test]
async fn test_kg_results_survive_top_k_truncation() {
    // Why: issue #132 — top_k=1 on a query that seeds an expansion must
    // still return the KG-expanded neighbours if the seed is the only direct
    // hit, not silently drop them via the pre-expansion top_k cut.
    let idx = make_indexer();
    idx.add_chunk(RawChunk {
        id: "seed:1".to_string(),
        file: "seed.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn seed_fn() { target_fn(); }".to_string(),
        function_name: Some("seed_fn".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["target_fn".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "target:1".to_string(),
        file: "target.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn target_fn() {}".to_string(),
        function_name: Some("target_fn".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let q = SearchQuery {
        text: "callers of target_fn".to_string(),
        top_k: 1,
        expand_graph: true,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    // Both the direct hit and its KG neighbour must survive even at top_k=1.
    assert!(
        results.len() >= 2,
        "KG-expanded neighbours must survive top_k=1; got {} results: {results:#?}",
        results.len()
    );
}

#[tokio::test]
async fn test_kg_refine_query_none_preserves_all_neighbours() {
    let idx = make_indexer();
    idx.add_chunk(RawChunk {
        id: "seed:1".to_string(),
        file: "seed.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn seed_fn() { neighbour_a(); neighbour_b(); }".to_string(),
        function_name: Some("seed_fn".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["neighbour_a".to_string(), "neighbour_b".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "na:1".to_string(),
        file: "a.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn neighbour_a() {}".to_string(),
        function_name: Some("neighbour_a".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "nb:1".to_string(),
        file: "b.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn neighbour_b() {}".to_string(),
        function_name: Some("neighbour_b".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let q = SearchQuery {
        text: "callers of seed_fn".to_string(),
        top_k: 20,
        expand_graph: true,
        compact: false,
        refine_query: None,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
    assert!(
        ids.contains(&"na:1"),
        "neighbour_a must appear without refine_query, got {ids:?}"
    );
    assert!(
        ids.contains(&"nb:1"),
        "neighbour_b must appear without refine_query, got {ids:?}"
    );
}

#[tokio::test]
async fn test_kg_refine_query_filters_irrelevant_neighbours() {
    use crate::core::classifier::QueryIntent;

    let idx = make_indexer();

    idx.add_chunk(RawChunk {
        id: "seed:1".to_string(),
        file: "seed.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn seed_fn() { auth_target(); xyz_qqq(); }".to_string(),
        function_name: Some("seed_fn".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["auth_target".to_string(), "xyz_qqq".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let refine_text = "fn auth_target() { /* JWT validation */ }";
    idx.add_chunk(RawChunk {
        id: "rel:1".to_string(),
        file: "rel.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: refine_text.to_string(),
        function_name: Some("auth_target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    idx.add_chunk(RawChunk {
        id: "irr:1".to_string(),
        file: "irr.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "OPQRSTUVWXYZOPQRSTUVWXYZOPQRSTUVWXYZOPQRSTUVWXYZ".to_string(),
        function_name: Some("xyz_qqq".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let fused_seed: Vec<(String, f32)> = vec![("seed:1".to_string(), 1.0)];
    let intent = QueryIntent::Usage;

    let (all_no_refine, kg_ids_no_refine) = idx
        .expand_with_kg_for_test(fused_seed.clone(), &intent, true, true, None)
        .await;
    let no_refine_ids: Vec<&str> = all_no_refine.iter().map(|(id, _)| id.as_str()).collect();
    assert!(
        kg_ids_no_refine.contains("rel:1"),
        "rel:1 must appear in KG expansion without refine_embedding, \
         kg_ids={kg_ids_no_refine:?}"
    );
    assert!(
        kg_ids_no_refine.contains("irr:1"),
        "irr:1 must appear in KG expansion without refine_embedding, \
         kg_ids={kg_ids_no_refine:?}"
    );
    assert!(
        no_refine_ids.contains(&"rel:1"),
        "rel:1 must be in all_no_refine, got {no_refine_ids:?}"
    );
    assert!(
        no_refine_ids.contains(&"irr:1"),
        "irr:1 must be in all_no_refine, got {no_refine_ids:?}"
    );

    let refine_emb = idx
        .embed_text(refine_text)
        .await
        .unwrap()
        .unwrap_or_default();

    let rel_emb = idx.get_embedding("rel:1").unwrap_or_default();
    let irr_emb = idx.get_embedding("irr:1").unwrap_or_default();
    let cos_rel = crate::core::mmr::cosine_similarity(&refine_emb, &rel_emb);
    let cos_irr = crate::core::mmr::cosine_similarity(&refine_emb, &irr_emb);
    eprintln!(
        "cos_rel={cos_rel:.4} cos_irr={cos_irr:.4} threshold={}",
        KG_REFINE_THRESHOLD
    );
    assert!(
        cos_rel >= KG_REFINE_THRESHOLD,
        "relevant chunk cosine {cos_rel:.4} must be >= threshold {}",
        KG_REFINE_THRESHOLD
    );
    assert!(
        cos_irr < KG_REFINE_THRESHOLD,
        "irrelevant chunk cosine {cos_irr:.4} must be < threshold {}",
        KG_REFINE_THRESHOLD
    );

    let (all_with_refine, kg_ids_with_refine) = idx
        .expand_with_kg_for_test(
            fused_seed.clone(),
            &intent,
            true,
            true,
            Some(refine_emb.as_slice()),
        )
        .await;
    let refine_ids: Vec<&str> = all_with_refine.iter().map(|(id, _)| id.as_str()).collect();

    assert!(
        kg_ids_with_refine.contains("rel:1"),
        "rel:1 must survive the refine filter (cosine={cos_rel:.4} >= threshold), \
         kg_ids={kg_ids_with_refine:?}"
    );
    assert!(
        !kg_ids_with_refine.contains("irr:1"),
        "irr:1 must be dropped by the refine filter (cosine={cos_irr:.4} < threshold), \
         kg_ids={kg_ids_with_refine:?}"
    );
    assert!(
        refine_ids.contains(&"rel:1"),
        "rel:1 must be in final results (cosine={cos_rel:.4}), got {refine_ids:?}"
    );
    assert!(
        !refine_ids.contains(&"irr:1"),
        "irr:1 must not be in final results (cosine={cos_irr:.4}), got {refine_ids:?}"
    );
}

/// Threshold boundary: a neighbour with cosine exactly equal to
/// `KG_REFINE_THRESHOLD` must be kept (>= semantics).
#[tokio::test]
async fn test_kg_refine_threshold_boundary() {
    use crate::core::mmr::cosine_similarity;
    use KG_REFINE_THRESHOLD;

    let threshold = KG_REFINE_THRESHOLD;
    let chunk_vec = vec![1.0_f32, 0.0];
    let refine_vec = vec![threshold, (1.0_f32 - threshold * threshold).sqrt()];

    let actual_cos = cosine_similarity(&chunk_vec, &refine_vec);
    assert!(
        (actual_cos - threshold).abs() < 1e-5,
        "test setup: cosine {actual_cos:.6} should equal threshold {threshold:.6}"
    );
    assert!(
        actual_cos >= threshold,
        "boundary: {actual_cos:.6} >= {threshold:.6} must hold"
    );
}
