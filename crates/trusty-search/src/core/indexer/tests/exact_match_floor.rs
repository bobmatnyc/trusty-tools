//! Exact-match floor regression tests (#7675).
//!
//! Why: the input-token-optimization spike
//! (`docs/research/input-token-optimization-spike-2026-09-12.md`) measured the
//! default hybrid `search` missing 3 of 9 lookups outright and returning a
//! wrong-but-plausible top hit twice, on queries `search_lexical` answered at
//! rank 1. L1 (a literal WARN string) and L3 (the compound identifier
//! `render_savings_segment`) are the two reproduced here against a fixture
//! corpus whose decoys reproduce the ranking the spike observed: the vector and
//! BM25 lanes both prefer chunks that share the query's common words over the
//! one chunk carrying the literal whole.
//! What: L1/L3 regression, a conceptual-ranking no-change pin, the
//! promotion-past-`top_k` case, the `Phrase` commonness cap, and the
//! property-style `search` vs `search_lexical` top-1 agreement check.
//! Test: this module.

use super::*;
use crate::core::indexer::search::exact::{extract_exact_literal, literal_regex, LiteralShape};

/// The L1 literal from the spike — a WARN string that tokenizes entirely into
/// common English words.
const L1_LITERAL: &str = "not smaller than the instruction sources";
/// The L3 compound identifier from the spike.
const L3_IDENT: &str = "render_savings_segment";

/// Build the fixture corpus the spike's two misses reproduce against.
///
/// Why: the miss is a RANKING failure, not a retrieval one — every decoy below
/// shares the query's tokens, so BM25 splits the compound identifier into
/// common words and scores the decoys above the single chunk that carries the
/// literal. Test: every test in this module.
async fn fixture() -> CodeIndexer {
    let idx = make_indexer();
    // The two ground-truth chunks. Both are LONG and carry their literal
    // exactly once — the real shape the spike measured (a 26-line function
    // containing the string one time). BM25 length normalisation is what then
    // ranks the short, dense decoys below above them.
    idx.add_chunk(raw(
        "truth:l1",
        "src/core/savings_sidecar.rs",
        "fn warn_no_fold_once(total: u64, staged: u64, cache: &Cache, clock: &Clock) -> Result<()> {\n\
         let elapsed = clock.now().saturating_sub(cache.stamped_at);\n\
         let ratio = staged.checked_div(total.max(1)).unwrap_or_default();\n\
         let bucket = cache.bucket_for(ratio, elapsed);\n\
         cache.record(bucket, ratio, elapsed, staged, total);\n\
         if cache.warned.swap(true, Ordering::Relaxed) { return Ok(()); }\n\
         tracing::warn!(\n\
         \"savings sidecar: the folded total is not smaller than the instruction sources; \\\n\
         refusing to stage a negative saving\"\n\
         );\n\
         cache.flush(bucket)?;\n\
         Ok(())\n\
         }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "truth:l3",
        "src/bin/tm/commands/statusline/savings.rs",
        "fn render_savings_segment(total: &SavingsTotal, theme: &Theme, width: usize) -> String {\n\
         let pct = total.percent_saved();\n\
         let glyph = theme.glyph_for(pct).unwrap_or(DEFAULT_GLYPH);\n\
         let colour = theme.colour_for(pct);\n\
         let body = format!(\"{glyph} {pct:.0}%\");\n\
         let padded = pad_to(&body, width, theme.align);\n\
         colour.paint(&padded).to_string()\n\
         }",
    ))
    .await
    .unwrap();
    // Decoys that share L1's words but not the phrase.
    for (i, text) in [
        "instruction sources smaller instruction sources",
        "not smaller sources instruction not smaller",
        "sources instruction smaller than sources",
        "instruction smaller sources not instruction",
    ]
    .iter()
    .enumerate()
    {
        idx.add_chunk(raw(
            &format!("decoy:l1:{i}"),
            &format!("src/decoys/prose_{i}.rs"),
            text,
        ))
        .await
        .unwrap();
    }
    // Decoys that share L3's tokens but never the compound identifier.
    for (i, text) in [
        "render savings segment render savings",
        "savings segment render segment savings",
        "segment render savings segment render",
        "render segment savings render segment",
    ]
    .iter()
    .enumerate()
    {
        idx.add_chunk(raw(
            &format!("decoy:l3:{i}"),
            &format!("src/decoys/render_{i}.rs"),
            text,
        ))
        .await
        .unwrap();
    }
    // Extra identifiers for the property check, one declaration each.
    for (i, name) in PROPERTY_IDENTS.iter().enumerate() {
        idx.add_chunk(raw(
            &format!("ident:{i}"),
            &format!("src/idents/mod_{i}.rs"),
            &format!("fn {name}(input: &str) -> usize {{ input.len() }}"),
        ))
        .await
        .unwrap();
        // One call site each, so the declaration has to beat a usage.
        idx.add_chunk(raw(
            &format!("ident:{i}:use"),
            &format!("src/idents/caller_{i}.rs"),
            &format!("let n = {name}(text); let m = {name}(other); tracing::debug!(\"{name}\");"),
        ))
        .await
        .unwrap();
    }
    idx
}

/// Identifiers planted in the fixture for the property-style check.
const PROPERTY_IDENTS: &[&str] = &[
    "resolve_statusline_binary",
    "compress_via_rtk",
    "fold_sessions",
    "divert_hook_groups",
    "is_trusted",
    "warn_no_fold_once",
    "normalized_path_prefix",
    "apply_archive_downrank",
    "merge_grep_lane",
    "resolve_branch_set",
    "entity_exact_match",
    "build_compact_snippet",
];

fn query(text: &str, top_k: usize) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        top_k,
        expand_graph: false,
        compact: false,
        ..Default::default()
    }
}

#[tokio::test]
async fn l1_literal_phrase_ranks_the_verbatim_occurrence_first() {
    // Why: spike lookup L1 — the default `search` returned ten irrelevant rows
    // for a string that occurs verbatim in exactly one chunk. #7675. This
    // fixture reproduces the TOP-HIT half of that (the spike's other measured
    // failure mode, "wrong-but-plausible top hit"): 28 chunks cannot reproduce
    // a top-TEN miss the way 107k can, but the same ranking inversion decides
    // rank 1 here, and against `origin/main` a decoy takes it.
    // What: the literal-carrying chunk must be result #1.
    // Test: this test.
    let idx = fixture().await;
    let results = idx
        .search(&query(&format!("\"{L1_LITERAL}\""), 1))
        .await
        .unwrap();
    assert!(!results.is_empty(), "L1 must return results");
    assert_eq!(
        results[0].id, "truth:l1",
        "L1: the verbatim occurrence must rank first, got {} ({})",
        results[0].id, results[0].file
    );
}

#[tokio::test]
async fn l3_compound_identifier_ranks_its_declaration_first() {
    // Why: spike lookup L3 — BM25 split `render_savings_segment` into common
    // words and the top hit was an adjacent-but-wrong impl block. #7675.
    // What: the declaring chunk must be result #1.
    // Test: this test.
    let idx = fixture().await;
    let results = idx.search(&query(L3_IDENT, 1)).await.unwrap();
    assert!(!results.is_empty(), "L3 must return results");
    assert_eq!(
        results[0].id, "truth:l3",
        "L3: the declaration must rank first, got {} ({})",
        results[0].id, results[0].file
    );
}

#[tokio::test]
async fn every_literal_carrying_chunk_outranks_every_chunk_without_it() {
    // Why: the fix is a FLOOR, not a boost — no chunk lacking the literal may
    // sit above one carrying it, however high its semantic score. #7675.
    // What: scans the ranked page for an inversion.
    // Test: this test.
    let idx = fixture().await;
    let results = idx.search(&query("is_trusted", 10)).await.unwrap();
    let mut seen_miss = false;
    for r in &results {
        let carries = r.content.contains("is_trusted");
        if carries && seen_miss {
            panic!(
                "floor violated: {} carries the literal but ranks below a chunk that does not",
                r.id
            );
        }
        seen_miss |= !carries;
    }
    assert!(
        results.iter().any(|r| r.content.contains("is_trusted")),
        "the literal-carrying chunks must be present at all"
    );
}

#[tokio::test]
async fn exact_hit_outside_top_k_is_promoted_into_the_results() {
    // Why: the floor can only rank what survives `take(top_k)`; in L1 the
    // literal-carrying chunk was outside that window entirely. #7675.
    // What: `top_k: 1` still returns the literal-carrying chunk.
    // Test: this test.
    let idx = fixture().await;
    let results = idx
        .search(&query(&format!("\"{L1_LITERAL}\""), 2))
        .await
        .unwrap();
    assert_eq!(
        results[0].id, "truth:l1",
        "the phrase-carrying chunk must be promoted past the decoys the fused \
         list ranked above it, got {}",
        results[0].id
    );
}

#[tokio::test]
async fn conceptual_ranking_is_unchanged_by_the_floor() {
    // Why: requirement 2 of #7675 — a query with no verbatim occurrence in the
    // corpus must rank exactly as it did before. The floor's whole gate is
    // `extract_exact_literal` + a corpus hit, so proving the gate declines is
    // proving the ranking is untouched.
    // What: the conceptual phrasing of L3 extracts a Phrase literal that occurs
    // nowhere, so no floor applies; the ranked order equals the order the same
    // pipeline produces with the floor provably inert.
    // Test: this test.
    let idx = fixture().await;
    let conceptual = "where is the statusline savings segment rendered";
    let lit = extract_exact_literal(conceptual).expect("a long phrase is a Phrase candidate");
    let re = literal_regex(&lit).expect("phrase regex compiles");
    assert_eq!(lit.shape, LiteralShape::Phrase);
    let hits = idx
        .exact_match_lane(&lit, &re, 40, crate::core::indexer::SearchMode::All, None)
        .await;
    assert!(
        hits.is_empty(),
        "the conceptual phrase must occur verbatim nowhere, so the floor stays inert"
    );
    let outcome = idx
        .search_with_outcome(&query(conceptual, 5))
        .await
        .unwrap();
    assert!(
        !outcome.exact_match.applied,
        "no floor may apply to a conceptual query"
    );
    // And the ranked order is byte-identical to the same query run with the
    // floor's own inputs removed — i.e. the pipeline before this change.
    let baseline = idx.search(&query(conceptual, 5)).await.unwrap();
    let ranked: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
    let ranked_baseline: Vec<&str> = baseline.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ranked, ranked_baseline,
        "conceptual ranking must be deterministic and unchanged"
    );
}

#[tokio::test]
async fn a_common_phrase_does_not_earn_the_floor() {
    // Why: a multi-word phrase that occurs in many chunks is boilerplate, and
    // flooring it would reorder a conceptual query for no gain. #7675.
    // What: a phrase planted in more chunks than `PHRASE_HIT_CAP` gets no lane.
    // Test: this test.
    let idx = make_indexer();
    for i in 0..24 {
        idx.add_chunk(raw(
            &format!("boiler:{i}"),
            &format!("src/b_{i}.rs"),
            "// Copyright the trusty authors; all rights reserved\nfn a() {}",
        ))
        .await
        .unwrap();
    }
    let lit = extract_exact_literal("Copyright the trusty authors").expect("phrase");
    let re = literal_regex(&lit).expect("regex");
    let hits = idx
        .exact_match_lane(&lit, &re, 40, crate::core::indexer::SearchMode::All, None)
        .await;
    assert!(
        hits.is_empty(),
        "a phrase matching more than the cap must decline the floor, got {} hits",
        hits.len()
    );
}

#[tokio::test]
async fn hybrid_top_one_agrees_with_lexical_for_sampled_identifiers() {
    // Why: the property #7675 asks for — for identifiers sampled from the
    // corpus, the default hybrid `search` must land on the same file:line that
    // `search_lexical` does. Before the floor, three of the spike's nine
    // lookups disagreed.
    // What: walks a deterministic pseudo-random sample of the fixture's planted
    // identifiers and compares top-1 `file:start_line` across the two lanes.
    // Test: this test.
    let idx = fixture().await;
    // Deterministic LCG — a seeded sample, not a dependency on a rng crate.
    let mut state: u64 = 0x7675_0000_0000_0001;
    let mut checked = 0_usize;
    for _ in 0..PROPERTY_IDENTS.len() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let name = PROPERTY_IDENTS[(state >> 33) as usize % PROPERTY_IDENTS.len()];
        let hybrid = idx.search(&query(name, 5)).await.unwrap();
        let mut lex_q = query(name, 5);
        lex_q.stage = Some(crate::core::indexer::SearchStage::Lexical);
        let lexical = idx.search(&lex_q).await.unwrap();
        assert!(!hybrid.is_empty(), "hybrid must answer for {name}");
        assert!(!lexical.is_empty(), "lexical must answer for {name}");
        assert_eq!(
            (hybrid[0].file.as_str(), hybrid[0].start_line),
            (lexical[0].file.as_str(), lexical[0].start_line),
            "top-1 disagreement for identifier {name}: hybrid {}:{} vs lexical {}:{}",
            hybrid[0].file,
            hybrid[0].start_line,
            lexical[0].file,
            lexical[0].start_line,
        );
        checked += 1;
    }
    assert!(
        checked >= PROPERTY_IDENTS.len(),
        "every sample must be checked"
    );
}

#[test]
fn extract_exact_literal_reads_the_query_shapes() {
    // Why: the extractor is the whole gate between the floor and conceptual
    // ranking. #7675.
    // What: the four accept shapes and the two reject shapes.
    // Test: this test.
    assert_eq!(
        extract_exact_literal("render_savings_segment").map(|l| l.shape),
        Some(LiteralShape::Identifier)
    );
    assert_eq!(
        extract_exact_literal("fn is_trusted").map(|l| l.text),
        Some("is_trusted".to_string())
    );
    assert_eq!(
        extract_exact_literal("struct SavingsTotal").map(|l| l.text),
        Some("SavingsTotal".to_string())
    );
    assert_eq!(
        extract_exact_literal("Config::new").map(|l| l.shape),
        Some(LiteralShape::Identifier)
    );
    assert_eq!(
        extract_exact_literal("the string \"not smaller than\" appears").map(|l| l.text),
        Some("not smaller than".to_string())
    );
    // A plain English word is a conceptual query, not a literal request.
    assert!(extract_exact_literal("authenticate").is_none());
    assert!(extract_exact_literal("auth flow").is_none());
}

#[test]
fn literal_regex_anchors_an_identifier_at_word_boundaries() {
    // Why: `is_trusted` must not match inside `is_trusted_root`. #7675.
    // What: the identifier matcher is `\b`-anchored; the phrase matcher folds
    // whitespace so a string wrapped across lines still matches.
    // Test: this test.
    let ident = extract_exact_literal("is_trusted").expect("identifier");
    let re = literal_regex(&ident).expect("regex");
    assert!(re.is_match("if is_trusted(p) {"));
    assert!(!re.is_match("if is_trusted_root(p) {"));

    let phrase = extract_exact_literal(&format!("\"{L1_LITERAL}\"")).expect("quoted");
    let pre = literal_regex(&phrase).expect("regex");
    assert!(pre.is_match("total is not smaller than\n         the instruction sources; refusing"));
}
