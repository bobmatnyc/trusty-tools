//! Aggregate quality metrics over a chunk corpus.
//!
//! Why: the `/indexes/:id/quality` endpoint and the `analyze_quality` MCP tool
//! both want a one-shot summary number — average cyclomatic complexity, %
//! grade-A chunks, total smell count. Pulling the math into a pure function
//! keeps the HTTP/MCP layers thin.
//!
//! Language dispatch: every chunk is scored via `compute_complexity_for`
//! (dispatched on `crate::lang::ext_map::lang_for_extension(&c.file)`) rather
//! than the raw text heuristic. The text heuristic's own module docs admit it
//! is "wrong on common idioms" (see `complexity_ts.rs`); routing through the
//! language-aware dispatcher gives Rust/TypeScript chunks accurate
//! tree-sitter-backed smell detection instead of whitespace-based guesses,
//! which is what made the whole-codebase smell count trustworthy again
//! (previously every chunk was scored 3x redundantly with the text-only
//! `compute_complexity`, regardless of its actual language).

use crate::types::complexity::{ComplexityGrade, ComplexityMetrics};
use crate::types::CodeChunk;
use serde::Serialize;

use crate::core::complexity::compute_complexity_for;

/// Score a chunk once, using the language dispatcher keyed off its file
/// extension, so callers don't each re-derive the language or re-run
/// complexity analysis on the same content.
fn metrics_of(c: &CodeChunk) -> ComplexityMetrics {
    let lang = crate::lang::ext_map::lang_for_extension(&c.file);
    compute_complexity_for(&c.content, lang)
}

/// One-shot quality summary over a chunk corpus.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QualityReport {
    pub avg_cyclomatic: f32,
    pub pct_grade_a: f32,
    pub smell_count: usize,
    pub chunk_count: usize,
}

/// A ranked complexity hotspot: the chunk plus the complexity numbers that
/// earned it its rank.
///
/// Why: `/complexity_hotspots` historically ranked chunks by cyclomatic
/// complexity but discarded the score before serialization (#2446), so HTTP
/// clients (trusty-review) either re-derived it or silently read zeros. This
/// wrapper carries the score through to the response so client-side complexity
/// bucketing works without a second pass.
/// What: flattens `CodeChunk` so the JSON stays backward-compatible — every
/// prior field remains at the top level — and adds two sibling fields,
/// `cyclomatic` and `cognitive`. `CodeChunk` itself is left untouched.
/// Test: `hotspots_carry_complexity_numbers` asserts the messy chunk ranks
/// first and its carried `cyclomatic` is non-zero.
#[derive(Debug, Clone, Serialize)]
pub struct HotspotItem {
    #[serde(flatten)]
    pub chunk: CodeChunk,
    pub cyclomatic: u32,
    pub cognitive: u32,
}

/// Sort `chunks` by descending cyclomatic complexity and return the top N,
/// each carrying the cyclomatic + cognitive numbers it was ranked on.
/// Chunks are scored via the language-aware `metrics_of` dispatcher.
pub fn complexity_hotspots(chunks: &[CodeChunk], top_n: usize) -> Vec<HotspotItem> {
    let mut scored: Vec<HotspotItem> = chunks
        .iter()
        .map(|c| {
            let metrics = metrics_of(c);
            HotspotItem {
                chunk: c.clone(),
                cyclomatic: metrics.cyclomatic,
                cognitive: metrics.cognitive,
            }
        })
        .collect();
    scored.sort_by_key(|h| std::cmp::Reverse(h.cyclomatic));
    scored.truncate(top_n);
    scored
}

/// Return chunks that have at least one detected smell.
pub fn smelly_chunks(chunks: &[CodeChunk]) -> Vec<CodeChunk> {
    chunks
        .iter()
        .filter(|c| !metrics_of(c).smells.is_empty())
        .cloned()
        .collect()
}

/// Compute the aggregate `QualityReport`.
pub fn aggregate_quality(chunks: &[CodeChunk]) -> QualityReport {
    let chunk_count = chunks.len();
    let (sum_cyclo, grade_a, smell_count) =
        chunks.iter().fold((0u64, 0usize, 0usize), |(s, a, sm), c| {
            let metrics = metrics_of(c);
            let a_inc = if metrics.grade == ComplexityGrade::A {
                1
            } else {
                0
            };
            (
                s + metrics.cyclomatic as u64,
                a + a_inc,
                sm + metrics.smells.len(),
            )
        });
    let avg_cyclomatic = if chunk_count == 0 {
        0.0_f32
    } else {
        sum_cyclo as f32 / chunk_count as f32
    };
    let pct_grade_a = if chunk_count == 0 {
        0.0_f32
    } else {
        grade_a as f32 / chunk_count as f32
    };
    QualityReport {
        avg_cyclomatic,
        pct_grade_a,
        smell_count,
        chunk_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &str, content: &str) -> CodeChunk {
        CodeChunk {
            id: id.into(),
            file: "f.rs".into(),
            start_line: 1,
            end_line: 10,
            content: content.into(),
            function_name: None,
            score: 0.0,
            compact_snippet: None,
            match_reason: "test".into(),
        }
    }

    #[test]
    fn empty_corpus_returns_zeros() {
        let r = aggregate_quality(&[]);
        assert_eq!(r.chunk_count, 0);
        assert_eq!(r.avg_cyclomatic, 0.0);
        assert_eq!(r.pct_grade_a, 0.0);
        assert_eq!(r.smell_count, 0);
    }

    #[test]
    fn hotspots_carry_complexity_numbers() {
        // build content with varying decision-point counts.
        let simple = chunk("s", "/// doc\nfn s() {}\n");
        let mut messy_src = String::from("/// doc\nfn m(a: u32) {\n");
        for _ in 0..10 {
            messy_src.push_str("    if a == 1 { return; }\n");
        }
        messy_src.push_str("}\n");
        let messy = chunk("m", &messy_src);
        let hot = complexity_hotspots(&[simple.clone(), messy.clone()], 2);
        assert_eq!(hot.len(), 2);
        assert_eq!(hot[0].chunk.id, "m", "messy chunk ranks first");
        // The carried score is the number it was ranked on — non-zero here.
        assert!(
            hot[0].cyclomatic > 1,
            "messy chunk cyclomatic should exceed 1"
        );
        assert!(hot[0].cyclomatic >= hot[1].cyclomatic);
    }

    #[test]
    fn hotspot_item_flattens_chunk_and_adds_numbers() {
        // The JSON must keep every CodeChunk field at the top level and add
        // cyclomatic + cognitive siblings (backward-compatible, additive).
        let items = complexity_hotspots(&[chunk("f:1:5", "fn f() {}")], 1);
        let v = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(v["id"], "f:1:5"); // flattened chunk field at top level
        assert!(v.get("cyclomatic").is_some());
        assert!(v.get("cognitive").is_some());
    }

    #[test]
    fn smelly_chunks_filters_clean_ones() {
        let clean = chunk("c", "/// doc\nfn f() {}\n");
        let mut long = String::from("/// doc\nfn big() {\n");
        for _ in 0..60 {
            long.push_str("    let _ = 1;\n");
        }
        long.push_str("}\n");
        let big = chunk("b", &long);
        let out = smelly_chunks(&[clean, big]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "b");
    }

    #[test]
    fn idiomatic_private_enumerate_chunk_is_not_smelly() {
        // End-to-end regression test for the reported false positive: a
        // whole-codebase quality scan must not flag an ordinary, private
        // helper function that uses idiomatic `.iter().enumerate()`.
        let src = r#"fn process(items: &[i32]) -> i32 {
    let mut total = 0;
    for (i, item) in items.iter().enumerate() {
        total += i as i32 + item;
    }
    total
}
"#;
        let c = chunk("enum1", src);
        assert!(
            smelly_chunks(std::slice::from_ref(&c)).is_empty(),
            "idiomatic private .enumerate() chunk must not be reported as smelly"
        );
        let report = aggregate_quality(&[c]);
        assert_eq!(
            report.smell_count, 0,
            "idiomatic private .enumerate() chunk must contribute zero smells"
        );
    }

    #[test]
    fn undocumented_pub_chunk_still_counts_as_smelly() {
        // The fix must not silence real signal: an undocumented `pub` item
        // is still part of the public API and should still be flagged.
        let c = chunk("pub1", "pub fn f() {}\n");
        assert_eq!(smelly_chunks(std::slice::from_ref(&c)).len(), 1);
        let report = aggregate_quality(&[c]);
        assert_eq!(report.smell_count, 1);
    }
}
