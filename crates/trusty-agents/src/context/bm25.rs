//! BM25 scorer over an in-memory document set (#70).
//!
//! Why: Hybrid retrieval (vector + lexical) consistently beats either alone.
//! Keeping the index in-memory avoids a full-text search dependency; the
//! entries JSONL is small enough that a rebuild-per-query is fine.
//! What: Classic BM25 with `k1=1.5`, `b=0.75`. Caller feeds `(id, terms)` and
//! queries with pre-tokenized query terms.
//! Test: `bm25_ranks_matching_doc_higher`.

use std::collections::HashMap;

/// In-memory BM25 document index keyed by document id.
///
/// # Intent
/// Encapsulates BM25 state (k1/b constants, doc list, DF map, avg doc length)
/// so the retriever can treat lexical scoring as a black-box `score(query)`
/// call. Rebuilt on every retrieval — cheap enough at our scale and avoids
/// index-coherency bugs when the JSONL changes between runs.
///
/// Test: `bm25_empty_returns_empty`, `bm25_ranks_matching_doc_higher`.
pub struct Bm25Index {
    k1: f32,
    b: f32,
    docs: Vec<Bm25Doc>,
    avg_doc_len: f32,
    df: HashMap<String, u32>,
}

struct Bm25Doc {
    id: String,
    terms: Vec<String>,
    len: usize,
}

impl Default for Bm25Index {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm25Index {
    /// Construct an empty index with standard BM25 constants (k1=1.5, b=0.75).
    ///
    /// Why: The defaults are the canonical Robertson/Zaragoza values that work
    /// well for short-to-medium documents like our turn snippets.
    /// What: Zero-allocated empty index; call `add_doc` to populate.
    /// Test: `bm25_empty_returns_empty`.
    pub fn new() -> Self {
        Self {
            k1: 1.5,
            b: 0.75,
            docs: Vec::new(),
            avg_doc_len: 0.0,
            df: HashMap::new(),
        }
    }

    /// Add a document; terms should already be tokenized.
    pub fn add_doc(&mut self, id: String, terms: Vec<String>) {
        // Update DF using unique terms to match standard BM25.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for t in &terms {
            if seen.insert(t.as_str()) {
                *self.df.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let len = terms.len();
        self.docs.push(Bm25Doc { id, terms, len });
        let total: usize = self.docs.iter().map(|d| d.len).sum();
        self.avg_doc_len = if self.docs.is_empty() {
            0.0
        } else {
            total as f32 / self.docs.len() as f32
        };
    }

    /// Score all documents against `query_terms`; returns `(id, score)` pairs
    /// sorted descending by score.
    pub fn score(&self, query_terms: &[String]) -> Vec<(String, f32)> {
        let n = self.docs.len() as f32;
        if n == 0.0 {
            return Vec::new();
        }
        let mut scores: Vec<(String, f32)> = self
            .docs
            .iter()
            .map(|doc| {
                let mut tf_map: HashMap<&str, u32> = HashMap::new();
                for t in &doc.terms {
                    *tf_map.entry(t.as_str()).or_insert(0) += 1;
                }
                let score: f32 = query_terms
                    .iter()
                    .map(|term| {
                        let tf = *tf_map.get(term.as_str()).unwrap_or(&0) as f32;
                        let df = *self.df.get(term).unwrap_or(&0) as f32;
                        if df == 0.0 {
                            return 0.0;
                        }
                        let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                        let denom = tf
                            + self.k1
                                * (1.0 - self.b
                                    + self.b * (doc.len as f32) / self.avg_doc_len.max(1.0));
                        let tf_norm = (tf * (self.k1 + 1.0)) / denom.max(f32::EPSILON);
                        idf * tf_norm
                    })
                    .sum();
                (doc.id.clone(), score)
            })
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bm25_empty_returns_empty() {
        let idx = Bm25Index::new();
        assert!(idx.score(&toks(&["foo"])).is_empty());
    }

    #[test]
    fn bm25_ranks_matching_doc_higher() {
        let mut idx = Bm25Index::new();
        idx.add_doc("d1".into(), toks(&["rust", "tokio", "async"]));
        idx.add_doc("d2".into(), toks(&["python", "django", "views"]));
        idx.add_doc("d3".into(), toks(&["rust", "serde", "json"]));
        let scores = idx.score(&toks(&["rust", "tokio"]));
        // d1 has both query terms, d3 has one, d2 has none.
        assert_eq!(scores[0].0, "d1");
        assert!(scores[0].1 > scores[1].1);
        assert_eq!(scores.last().unwrap().0, "d2");
    }
}
