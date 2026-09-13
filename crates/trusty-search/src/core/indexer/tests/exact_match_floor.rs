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
//! What: L1/L3 regression, a conceptual-ranking no-change pin (including a
//! prose sentence that occurs verbatim and still must not be floored), the
//! promotion-past-`top_k` case, the filename and issue-reference shapes, the
//! archived-duplicate ordering, the postings-vs-scan agreement, and the
//! property-style `search` vs `search_lexical` top-1 agreement check.
//! Test: this module.

use super::*;
use crate::core::indexer::search::exact::{
    extract_exact_literal, literal_regex, LiteralShape, FILENAME_HIT_CAP,
};

/// The L1 literal from the spike — a WARN string that tokenizes entirely into
/// common English words.
const L1_LITERAL: &str = "not smaller than the instruction sources";
/// The L3 compound identifier from the spike.
const L3_IDENT: &str = "render_savings_segment";
/// A ≥3-token prose sentence that occurs VERBATIM in exactly one doc chunk.
///
/// Why: the round-2 HIGH finding — an unquoted phrase used to earn the floor,
/// so a sentence like this one took the top slot from every chunk the semantic
/// lane judged relevant. It must now extract no literal at all. #7675.
const CONCEPTUAL_PROSE: &str = "reclaims idle caches on a bounded ticker";

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
    // A prose doc chunk carrying CONCEPTUAL_PROSE verbatim. #7675 round 2: an
    // unquoted multi-word phrase must not float this chunk over the code the
    // semantic lane judged relevant, however exactly the sentence matches.
    idx.add_chunk(raw(
        "doc:prose",
        "docs/notes/rollout.md",
        "## Rollout notes\n\nThe daemon reclaims idle caches on a bounded ticker \
         so a quiet host shrinks back to its durable baseline.\n",
    ))
    .await
    .unwrap();
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
    // What: two conceptual queries — one whose words occur only scattered, and
    // one (`CONCEPTUAL_PROSE`) that occurs VERBATIM in a prose doc chunk. The
    // second is the round-2 regression: an unquoted phrase must extract no
    // literal, so the doc chunk cannot be floored over the code the semantic
    // lane picked. Both ranked orders equal the order the same pipeline
    // produces with the floor provably inert.
    // Test: this test.
    let idx = fixture().await;
    for conceptual in [
        "where is the statusline savings segment rendered",
        CONCEPTUAL_PROSE,
    ] {
        assert!(
            extract_exact_literal(conceptual).is_none(),
            "an unquoted multi-word query must extract no literal: {conceptual}"
        );
        let outcome = idx
            .search_with_outcome(&query(conceptual, 5))
            .await
            .unwrap();
        assert!(
            !outcome.exact_match.applied,
            "no floor may apply to a conceptual query: {conceptual}"
        );
        assert!(
            outcome.exact_match.literal.is_none(),
            "a conceptual query names no literal: {conceptual}"
        );
        assert!(
            !outcome.results.is_empty(),
            "the conceptual query must still answer: {conceptual}"
        );
        // And the ranked order is byte-identical to the same query run with the
        // floor's own inputs removed — i.e. the pipeline before this change.
        let baseline = idx.search(&query(conceptual, 5)).await.unwrap();
        let ranked: Vec<&str> = outcome.results.iter().map(|r| r.id.as_str()).collect();
        let ranked_baseline: Vec<&str> = baseline.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ranked, ranked_baseline,
            "conceptual ranking must be deterministic and unchanged: {conceptual}"
        );
    }
}

#[tokio::test]
async fn an_unquoted_phrase_earns_no_literal_even_when_it_occurs_verbatim() {
    // Why: round-2 HIGH. An unquoted multi-word phrase used to earn the floor
    // under a hit cap alone, so a distinctive prose sentence occurring once in
    // a CHANGELOG line took rank 1 from every semantically-relevant chunk.
    // Quoting it is how a caller asks for it literally. #7675.
    // What: the phrase extracts nothing; the same words quoted extract a
    // `Quoted` literal that DOES floor the doc chunk carrying them.
    // Test: this test.
    let idx = fixture().await;
    assert!(
        extract_exact_literal(CONCEPTUAL_PROSE).is_none(),
        "an unquoted phrase must not earn the floor"
    );
    let quoted = extract_exact_literal(&format!("\"{CONCEPTUAL_PROSE}\"")).expect("quoted");
    assert_eq!(quoted.shape, LiteralShape::Quoted);
    let re = literal_regex(&quoted).expect("regex");
    let lane = idx
        .exact_match_lane(
            &quoted,
            &re,
            40,
            crate::core::indexer::SearchMode::All,
            None,
        )
        .await;
    assert_eq!(
        lane.hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        vec!["doc:prose"],
        "quoting the same words is how a caller asks for the literal"
    );
}

#[tokio::test]
async fn hybrid_top_one_agrees_with_lexical_for_every_planted_identifier() {
    // Why: the property #7675 asks for — for every identifier planted in the
    // corpus, the default hybrid `search` must land on the same file:line that
    // `search_lexical` does. Before the floor, three of the spike's nine
    // lookups disagreed.
    // What: iterates `PROPERTY_IDENTS` directly — full, non-duplicated coverage
    // of the definition/use tie planted per identifier, which the previous
    // sample-with-replacement LCG neither guaranteed nor checked.
    // Test: this test.
    let idx = fixture().await;
    let mut checked: Vec<&str> = Vec::new();
    for name in PROPERTY_IDENTS {
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
        checked.push(name);
    }
    // Teeth: the assertion names WHICH identifiers were compared, so a loop
    // that silently skips one (or compares one twice) fails here rather than
    // passing on a count the loop itself guarantees.
    assert_eq!(
        checked,
        PROPERTY_IDENTS.to_vec(),
        "every planted identifier must be compared exactly once, in order"
    );
}

#[test]
fn extract_exact_literal_reads_the_query_shapes() {
    // Why: the extractor is the whole gate between the floor and conceptual
    // ranking. #7675.
    // What: every accept shape and every reject shape.
    // Test: this test.
    assert_eq!(
        extract_exact_literal("session_mcp_scope.rs").map(|l| l.shape),
        Some(LiteralShape::Filename),
        "a bare filename is a ripgrep-parity literal"
    );
    assert!(
        extract_exact_literal("the cache is warm.").is_none(),
        "a sentence ending in a period is not a filename"
    );
    assert!(
        extract_exact_literal("notes.backup").is_none(),
        "an unknown extension is not a filename"
    );
    assert_eq!(
        extract_exact_literal("#7675").map(|l| l.shape),
        Some(LiteralShape::IssueRef)
    );
    assert!(
        extract_exact_literal("#draft").is_none(),
        "only digits follow the hash in an issue reference"
    );
    // MEDIUM, round 2: the two-token declaration form deliberately accepts a
    // bare, signal-less name — the keyword IS the disambiguation. Pinned so a
    // refactor cannot flip it silently.
    assert_eq!(
        extract_exact_literal("fn fold").map(|l| (l.text, l.shape)),
        Some(("fold".to_string(), LiteralShape::Identifier)),
        "`fn fold` asks for a declaration where bare `fold` is conceptual"
    );
    assert_eq!(
        extract_exact_literal("struct Cache").map(|l| l.text),
        Some("Cache".to_string())
    );
    assert!(
        extract_exact_literal("fold").is_none(),
        "the bare word alone stays conceptual"
    );
    assert!(
        extract_exact_literal("Palace").is_none(),
        "a single Capitalized name carries no boundary signal"
    );
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
    assert!(
        extract_exact_literal("how does the rehydrate ticker decide").is_none(),
        "an unquoted multi-word phrase earns nothing"
    );
}

#[tokio::test]
async fn a_filename_query_floors_the_chunks_of_that_file() {
    // Why: ripgrep finds `session_mcp_scope.rs` trivially; the identifier gate
    // rejected it outright because of the dot. Round-2 MEDIUM. #7675.
    // What: the floor matches the chunk's path basename, not its content, so a
    // chunk merely MENTIONING the filename is not floored.
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "file:target",
        "src/service/session_mcp_scope.rs",
        "fn apply_scope(project: &Project) -> Result<()> { Ok(()) }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "file:mention",
        "src/docs/notes.rs",
        "// see session_mcp_scope.rs for the scope rules",
    ))
    .await
    .unwrap();
    let lit = extract_exact_literal("session_mcp_scope.rs").expect("filename");
    let re = literal_regex(&lit).expect("regex");
    let lane = idx
        .exact_match_lane(&lit, &re, 10, crate::core::indexer::SearchMode::All, None)
        .await;
    assert_eq!(
        lane.hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        vec!["file:target"],
        "a filename query floors the file itself, not chunks that mention it"
    );
    assert!(
        !lane.full_scan,
        "a filename query matches paths, so it is not the content-scan fallback"
    );
}

#[tokio::test]
async fn a_common_basename_is_capped_and_does_not_bury_the_relevant_file() {
    // Why: round-3 HIGH — `Filename` had no analogue of the deleted
    // `PHRASE_HIT_CAP`, so `occurrences_of` floored every chunk of every file
    // sharing a basename, unbounded. A basename as common as `mod.rs` (510
    // files in this repo alone) would flood the floor with an effectively
    // arbitrary, id-ordered subset of unrelated files, burying whatever the
    // semantic lanes ranked. #7675.
    // What: plant more files sharing one basename than `FILENAME_HIT_CAP`,
    // plus one differently-named chunk. The lane must contribute at most the
    // cap (this is what fails against the pre-fix head: it returned all 12).
    // The final page — sized to the whole tiny corpus, so its composition is
    // deterministic regardless of fused-lane scoring — must still include the
    // differently-named chunk, proving the capped promotion does not drop it.
    // Test: this test.
    let idx = make_indexer();
    let flood = FILENAME_HIT_CAP + 4;
    for i in 0..flood {
        idx.add_chunk(raw(
            &format!("flood:{i}"),
            &format!("src/gen/mod_{i}/mod.rs"),
            &format!("fn handler_{i}(input: &str) -> usize {{ input.len() }}"),
        ))
        .await
        .unwrap();
    }
    idx.add_chunk(raw(
        "relevant:other",
        "src/core/other_module.rs",
        "fn distinct_helper() {}",
    ))
    .await
    .unwrap();

    let lit = extract_exact_literal("mod.rs").expect("filename");
    let re = literal_regex(&lit).expect("regex");
    // `want` is deliberately generous so only `FILENAME_HIT_CAP` — not `want`
    // — is what bounds the result.
    let lane = idx
        .exact_match_lane(
            &lit,
            &re,
            flood + 10,
            crate::core::indexer::SearchMode::All,
            None,
        )
        .await;
    assert!(
        lane.hits.len() <= FILENAME_HIT_CAP,
        "a Filename match must contribute at most FILENAME_HIT_CAP ({FILENAME_HIT_CAP}) hits \
         to the floor, got {} from {flood} planted files sharing the basename",
        lane.hits.len()
    );

    // Total corpus size equals top_k, so every chunk is guaranteed onto the
    // page regardless of lane scoring — the composition check below is about
    // whether the capped promotion still surfaces the other file, not about
    // fused-lane tie-breaking.
    let top_k = flood + 1;
    let results = idx.search(&query("mod.rs", top_k)).await.unwrap();
    assert!(
        results.iter().any(|r| r.id == "relevant:other"),
        "the differently-named chunk must not be dropped by the capped promotion, got {:?}",
        results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
    );
    assert!(
        !results.iter().all(|r| r.file.ends_with("mod.rs")),
        "the page must not be entirely the flooded basename"
    );
}

#[tokio::test]
async fn a_path_shaped_query_returns_its_file_first() {
    // Why: round-3 "must be true" — a multi-segment path query disambiguates
    // a common basename on purpose, and the cap must keep its exact match
    // first rather than truncate it away behind weaker basename-only hits in
    // an arbitrary order. #7675.
    // What: two chunks share the basename `exact.rs`; only one's whole path
    // ends with the queried multi-segment suffix `indexer/search/exact.rs`.
    // That one must rank first (the deterministic ordering this round adds:
    // exact path-suffix match before bare-basename-only match).
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "path:target",
        "crates/trusty-search/src/core/indexer/search/exact.rs",
        "fn exact_match_lane() {}",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "path:decoy",
        "crates/trusty-other/src/render/exact.rs",
        "fn unrelated() {}",
    ))
    .await
    .unwrap();

    let lit = extract_exact_literal("indexer/search/exact.rs").expect("path-shaped filename");
    assert_eq!(
        lit.shape,
        LiteralShape::Filename,
        "a path-shaped token must still read as a Filename literal"
    );
    let re = literal_regex(&lit).expect("regex");
    let lane = idx
        .exact_match_lane(&lit, &re, 10, crate::core::indexer::SearchMode::All, None)
        .await;
    assert_eq!(
        lane.hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        vec!["path:target", "path:decoy"],
        "the exact path-suffix match must rank ahead of the basename-only match"
    );

    // And it survives the full pipeline at top_k: 1 — ripgrep parity.
    let results = idx
        .search(&query("indexer/search/exact.rs", 1))
        .await
        .unwrap();
    assert_eq!(
        results[0].id, "path:target",
        "a path-shaped query must return its file on the first page, got {}",
        results[0].id
    );
}

#[tokio::test]
async fn an_issue_reference_floors_its_verbatim_occurrence() {
    // Why: `#7675` fails every identifier rule on its leading hash, yet it is
    // exactly the kind of literal a caller reaches for ripgrep with. #7675.
    // What: the reference is matched verbatim and does not hit `#76750`.
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "issue:hit",
        "src/core/a.rs",
        "// #7675: the exact-match floor lives here\nfn floor() {}",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "issue:longer",
        "src/core/b.rs",
        "// #76750 is a different issue entirely\nfn other() {}",
    ))
    .await
    .unwrap();
    let lit = extract_exact_literal("#7675").expect("issue ref");
    let re = literal_regex(&lit).expect("regex");
    let lane = idx
        .exact_match_lane(&lit, &re, 10, crate::core::indexer::SearchMode::All, None)
        .await;
    assert_eq!(
        lane.hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        vec!["issue:hit"],
        "`#7675` must not match inside `#76750`"
    );
}

#[tokio::test]
async fn an_archived_duplicate_ranks_below_the_live_definition() {
    // Why: round-2 HIGH. `apply_archive_downrank` multiplies a stale chunk's
    // score, and `apply_floor` then overwrote that score — so a deprecated
    // duplicate of a declaration could take rank 1 from the live one, inside
    // the one query type users rely on to reach the real definition. #7675.
    // What: two chunks declare the same identifier; the archived one must sort
    // second under the floor.
    // Test: this test.
    let idx = make_indexer();
    let body =
        "fn resolve_scope_root(p: &Path) -> Option<PathBuf> { p.parent().map(Path::to_owned) }";
    // `archive::classify` reads the path, so a `legacy/` directory is the
    // signal — the floor reuses that verdict rather than recomputing one. The
    // ids are chosen so the ARCHIVED chunk wins the final `id` tie-break: both
    // chunks are declarations with one occurrence and no branch preference, so
    // without the archive key in the sort this assertion fails, which is what
    // it did against 64f1718e6.
    idx.add_chunk(raw("arch:z_live", "src/core/scope.rs", body))
        .await
        .unwrap();
    idx.add_chunk(raw("arch:a_archived", "legacy/core/scope.rs", body))
        .await
        .unwrap();
    let results = idx.search(&query("resolve_scope_root", 5)).await.unwrap();
    let order: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
    assert!(
        order.len() >= 2,
        "both declarations must survive to be ordered, got {order:?}"
    );
    let live = order.iter().position(|id| *id == "arch:z_live");
    let old = order.iter().position(|id| *id == "arch:a_archived");
    assert!(
        matches!((live, old), (Some(l), Some(o)) if l < o),
        "the live declaration must outrank its archived duplicate, got {order:?}"
    );
}

#[tokio::test]
async fn the_postings_candidate_path_and_the_full_scan_agree() {
    // Why: the postings prefilter replaces an O(corpus) content scan, so it is
    // only sound if it finds exactly what the scan would. #7675.
    // What: the same identifier through the lane (postings-driven, because the
    // BM25 corpus covers every chunk) and through a direct content scan of the
    // fixture must name the same chunk ids.
    // Test: this test.
    let idx = fixture().await;
    let lit = extract_exact_literal(L3_IDENT).expect("identifier");
    let re = literal_regex(&lit).expect("regex");
    let lane = idx
        .exact_match_lane(&lit, &re, 40, crate::core::indexer::SearchMode::All, None)
        .await;
    assert!(
        !lane.full_scan,
        "a complete BM25 corpus must drive candidates from the postings"
    );
    let mut from_lane: Vec<String> = lane.hits.iter().map(|h| h.id.clone()).collect();
    from_lane.sort();
    let mut by_scan: Vec<String> = idx
        .chunks
        .read()
        .await
        .values()
        .filter(|raw| re.is_match(&raw.content))
        .map(|raw| raw.id.clone())
        .collect();
    by_scan.sort();
    assert_eq!(
        from_lane, by_scan,
        "the postings candidate set must find exactly what a full scan finds"
    );
    assert!(!by_scan.is_empty(), "the fixture must contain the literal");
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
