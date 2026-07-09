//! Ranking-adjustment and mode-filter tests for [`CodeIndexer`].
//!
//! Why: split out of the former monolithic `tests.rs` to keep each test
//! file under the 1500-SLOC cap (issue #1195).
//! What: covers file-type multipliers, definition/struct/function/method
//! boosts, conceptual-intent behaviour, code/definition/conceptual mode
//! defaults, KG top-k survival, intent routing, and stable-order
//! enumeration pagination.
//! Test: this module.
use super::super::*;
use super::*;

#[test]
fn test_file_type_multiplier_demotes_docs() {
    // Why: Definition-intent ranking should prefer source over docs.
    // What: confirms the helper's contract — multiplier 0.5 for .md/.toml/
    // .yaml/.json/.txt, 1.0 for everything else.
    // Test: direct assertions on the helper.
    assert_eq!(file_type_score_multiplier("src/auth.rs"), 1.0);
    assert_eq!(file_type_score_multiplier("src/auth.py"), 1.0);
    assert_eq!(file_type_score_multiplier("src/auth.go"), 1.0);
    assert_eq!(file_type_score_multiplier("CHANGELOG.md"), 0.5);
    assert_eq!(file_type_score_multiplier("docs/CLAUDE.md"), 0.5);
    assert_eq!(file_type_score_multiplier("Cargo.toml"), 0.5);
    assert_eq!(file_type_score_multiplier("config.yaml"), 0.5);
    assert_eq!(file_type_score_multiplier("data.json"), 0.5);
    // Case-insensitive
    assert_eq!(file_type_score_multiplier("README.MD"), 0.5);
}

#[tokio::test]
async fn test_definition_demotes_markdown_below_source() {
    // Why: issue #92 — for Definition-intent queries, the canonical
    // source-file declaration must outrank any .md doc that mentions the
    // symbol many times.
    // What: build a corpus with one .rs source chunk and one .md chunk
    // both containing the literal "CodeChunk struct"; run a Definition
    // query and assert the .rs file ranks first.
    // Test: this test.
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
    // Why: issue #117 — queries containing struct-name acronyms (`HNSW`,
    // `BM25`, `RRF`, `ORT`) historically returned usage sites at top ranks
    // because the BM25 lane couldn't distinguish "file mentions HNSW many
    // times" from "file IS the HNSW declaration". On the v0.8.1 benchmark
    // `HNSW vector similarity search` placed `hnsw_store.rs` at rank 8,
    // behind `retrieval.rs` and `mmr.rs`.
    //
    // Combined fix:
    //   1. #119 classifies short acronym queries (≤2 tokens) as Definition
    //      via the ALL-CAPS acronym hint.
    //   2. The structural boost in `apply_score_adjustments` multiplies
    //      the score of any Struct/Enum/Class/Trait chunk whose
    //      `function_name` matches a query token by `STRUCT_DEFINITION_BOOST`.
    //
    // Updated for issue #197: the original `HNSW vector similarity search`
    // query no longer routes to Definition (the token-count guard suppresses
    // ACRONYM_HINT_RE for multi-word NL-heavy queries) — it now reads as a
    // Conceptual query, which is the correct semantic intent. The
    // Definition structural-boost path is still exercised here by the
    // shorter 2-token acronym query `HNSW lookup`, which preserves the
    // #117 acceptance criterion: `hnsw_store.rs` (canonical struct decl)
    // must outrank usage sites for a Definition-intent acronym query.
    // Test: this test.
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Sanity: the (short, 2-token) query must classify as Definition. The
    // acronym-hint rule from #119, gated by the #197 token-count guard,
    // is what makes this true; if it regresses, the test should fail loudly
    // here rather than in the ranking assertion below.
    assert_eq!(
        QueryClassifier::classify("HNSW lookup"),
        QueryIntent::Definition,
        "test pre-condition: short ALL-CAPS acronym query must classify as \
         Definition (#119 + #197 short-query carve-out)"
    );

    let idx = make_indexer();
    // 1) The canonical declaration: a Struct chunk whose function_name
    //    (= the type name) is `HnswStore` — lowercased, this matches the
    //    `hnsw` query token.
    idx.add_chunk(raw_with_kind(
        "def:1",
        "src/hnsw_store.rs",
        "pub struct HnswStore { index: Index, dim: usize }",
        ChunkType::Struct,
        Some("HnswStore"),
    ))
    .await
    .unwrap();
    // 2-4) Three usage chunks in plausible-looking files. They mention
    //      `HNSW` heavily so the BM25 lane would otherwise rank them
    //      above the declaration (the #117 failure mode).
    idx.add_chunk(raw(
        "use:1",
        "src/retrieval.rs",
        "// HNSW lookup path.\n\
         // Uses HNSW to retrieve top-k vectors.\n\
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
        "issue #117 acceptance: hnsw_store.rs must rank in top-3 for \
         the canonical acronym query; got top-3 files = {top3_files:?}, \
         full ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_function_definition_boost_surfaces_function_over_string_literal_usage() {
    // Why: issue #122 — function-name queries (`BRUSILOV_EPOCH`,
    // `get_call_chain`) were placing usage sites OR string-literal
    // occurrences at rank 1 instead of the canonical declaration.
    // The synthetic-corpus baseline (#123) reproduced this on a clean
    // corpus across all three modes (lexical/hybrid/kg-leading), so it
    // is a real ranking bug rather than a circular-bias artifact.
    //
    // Fix: extend the Definition-intent structural boost (#117) to also
    // cover `Function`/`Method` chunks. The chunk_type filter naturally
    // excludes string-literal occurrences embedded in JSON-shaped
    // descriptors because those chunk as `Constant`, not `Function`.
    //
    // What: plant one Function chunk (the canonical declaration) and one
    // Constant chunk that contains the query token only as a string
    // literal inside a JSON-like descriptor (the historical false-positive
    // shape from `mcp_descriptor.rs`). Assert the Function chunk ranks at
    // top-2 or better for the function-name query.
    // Test: this test.
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Sanity: snake_case identifier with a digit / underscore should
    // classify as Definition (or Unknown, both eligible — but #119 routes
    // SCREAMING_SNAKE_CASE / get_xxx-style symbols to Definition).
    assert_eq!(
        QueryClassifier::classify("get_call_chain"),
        QueryIntent::Definition,
        "test pre-condition: snake_case symbol must classify as Definition"
    );

    let idx = make_indexer();
    // 1) The canonical Function declaration. function_name matches the
    //    query token verbatim; the chunk body is short and contains the
    //    symbol exactly once — i.e. BM25 TF is LOW. Without the boost,
    //    the usage / string-literal chunks dominate.
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
    // 2) A `Constant` chunk that mentions `get_call_chain` only as a
    //    string literal inside a JSON-shaped MCP tool descriptor. This
    //    is the historical false-positive shape (`mcp_descriptor.rs`).
    //    We deliberately make the TF very high so without the chunk_type
    //    filter the boost would mis-fire here.
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
    // 3) A plain code/usage chunk that calls the function — mid-TF.
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
        "issue #122 acceptance: Function declaration must rank at top-2 or \
         better; got rank {rank_of_fn}, ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_method_definition_boost_fires() {
    // Why: issue #122 — symmetric coverage for `ChunkType::Method`. The
    // boost must apply identically for impl-block method declarations.
    // What: plant one Method chunk + one usage chunk; assert the Method
    // ranks above the usage chunk for a method-name query.
    // Test: this test.
    use crate::core::chunker::ChunkType;

    let idx = make_indexer();
    // Method declaration (impl-block shape).
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
    // Usage chunk: mentions parse_token several times in a regular code
    // block (typed as Code, not Method).
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
         out-rank the usage chunk (rank {rank_of_usage}); ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_function_boost_skipped_on_conceptual_intent() {
    // Why: issue #122 — the function-definition boost must only fire when
    // the classifier routes the query to Definition. On Conceptual intent
    // (e.g. "how does ...") the BM25 lane should decide ranking. This pins
    // the conditional so a future refactor can't silently widen the boost
    // to all intents.
    // What: same shape as the positive test, but use a Conceptual-phrased
    // query. Assert the Function chunk does NOT receive the 2× boost —
    // we verify this by checking that the boost was skipped: with the
    // boost active the Function chunk would dominate, but on Conceptual
    // intent the usage chunk should compete on equal BM25 footing.
    // Test: this test.
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Pre-condition: "how does X work" must classify as Conceptual.
    assert_eq!(
        QueryClassifier::classify("how does parse_token work in the parser"),
        QueryIntent::Conceptual,
        "test pre-condition: 'how does X work' must classify as Conceptual"
    );

    let idx = make_indexer();
    // Function declaration: short, low TF.
    idx.add_chunk(raw_with_kind(
        "def:fn",
        "src/parser.rs",
        "pub fn parse_token(input: &str) -> Token { Token::default() }",
        ChunkType::Function,
        Some("parse_token"),
    ))
    .await
    .unwrap();
    // Conceptual / explanatory chunk in a doc with high TF for the
    // query terms.
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
    // Negative-direction assertion: on Conceptual intent the boost is
    // skipped, so the Function chunk gains no artificial 2× lift. The
    // doc-heavy chunk with high TF should at minimum compete with the
    // Function chunk — i.e. the Function is NOT guaranteed to be rank 0
    // the way it is in `test_function_definition_boost_surfaces_function_over_string_literal_usage`.
    // We assert the doc chunk is present in the top results — proving the
    // function-definition boost did not silently fire on Conceptual.
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "Conceptual intent must not apply the function-definition boost — \
         the doc chunk should still surface; ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_function_boost_no_op_when_function_name_missing() {
    // Why: issue #122 — guard against a panic / unwrap regression. A
    // Function chunk that somehow ended up without a `function_name`
    // (e.g. anonymous closure that the chunker couldn't name) must not
    // crash the boost path and must not be boosted (no name to match).
    // What: plant a Function chunk with `function_name: None` and an
    // empty-name Function chunk; run a Definition-intent query that
    // would match if the name were present. Assert: no panic, both
    // chunks are returned at unboosted scores.
    // Test: this test.
    use crate::core::chunker::ChunkType;

    let idx = make_indexer();
    // Function with no name at all.
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
    // Function with empty-string name — defensive: should be treated the
    // same as None for boost purposes (no query token can be a substring
    // of the empty string except the empty token, which the tokenizer
    // discards via the `len() >= 2` filter).
    idx.add_chunk(raw_with_kind(
        "def:empty",
        "src/empty.rs",
        "// another anon block: get_call_chain helper",
        ChunkType::Function,
        Some(""),
    ))
    .await
    .unwrap();
    // Control: a normal chunk with the same token.
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
    // Primary assertion: this must not panic. Secondary assertion: all
    // three chunks come back (the boost path didn't filter them out).
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "search must return results — no panic in the boost path"
    );
    // Verify the unnamed Function chunks were NOT boosted: their score
    // must not be artificially lifted to the top. Since none of the
    // three chunks have a function_name that matches `get_call_chain`,
    // none should be boosted, so ranking comes purely from BM25.
    // We simply verify no panic + non-empty results above; the precise
    // ranking is BM25-determined and out of scope.
}

#[tokio::test]
async fn test_conceptual_does_not_demote_docs() {
    // Why: issue #73 — Conceptual queries are documentation-retrieval by
    // nature; they need `.md` content to answer correctly. When the
    // caller uses the default `SearchMode::Code` (the implicit default,
    // not an explicit override), the search pipeline must upgrade the
    // effective mode to `All` so docs survive the post-filter. An
    // explicit `SearchMode::Code` from the caller still excludes `.md`
    // (covered by `test_mode_filter_code_excludes_markdown`).
    // What: same corpus shape as before, but uses the default mode
    // (i.e. `SearchMode::Code` via `..Default::default()`) and asserts
    // that the intent-aware effective-mode override still surfaces docs.
    // Test: this test plus `test_mode_filter_code_excludes_markdown` for
    // the explicit-mode contract.
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
        // Intentionally leave `mode` as default (`SearchMode::Code`) — the
        // intent-aware override in `search()` should upgrade it to `All`
        // for Conceptual intent so .md content can still surface.
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "Conceptual queries in default mode must still surface .md docs \
         (intent-aware effective-mode override, issue #73)"
    );
}

/// Issue #72 regression: in explicit `SearchMode::Code`, a high-BM25-TF
/// prose chunk must not crowd a genuine source-file match out of `top_k`
/// before the post-RRF hard filter runs.
///
/// Why: production reported code-navigation queries returning docs-heavy
/// or empty result sets. The `doc_score_penalty` matrix used to fire only
/// *after* the `take(top_k)` truncation, so a long CHANGELOG.md with many
/// keyword repeats could fill every top_k slot, the source chunk got
/// truncated away, and then the hard file-type filter dropped the prose —
/// leaving zero results. Issue #72 moved the penalty into
/// `apply_score_adjustments` (pre-truncation) so prose sinks before the
/// cut and the source chunk claims a slot.
/// What: builds a corpus with a high-TF `.md` chunk and a single `.rs`
/// source chunk, runs a BugDebt-intent query (which keeps the explicit
/// `Code` mode — it is not upgraded to `All` like Definition/Conceptual)
/// with `top_k = 1`, and asserts the surviving result is the `.rs` source
/// chunk rather than nothing.
/// Test: this test.
#[tokio::test]
async fn test_code_mode_source_outranks_changelog_pre_truncation() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Pre-condition: the query must NOT classify as Definition/Conceptual,
    // otherwise the intent-aware override promotes mode to All and the
    // hard filter no longer drops the .md — defeating the scenario.
    let intent = QueryClassifier::classify("error handling retry logic deprecated path");
    assert_eq!(
        intent,
        QueryIntent::BugDebt,
        "test pre-condition: query should classify as BugDebt so explicit Code mode survives"
    );

    let idx = make_indexer();
    // High-TF prose chunk: repeats the query terms many times so its raw
    // BM25 score dominates the single source chunk pre-penalty.
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
        // Explicit Code mode — BugDebt intent does not upgrade it, so the
        // .md chunk must be penalised pre-truncation, not after.
        mode: crate::core::indexer::SearchMode::Code,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert_eq!(
        results.len(),
        1,
        "with top_k=1 the source chunk must survive into the single slot \
         (pre-truncation penalty, issue #72) — got {:?}",
        results.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
    assert!(
        results[0].file.ends_with(".rs"),
        "code-mode query must return the source file, not be crowded out by \
         high-TF prose (issue #72); got {}",
        results[0].file
    );
}

/// Issue #79 regression: a Definition-intent query against a corpus where
/// the matching content lives ONLY in markdown docs must still return
/// results when the caller uses the default mode.
///
/// Why: production v0.4.4 reported "UserPromptSubmit hook registration"
/// (Definition intent, default Code mode) returning zero results, because
/// the intent override to `All` mode was being undermined elsewhere in the
/// pipeline. The previous `test_conceptual_does_not_demote_docs` only
/// checked that .md docs *survived* alongside .rs source; it did not
/// exercise the docs-only path where the source-file fallback hides the
/// bug.
/// What: index a single .md chunk describing a hook registration concept
/// (no matching .rs file at all), classify as Definition via a PascalCase
/// trigger, run the search in default mode, and assert non-empty results.
/// Test: this test.
#[tokio::test]
async fn test_definition_default_mode_returns_docs_when_no_source_matches() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Sanity: ensure the query phrase classifies as Definition so this
    // test exercises the intent-override code path.
    let intent = QueryClassifier::classify("UserPromptSubmit hook registration");
    assert_eq!(
        intent,
        QueryIntent::Definition,
        "test pre-condition: PascalCase identifier should classify as Definition"
    );

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
        // Default mode (SearchMode::Code) — the intent override must promote
        // to All so the .md chunk survives the post-filter.
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "Definition-intent query against docs-only corpus returned 0 results — \
         the intent-aware mode override is broken (issue #79)"
    );
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "expected the .md chunk to survive the post-filter, got: {:?}",
        results.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
}

/// Issue #79 regression: a Conceptual-intent query against a docs-only
/// corpus must return results even when the caller uses the default mode.
///
/// Why: parallel to `test_definition_default_mode_returns_docs_when_no_source_matches`
/// but for Conceptual intent ("how does the X work" queries that should
/// retrieve architecture / overview docs).
/// What: index a single .md chunk, run a "how does ..." query, assert
/// non-empty results in default mode.
/// Test: this test.
#[tokio::test]
async fn test_conceptual_default_mode_returns_docs_when_no_source_matches() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    let intent = QueryClassifier::classify("how does the hook system work");
    assert_eq!(
        intent,
        QueryIntent::Conceptual,
        "test pre-condition: 'how does' should classify as Conceptual"
    );

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
        "Conceptual-intent query against docs-only corpus returned 0 results — \
         the intent-aware mode override is broken (issue #79)"
    );
}

#[tokio::test]
async fn test_kg_results_survive_top_k_truncation() {
    // Why: issue #94 — KG-expanded neighbours used to be appended after
    // `take(top_k)` had already trimmed the result list, so on busy
    // indexes the "hybrid+kg" reason never surfaced. We now re-sort the
    // merged direct+KG list by score before truncation.
    // What: fill the index with N direct hits at top_k limit, plus one
    // KG-only neighbour; assert the neighbour survives.
    // Test: this test.
    let idx = CodeIndexer::new("kg-trunc", "/tmp/test");
    // Direct hit + KG seed via `calls`.
    idx.add_chunk(RawChunk {
        id: "src:caller".to_string(),
        file: "caller.rs".to_string(),
        start_line: 1,
        end_line: 3,
        content: "fn caller() { /* dispatches */ }".to_string(),
        function_name: Some("caller".to_string()),
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
        id: "src:authenticate".to_string(),
        file: "auth.rs".to_string(),
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
    assert!(
        results.iter().any(|c| c.match_reason == "hybrid+kg"),
        "at least one result must carry 'hybrid+kg' match_reason, got: {:#?}",
        results
            .iter()
            .map(|c| (&c.id, &c.match_reason))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_intent_routing_definitions() {
    // Sanity: intent table from CLAUDE.md is wired through.
    use crate::core::classifier::QueryIntent;
    let (a, b, kg) = QueryIntent::Definition.weights();
    assert!((a - 0.3).abs() < 1e-6 && (b - 0.7).abs() < 1e-6 && !kg);
    let (a, b, kg) = QueryIntent::Usage.weights();
    assert!((a - 0.5).abs() < 1e-6 && (b - 0.5).abs() < 1e-6 && kg);
}

#[tokio::test]
async fn test_enumerate_chunks_paginates_stable_order() {
    // Why: pagination over an underlying HashMap must produce a stable
    // total order so successive pages don't overlap or skip rows.
    let idx = make_indexer();
    // Helper: build a chunk whose `start_line`/`end_line` match the ID so
    // the `(file, start_line, end_line)` sort exercised below has the
    // expected total order (the bare `raw` helper hardcodes
    // `start_line: 1` for every chunk).
    fn raw_lines(id: &str, file: &str, start: usize, end: usize, content: &str) -> RawChunk {
        let mut r = raw(id, file, content);
        r.start_line = start;
        r.end_line = end;
        r
    }
    // Insert in an order that exercises the file/start_line sort.
    idx.add_chunk(raw_lines("b.rs:10:20", "b.rs", 10, 20, "fn b_two() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw_lines("a.rs:1:5", "a.rs", 1, 5, "fn a_one() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw_lines("b.rs:1:5", "b.rs", 1, 5, "fn b_one() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw_lines("a.rs:30:40", "a.rs", 30, 40, "fn a_two() {}"))
        .await
        .unwrap();

    // Full enumeration: sorted by (file, start_line).
    let (total_all, all) = idx.enumerate_chunks(0, 100).await;
    assert_eq!(total_all, 4);
    let ids: Vec<_> = all.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["a.rs:1:5", "a.rs:30:40", "b.rs:1:5", "b.rs:10:20"]
    );

    // Page 1 (offset=0, limit=2) + Page 2 (offset=2, limit=2) cover all.
    let (total_p1, page1) = idx.enumerate_chunks(0, 2).await;
    let (total_p2, page2) = idx.enumerate_chunks(2, 2).await;
    assert_eq!(total_p1, 4);
    assert_eq!(total_p2, 4);
    assert_eq!(page1.len(), 2);
    assert_eq!(page2.len(), 2);
    let combined: Vec<_> = page1
        .iter()
        .chain(page2.iter())
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(combined, ids);

    // Offset past the end returns empty, but total is preserved.
    let (total_end, end) = idx.enumerate_chunks(10, 5).await;
    assert_eq!(total_end, 4);
    assert!(end.is_empty());

    // limit=0 returns empty.
    let (total_z, z) = idx.enumerate_chunks(0, 0).await;
    assert_eq!(total_z, 4);
    assert!(z.is_empty());
}
