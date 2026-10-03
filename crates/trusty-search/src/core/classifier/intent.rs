/// Search query intent classification enum.
///
/// Why: different query shapes benefit from different BM25/vector balance;
/// a typed enum lets the routing layer select optimal weights without
/// per-result heuristics.
/// What: enumerates the six recognised intent categories, each carrying its
/// own routing weight tuple via [`QueryIntent::weights`].
/// Test: see `classify.rs` and `tests.rs` for representative examples per intent.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryIntent {
    Definition, // BM25-heavy: alpha=0.3, beta=0.7
    Usage,      // KG-first: alpha=0.5, beta=0.5, use_kg_first=true
    Conceptual, // vector-heavy: alpha=0.8, beta=0.2
    BugDebt,    // BM25-only: alpha=0.1, beta=0.9
    // #9027: one bare word ("authentication", "target") — a topic or a symbol,
    // so it keeps Unknown's balanced routing everywhere and only names the shape.
    Keyword, // balanced: alpha=0.6, beta=0.4
    Unknown, // balanced: alpha=0.6, beta=0.4
}

impl QueryIntent {
    pub fn weights(&self) -> (f32, f32, bool) {
        // returns (alpha_vector, beta_bm25, use_kg_first)
        match self {
            QueryIntent::Definition => (0.3, 0.7, false),
            QueryIntent::Usage => (0.5, 0.5, true),
            QueryIntent::Conceptual => (0.8, 0.2, false),
            QueryIntent::BugDebt => (0.1, 0.9, false),
            QueryIntent::Keyword | QueryIntent::Unknown => (0.6, 0.4, false),
        }
    }

    /// `true` for the two intents with balanced routing, `Keyword` and
    /// `Unknown` (#9027).
    ///
    /// Why: `Keyword` keeps `Unknown`'s routing at every site — the Code→All
    /// mode upgrade, the soft doc downrank, the entity exact-match boost. One
    /// predicate keeps those sites from drifting apart when an intent changes.
    /// Test: `only_keyword_and_unknown_are_balanced`.
    pub fn is_balanced(&self) -> bool {
        matches!(self, QueryIntent::Keyword | QueryIntent::Unknown)
    }
}
