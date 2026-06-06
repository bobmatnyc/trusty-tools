//! Hybrid memory retrieval with temporal decay + cluster boost (#71).
//!
//! Why: Vector similarity alone misses lexical exact matches (proper nouns,
//! file paths). BM25 alone misses paraphrases. Combining both and applying
//! an exponential temporal decay keeps recent, relevant turns at the top.
//! Cluster entries (consolidated from prior searches) get a 2x boost so the
//! system self-reinforces good hits.
//! What: `MemoryRetriever` scores every entry via
//! `alpha * cosine + beta * bm25` times `exp(-lambda * age_hours)` times
//! `cluster_boost`.
//! Test: Covered by unit tests exercising `cosine_similarity` and the
//! end-to-end ranking.

use chrono::Utc;

use super::bm25::Bm25Index;
use super::indexer::IndexedEntry;

/// Tunable retrieval weights and limits.
pub struct MemoryRetriever {
    pub alpha: f32,
    pub beta: f32,
    pub lambda: f32,
    pub max_results: usize,
}

impl Default for MemoryRetriever {
    fn default() -> Self {
        Self {
            alpha: 0.6,
            beta: 0.4,
            lambda: 0.1,
            max_results: 5,
        }
    }
}

impl MemoryRetriever {
    /// Score and rank entries; returns top `max_results` with final scores.
    pub fn search(
        &self,
        query_embedding: &[f32],
        query_terms: &[String],
        entries: &[IndexedEntry],
        clusters: &[IndexedEntry],
    ) -> Vec<SearchResult> {
        let all: Vec<&IndexedEntry> = entries.iter().chain(clusters.iter()).collect();
        if all.is_empty() {
            return Vec::new();
        }

        let cluster_ids: std::collections::HashSet<&str> =
            clusters.iter().map(|c| c.id.as_str()).collect();

        let mut bm25 = Bm25Index::new();
        for e in &all {
            bm25.add_doc(e.id.clone(), e.bm25_terms.clone());
        }
        let bm25_scores: std::collections::HashMap<String, f32> =
            bm25.score(query_terms).into_iter().collect();

        let mut results: Vec<SearchResult> = all
            .iter()
            .map(|e| {
                let cosine = cosine_similarity(query_embedding, &e.embedding);
                let bm25_s = bm25_scores.get(&e.id).copied().unwrap_or(0.0);
                let combined = self.alpha * cosine + self.beta * bm25_s;

                let age_hours = (Utc::now() - e.turn.timestamp).num_minutes() as f32 / 60.0;
                let decay = (-self.lambda * age_hours.max(0.0)).exp();

                let cluster_boost = if cluster_ids.contains(e.id.as_str()) {
                    2.0
                } else {
                    1.0
                };

                SearchResult {
                    entry: (*e).clone(),
                    score: combined * decay * cluster_boost,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(self.max_results);
        results
    }
}

/// Cosine similarity between two equal-length vectors.
///
/// Why: Used by both retrieval and the memory cleaner (dedup), so exposed as
/// a pub helper to avoid duplication.
/// What: Returns 0.0 for mismatched lengths or zero magnitude.
/// Test: `cosine_identical_is_one`, `cosine_orthogonal_is_zero`.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

/// One scored retrieval hit returned by `MemoryRetriever`.
///
/// # Intent
/// Pairs the underlying entry with its combined hybrid score so callers can
/// inspect ranking directly (e.g. for debug logs or confidence gating).
///
/// Test: Indirectly via `cosine_identical_is_one` + retriever tests.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub entry: IndexedEntry,
    pub score: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::indexer::TurnRecord;

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        let s = cosine_similarity(&v, &v);
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_mismatched_length_returns_zero() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
    }

    fn mkentry(id: &str, emb: Vec<f32>, terms: Vec<&str>) -> IndexedEntry {
        IndexedEntry {
            id: id.to_string(),
            turn: TurnRecord {
                session_id: "s".into(),
                agent: "a".into(),
                turn_number: 0,
                timestamp: Utc::now(),
                prompt_text: "p".into(),
                response_text: "r".into(),
                prompt_tokens: 0,
                completion_tokens: 0,
            },
            embedding: emb,
            bm25_terms: terms.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn search_empty_returns_empty() {
        let r = MemoryRetriever::default();
        let out = r.search(&[1.0, 0.0], &["x".into()], &[], &[]);
        assert!(out.is_empty());
    }

    #[test]
    fn search_ranks_cluster_above_matching_entry() {
        let r = MemoryRetriever::default();
        let entry = mkentry("e1", vec![1.0, 0.0], vec!["foo"]);
        let cluster = mkentry("c1", vec![1.0, 0.0], vec!["foo"]);
        let out = r.search(&[1.0, 0.0], &["foo".into()], &[entry], &[cluster]);
        assert_eq!(out[0].entry.id, "c1", "cluster should get 2x boost");
    }
}
