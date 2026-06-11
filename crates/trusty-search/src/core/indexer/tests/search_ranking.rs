/// Ranking, scoring boost, intent routing, and branch-boost tests for the indexer.
use super::*;

#[test]
fn test_file_type_multiplier_demotes_docs() {
    assert_eq!(file_type_score_multiplier("src/auth.rs"), 1.0);
    assert_eq!(file_type_score_multiplier("src/auth.py"), 1.0);
    assert_eq!(file_type_score_multiplier("src/auth.go"), 1.0);
    assert_eq!(file_type_score_multiplier("CHANGELOG.md"), 0.5);
    assert_eq!(file_type_score_multiplier("docs/CLAUDE.md"), 0.5);
    assert_eq!(file_type_score_multiplier("Cargo.toml"), 0.5);
    assert_eq!(file_type_score_multiplier("config.yaml"), 0.5);
    assert_eq!(file_type_score_multiplier("data.json"), 0.5);
    assert_eq!(file_type_score_multiplier("README.MD"), 0.5);
}

#[tokio::test]
async fn test_definition_demotes_markdown_below_source() {
    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "CHANGELOG.md",
        "## CodeChunk struct\nCodeChunk struct fields: id, file. CodeChunk struct fields are stable.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:1",
        "src/indexer.rs",
        "pub struct CodeChunk { pub id: String, pub file: String }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "struct CodeChunk fields".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must return results");
    assert!(
        results[0].file.ends_with(".rs"),
        "Definition intent must rank source over docs, top result file = {}",
        results[0].file
    );
}

#[tokio::test]
async fn test_struct_definition_boost_surfaces_struct_over_usage() {
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    assert_eq!(
        QueryClassifier::classify("HNSW lookup"),
        QueryIntent::Definition,
        "test pre-condition: short ALL-CAPS acronym query must classify as Definition"
    );

    let idx = make_indexer();
    idx.add_chunk(raw_with_kind(
        "def:1",
        "src/hnsw_store.rs",
        "pub struct HnswStore { index: Index, dim: usize }",
        ChunkType::Struct,
        Some("HnswStore"),
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:1",
        "src/retrieval.rs",
        "// HNSW lookup path.\n// Uses HNSW to retrieve top-k vectors.\n\
         // HNSW lookup HNSW lookup HNSW HNSW HNSW HNSW HNSW HNSW HNSW HNSW",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:2",
        "src/mmr.rs",
        "// MMR diversity reranker over HNSW lookup results.\n\
         // HNSW HNSW HNSW lookup lookup lookup HNSW HNSW HNSW",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:3",
        "src/search.rs",
        "// Top-level hybrid search: BM25 lane + HNSW lookup lane.\n\
         // HNSW HNSW HNSW lookup lookup HNSW HNSW lookup HNSW",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "HNSW lookup".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must return results");
    let top3_files: Vec<String> = results.iter().take(3).map(|c| c.file.clone()).collect();
    let hnsw_abs = abs("src/hnsw_store.rs");
    assert!(
        top3_files.contains(&hnsw_abs),
        "issue #117 acceptance: hnsw_store.rs must rank in top-3; got {top3_files:?}"
    );
}

#[tokio::test]
async fn test_function_definition_boost_surfaces_function_over_string_literal_usage() {
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    assert_eq!(
        QueryClassifier::classify("get_call_chain"),
        QueryIntent::Definition,
        "test pre-condition: snake_case symbol must classify as Definition"
    );

    let idx = make_indexer();
    idx.add_chunk(raw_with_kind(
        "def:fn",
        "src/call_chain.rs",
        "pub fn get_call_chain(symbol: &str) -> Vec<String> {\n    \
         vec![symbol.to_string()]\n}",
        ChunkType::Function,
        Some("get_call_chain"),
    ))
    .await
    .unwrap();
    idx.add_chunk(raw_with_kind(
        "use:descriptor",
        "src/mcp_descriptor.rs",
        "const TOOL: &str = r#\"{ \"name\": \"get_call_chain\", \
         \"description\": \"get_call_chain helper get_call_chain tool \
         get_call_chain get_call_chain get_call_chain\" }\"#;",
        ChunkType::Constant,
        Some("TOOL"),
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:call",
        "src/caller.rs",
        "let chain = get_call_chain(\"foo\"); \
         // get_call_chain returns the call chain; get_call_chain is a helper.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "get_call_chain".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must return results");
    let rank_of_fn = results
        .iter()
        .position(|c| c.file == abs("src/call_chain.rs"))
        .expect("Function declaration must be in results");
    assert!(
        rank_of_fn < 2,
        "issue #122 acceptance: Function declaration must rank at top-2; got rank {rank_of_fn}"
    );
}

#[tokio::test]
async fn test_method_definition_boost_fires() {
    use crate::core::chunker::ChunkType;

    let idx = make_indexer();
    idx.add_chunk(raw_with_kind(
        "def:method",
        "src/parser.rs",
        "impl Parser {\n    \
         pub fn parse_token(&self, input: &str) -> Token { Token::default() }\n}",
        ChunkType::Method,
        Some("parse_token"),
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:method",
        "src/driver.rs",
        "// driver calls parse_token; parse_token returns a Token. parse_token \
         parse_token parse_token parse_token.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "parse_token".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let rank_of_method = results
        .iter()
        .position(|c| c.file == abs("src/parser.rs"))
        .expect("Method declaration must be in results");
    let rank_of_usage = results
        .iter()
        .position(|c| c.file == abs("src/driver.rs"))
        .expect("Usage chunk must be in results");
    assert!(
        rank_of_method < rank_of_usage,
        "issue #122: Method declaration (rank {rank_of_method}) must \
         out-rank the usage chunk (rank {rank_of_usage})"
    );
}

#[tokio::test]
async fn test_function_boost_skipped_on_conceptual_intent() {
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    assert_eq!(
        QueryClassifier::classify("how does parse_token work in the parser"),
        QueryIntent::Conceptual,
        "test pre-condition: 'how does X work' must classify as Conceptual"
    );

    let idx = make_indexer();
    idx.add_chunk(raw_with_kind(
        "def:fn",
        "src/parser.rs",
        "pub fn parse_token(input: &str) -> Token { Token::default() }",
        ChunkType::Function,
        Some("parse_token"),
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "doc:1",
        "docs/ARCHITECTURE.md",
        "How does parse_token work? parse_token in the parser tokenises input \
         strings into Token values. parse_token parse_token parser parser \
         tokenise tokenise.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "how does parse_token work in the parser".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "Conceptual intent must not apply the function-definition boost; ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_function_boost_no_op_when_function_name_missing() {
    use crate::core::chunker::ChunkType;

    let idx = make_indexer();
    idx.add_chunk(raw_with_kind(
        "def:noname",
        "src/anon.rs",
        "// anonymous body referencing get_call_chain\n\
         get_call_chain(\"x\");",
        ChunkType::Function,
        None,
    ))
    .await
    .unwrap();
    idx.add_chunk(raw_with_kind(
        "def:empty",
        "src/empty.rs",
        "// another anon block: get_call_chain helper",
        ChunkType::Function,
        Some(""),
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:1",
        "src/use.rs",
        "let r = get_call_chain(\"foo\");",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "get_call_chain".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "search must return results — no panic in the boost path"
    );
}

#[tokio::test]
async fn test_conceptual_does_not_demote_docs() {
    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "ARCHITECTURE.md",
        "How does the CodeChunk pipeline work in trusty-search.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:1",
        "src/indexer.rs",
        "pub struct CodeChunk { pub id: String }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "how does the CodeChunk pipeline work".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "Conceptual queries in default mode must still surface .md docs (issue #73)"
    );
}

#[tokio::test]
async fn test_code_mode_source_outranks_changelog_pre_truncation() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    let intent = QueryClassifier::classify("error handling retry logic deprecated path");
    assert_eq!(
        intent,
        QueryIntent::BugDebt,
        "test pre-condition: query should classify as BugDebt"
    );

    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "CHANGELOG.md",
        "error handling error handling error handling retry logic retry logic \
         deprecated path deprecated path error handling retry logic deprecated \
         error handling retry logic deprecated path error handling retry logic",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:1",
        "src/retry.rs",
        "fn handle_error_with_retry() { /* error handling + retry logic, deprecated path */ }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "error handling retry logic deprecated path".to_string(),
        top_k: 1,
        expand_graph: false,
        compact: false,
        mode: crate::core::indexer::SearchMode::Code,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert_eq!(results.len(), 1, "source chunk must survive top_k=1");
    assert!(
        results[0].file.ends_with(".rs"),
        "code-mode query must return the source file (issue #72); got {}",
        results[0].file
    );
}

#[tokio::test]
async fn test_definition_default_mode_returns_docs_when_no_source_matches() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    let intent = QueryClassifier::classify("UserPromptSubmit hook registration");
    assert_eq!(intent, QueryIntent::Definition);

    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "docs/HOOKS.md",
        "# UserPromptSubmit hook registration\n\
         The UserPromptSubmit hook fires whenever the user submits a prompt. \
         Register your hook handler via the registration API to receive these events.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "UserPromptSubmit hook registration".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "Definition-intent query against docs-only corpus returned 0 results (issue #79)"
    );
    assert!(results.iter().any(|c| c.file.ends_with(".md")));
}

#[tokio::test]
async fn test_conceptual_default_mode_returns_docs_when_no_source_matches() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    let intent = QueryClassifier::classify("how does the hook system work");
    assert_eq!(intent, QueryIntent::Conceptual);

    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "docs/ARCHITECTURE.md",
        "## How the hook system works\n\
         The hook system dispatches events to registered handlers in priority order.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "how does the hook system work".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "Conceptual-intent query against docs-only corpus returned 0 results (issue #79)"
    );
}

// ---- Branch-aware search (issue #122) ----------------------------------

#[tokio::test]
async fn test_branch_boost_applied_to_matching_chunks() {
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/on.rs:1:1",
        "src/on.rs",
        "fn authenticate(user: &str) -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/off.rs:1:1",
        "src/off.rs",
        "fn authenticate(user: &str) -> bool { true }",
    ))
    .await
    .unwrap();

    let q = make_branch_query("fn authenticate", vec!["src/on.rs".to_string()], 1.5);
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "branch-aware search must return hits");
    let on_branch = results
        .iter()
        .find(|c| c.file == abs("src/on.rs"))
        .expect("on-branch chunk in results");
    let off_branch = results.iter().find(|c| c.file == abs("src/off.rs"));

    assert!(on_branch.on_branch, "on_branch must be true for on.rs");
    if let Some(off) = off_branch {
        assert!(!off.on_branch, "on_branch must be false for off.rs");
        assert!(on_branch.score >= off.score);
    }
    assert_eq!(results[0].file, abs("src/on.rs"));
}

#[tokio::test]
async fn test_branch_boost_clamped_to_3x() {
    let q = make_branch_query("foo", vec!["src/on.rs".to_string()], 10.0);
    let root = std::path::PathBuf::from("/tmp/test");
    let (set, boost) = super::search::resolve_branch_set(&q, &root);
    assert!(set.is_some(), "branch set must be present");
    assert!(
        (boost - 3.0).abs() < f32::EPSILON,
        "branch_boost=10.0 must clamp to 3.0, got {boost}"
    );

    let q_low = make_branch_query("foo", vec!["src/on.rs".to_string()], 0.0);
    let (set_low, boost_low) = super::search::resolve_branch_set(&q_low, &root);
    assert!(
        (boost_low - 1.0).abs() < f32::EPSILON,
        "branch_boost=0.0 must clamp to 1.0, got {boost_low}"
    );
    assert!(set_low.is_none(), "branch_boost=1.0 must drop the set");
}

#[tokio::test]
async fn test_on_branch_set_correctly() {
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/on.rs:1:1",
        "src/on.rs",
        "fn authenticate() -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/off.rs:1:1",
        "src/off.rs",
        "fn authenticate() -> bool { true }",
    ))
    .await
    .unwrap();

    let q = make_branch_query("fn authenticate", vec!["src/on.rs".to_string()], 1.5);
    let results = idx.search(&q).await.unwrap();
    let on_abs = abs("src/on.rs");
    let off_abs = abs("src/off.rs");
    for c in &results {
        if c.file == on_abs {
            assert!(c.on_branch, "on.rs must be flagged on_branch=true");
        } else if c.file == off_abs {
            assert!(!c.on_branch, "off.rs must be flagged on_branch=false");
        }
    }

    let q2 = make_branch_query("fn authenticate", vec!["./src/on.rs".to_string()], 1.5);
    let results2 = idx.search(&q2).await.unwrap();
    let on2 = results2
        .iter()
        .find(|c| c.file == on_abs)
        .expect("on-branch chunk in results");
    assert!(on2.on_branch, "leading './' must be normalized away");
}

#[tokio::test]
async fn test_no_boost_when_branch_files_absent() {
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/auth.rs:1:5",
        "src/auth.rs",
        "fn authenticate(user: &str, password: &str) -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/render.rs:1:3",
        "src/render.rs",
        "fn render_ui_components() { /* svelte */ }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "fn authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: SearchQuery::default_branch_boost(),
        branch: None,
        mode: SearchMode::Code,
        exclude_archived: false,
        stage: None,
        refine_query: None,
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty());
    for c in &results {
        assert!(
            !c.on_branch,
            "on_branch must default to false when no branch context provided"
        );
    }
}
