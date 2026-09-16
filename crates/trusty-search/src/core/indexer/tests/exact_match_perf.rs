//! Exact-match lane latency at the crate's documented corpus scale (#7675).
//!
//! Why: the lane verifies a literal against the corpus, and the first round of
//! #7675 did that with a sequential regex scan over every chunk's content. The
//! only evidence in that PR came from a 2 000-chunk fixture; linear
//! extrapolation to this crate's own "Sub-10ms p50 warm query on a 100k-chunk
//! index" target (`crates/trusty-search/CLAUDE.md`, Performance Targets) put
//! the added cost in the tens of milliseconds. This harness measures it there.
//! What: builds a 100 000-chunk in-memory corpus, plants each literal in
//! exactly one chunk, and reports the p50 of 21 timed `exact_match_lane` calls
//! for an identifier and for a quoted phrase.
//!
//! `#[ignore]`d — it is a measurement, not an assertion about the host it runs
//! on, and building the corpus costs minutes in a debug build. Run it in
//! release, with the two caps raised past the corpus size so the BM25 postings
//! cover every chunk:
//!
//! ```text
//! TRUSTY_MAX_CHUNKS=200000 TRUSTY_BM25_CORPUS_CAP=200000 \
//!   cargo test -p trusty-search --release --lib exact_match_lane_p50_at_100k \
//!   -- --ignored --nocapture
//! ```
//!
//! Test: this module.

use super::*;
use crate::core::indexer::search::exact::{extract_exact_literal, literal_regex};

/// Chunks in the synthetic corpus — the scale the crate's p50 target names.
const CORPUS_CHUNKS: usize = 100_000;
/// Timed runs per query. Odd, so the p50 is a real observation.
const TIMED_RUNS: usize = 21;
/// Untimed runs first, so the regex cache and allocator are warm.
const WARMUP_RUNS: usize = 3;

/// The identifier planted in exactly one chunk.
const PERF_IDENT: &str = "render_savings_segment";
/// The quoted phrase planted in exactly one (different) chunk.
const PERF_PHRASE: &str = "not smaller than the instruction sources";

/// Body text for a filler chunk — sized like a real function so the scan cost
/// the measurement reports is the scan cost the daemon pays.
fn filler(i: usize) -> String {
    format!(
        "fn handler_{i}(request: &Request, state: &State) -> Result<Response> {{\n\
         let scope = state.resolve_scope(request.index_id())?;\n\
         let budget = scope.budget_for(request.kind()).unwrap_or_default();\n\
         let mut page = state.corpus.page(scope.id(), budget, request.cursor())?;\n\
         page.retain(|row| row.is_visible_to(request.caller()));\n\
         tracing::debug!(rows = page.len(), \"handler_{i} answered\");\n\
         Ok(Response::ok(page))\n\
         }}"
    )
}

/// Median of a sorted-in-place sample.
fn p50(mut samples: Vec<std::time::Duration>) -> std::time::Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement harness: 100k-chunk build; run in release with the caps raised"]
async fn exact_match_lane_p50_at_100k() {
    let idx = CodeIndexer::new("perf-7675", "/tmp/perf-7675");
    for i in 0..CORPUS_CHUNKS {
        let body = match i {
            7 => format!(
                "fn {PERF_IDENT}(total: &SavingsTotal, width: usize) -> String {{\n\
                 let pct = total.percent_saved();\n\
                 format!(\"{{pct:.0}}%\")\n\
                 }}"
            ),
            11 => format!(
                "fn warn_once() {{ tracing::warn!(\"the folded total is {PERF_PHRASE}\"); }}"
            ),
            _ => filler(i),
        };
        idx.add_chunk_inner(raw(
            &format!("perf:{i}"),
            &format!("src/gen/mod_{}/handler_{i}.rs", i % 512),
            &body,
        ))
        .await
        .expect("add chunk");
    }
    let built = idx.chunks.read().await.len();
    assert_eq!(
        built, CORPUS_CHUNKS,
        "the corpus must reach {CORPUS_CHUNKS} chunks — raise TRUSTY_MAX_CHUNKS"
    );

    for (label, query) in [
        ("identifier", PERF_IDENT.to_string()),
        ("quoted phrase", format!("\"{PERF_PHRASE}\"")),
    ] {
        let lit = extract_exact_literal(&query).expect("the query must name a literal");
        let re = literal_regex(&lit).expect("regex compiles");
        for _ in 0..WARMUP_RUNS {
            let _ = idx
                .exact_match_lane(
                    &lit,
                    &re,
                    10,
                    crate::core::indexer::SearchMode::All,
                    None,
                    None,
                )
                .await;
        }
        let mut samples = Vec::with_capacity(TIMED_RUNS);
        let mut hits = 0;
        for _ in 0..TIMED_RUNS {
            let started = std::time::Instant::now();
            let lane = idx
                .exact_match_lane(
                    &lit,
                    &re,
                    10,
                    crate::core::indexer::SearchMode::All,
                    None,
                    None,
                )
                .await;
            samples.push(started.elapsed());
            hits = lane.hits.len();
        }
        println!(
            "exact_match_lane p50 [{label}] over {CORPUS_CHUNKS} chunks: {:?} (hits={hits})",
            p50(samples)
        );
        assert_eq!(hits, 1, "each literal is planted in exactly one chunk");
    }
}
