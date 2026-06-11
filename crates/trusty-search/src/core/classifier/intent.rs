/// Search query intent classification enum.
///
/// Why: different query shapes benefit from different BM25/vector balance;
/// a typed enum lets the routing layer select optimal weights without
/// per-result heuristics.
/// What: enumerates the five recognised intent categories, each carrying its
/// own routing weight tuple via [`QueryIntent::weights`].
/// Test: see `classify.rs` and `tests.rs` for representative examples per intent.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryIntent {
    Definition, // BM25-heavy: alpha=0.3, beta=0.7
    Usage,      // KG-first: alpha=0.5, beta=0.5, use_kg_first=true
    Conceptual, // vector-heavy: alpha=0.8, beta=0.2
    BugDebt,    // BM25-only: alpha=0.1, beta=0.9
    Unknown,    // balanced: alpha=0.6, beta=0.4
}

impl QueryIntent {
    pub fn weights(&self) -> (f32, f32, bool) {
        // returns (alpha_vector, beta_bm25, use_kg_first)
        match self {
            QueryIntent::Definition => (0.3, 0.7, false),
            QueryIntent::Usage => (0.5, 0.5, true),
            QueryIntent::Conceptual => (0.8, 0.2, false),
            QueryIntent::BugDebt => (0.1, 0.9, false),
            QueryIntent::Unknown => (0.6, 0.4, false),
        }
    }
}
