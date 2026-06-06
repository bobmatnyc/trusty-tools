//! Lightweight regex/keyword query classifier (#376).
//!
//! Why: Hybrid search (vector + BM25 RRF) hardcodes a single weighting
//! across all queries, but different intents have different optimal
//! weights. A query like `fn foo` benefits heavily from BM25 (exact
//! identifier match), while "how does authentication work" should weigh
//! the embedding signal more. Classifying intent at sub-millisecond cost
//! lets us tune `(alpha, beta)` per query without an LLM round-trip.
//! What: Pure functions — no I/O, no allocations beyond the lowercase
//! query string. Returns a [`ClassifiedQuery`] that downstream
//! `search_hybrid` consumes.
//! Test: See unit tests in this file — one per intent.

/// Coarse intent of a free-text search query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    /// `fn foo`, `def bar`, `struct X`, bare identifier — exact-name lookups.
    Definition,
    /// "callers of foo", "where is X used" — go to the symbol graph first.
    Usage,
    /// "how does X work", "explain", "what is" — vector recall dominates.
    Conceptual,
    /// "TODO", "FIXME", "panic", "unwrap" — pure pattern match.
    BugDebt,
    /// Default bucket for queries that don't match any signal.
    Unknown,
}

/// Result of classifying a query — intent plus tuned hybrid weights.
///
/// Why: Centralises the policy choice so call sites just consume `vector_weight`
/// / `bm25_weight` and stay agnostic to the underlying intent buckets.
/// What: Plain struct; carries the intent (for tracing) and the three
/// dispatch knobs (`alpha`, `beta`, `use_kg_first`).
#[derive(Debug, Clone)]
pub struct ClassifiedQuery {
    pub intent: QueryIntent,
    /// Vector recall weight (alpha). Higher = lean on embedding similarity.
    pub vector_weight: f32,
    /// BM25 lexical weight (beta). Higher = lean on exact tokens.
    pub bm25_weight: f32,
    /// When true, callers should consult the symbol graph before the
    /// vector/BM25 fusion (e.g., "callers of foo" jumps straight there).
    pub use_kg_first: bool,
}

/// Classify `query` into one of [`QueryIntent`] and pick fusion weights.
///
/// Why: Sub-millisecond router that lets `search_hybrid` adapt its RRF
/// weighting per query without paying an LLM call.
/// What: Lower-cases the query once, then runs a small set of regex-free
/// keyword/prefix checks in priority order. The first match wins. The
/// weights returned are per the issue spec (#376):
///   - Definition: alpha=0.3, beta=0.7
///   - Usage: alpha=0.5, beta=0.5, use_kg_first=true
///   - Conceptual: alpha=0.8, beta=0.2
///   - BugDebt: alpha=0.1, beta=0.9
///   - Unknown: alpha=0.6, beta=0.4
/// Test: One test per branch in this file's `tests` module.
pub fn classify_query(query: &str) -> ClassifiedQuery {
    let q = query.trim();
    let lower = q.to_lowercase();

    // --- Usage: precedence over Definition because "callers of foo" also
    //     matches the Definition heuristic of "ends with identifier".
    if contains_any(
        &lower,
        &[
            "calls ",
            "callers of",
            "uses ",
            "where is",
            "usages of",
            "references to",
            "who calls",
        ],
    ) {
        return ClassifiedQuery {
            intent: QueryIntent::Usage,
            vector_weight: 0.5,
            bm25_weight: 0.5,
            use_kg_first: true,
        };
    }

    // --- Conceptual: "how/why/what/explain ..." — vector-heavy.
    if starts_with_word(
        &lower,
        &["how", "what", "why", "explain", "describe", "understand"],
    ) {
        return ClassifiedQuery {
            intent: QueryIntent::Conceptual,
            vector_weight: 0.8,
            bm25_weight: 0.2,
            use_kg_first: false,
        };
    }

    // --- BugDebt: marker tokens; case-insensitive.
    if contains_any(
        &lower,
        &[
            "todo",
            "fixme",
            "hack",
            "panic",
            "unwrap",
            "error handling",
            "xxx",
        ],
    ) {
        return ClassifiedQuery {
            intent: QueryIntent::BugDebt,
            vector_weight: 0.1,
            bm25_weight: 0.9,
            use_kg_first: false,
        };
    }

    // --- Definition: language keywords or bare identifier-only queries.
    if starts_with_word(
        &lower,
        &[
            "fn", "def", "class", "struct", "impl", "trait", "type", "enum", "const", "let",
        ],
    ) || lower.ends_with(" definition")
        || is_bare_identifier(q)
    {
        return ClassifiedQuery {
            intent: QueryIntent::Definition,
            vector_weight: 0.3,
            bm25_weight: 0.7,
            use_kg_first: false,
        };
    }

    // --- Default: balanced.
    ClassifiedQuery {
        intent: QueryIntent::Unknown,
        vector_weight: 0.6,
        bm25_weight: 0.4,
        use_kg_first: false,
    }
}

/// True iff `haystack` contains any of `needles` (already-lowercased).
fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// True iff `lower` starts with any of `words` followed by whitespace
/// (or is exactly equal to one of them).
fn starts_with_word(lower: &str, words: &[&str]) -> bool {
    for w in words {
        if let Some(rest) = lower.strip_prefix(w)
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            return true;
        }
    }
    false
}

/// True iff `s` looks like a single identifier — no whitespace, no `?`,
/// non-empty, and only ASCII alphanumerics / `_` / `:`.
fn is_bare_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.contains(char::is_whitespace) || s.contains('?') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_definition_with_keyword_prefix() {
        let c = classify_query("fn foo");
        assert_eq!(c.intent, QueryIntent::Definition);
        assert!((c.vector_weight - 0.3).abs() < 1e-6);
        assert!((c.bm25_weight - 0.7).abs() < 1e-6);
        assert!(!c.use_kg_first);
    }

    #[test]
    fn classifies_definition_for_bare_identifier() {
        let c = classify_query("CodeIndexer");
        assert_eq!(c.intent, QueryIntent::Definition);
    }

    #[test]
    fn classifies_definition_for_namespaced_identifier() {
        let c = classify_query("crate::search::indexer");
        assert_eq!(c.intent, QueryIntent::Definition);
    }

    #[test]
    fn classifies_usage_and_sets_kg_first() {
        let c = classify_query("callers of search_hybrid");
        assert_eq!(c.intent, QueryIntent::Usage);
        assert!(c.use_kg_first, "usage queries must hit the KG first");
    }

    #[test]
    fn classifies_usage_for_where_is() {
        let c = classify_query("where is build_router used");
        assert_eq!(c.intent, QueryIntent::Usage);
    }

    #[test]
    fn classifies_conceptual_for_how_query() {
        let c = classify_query("how does the indexer warm up");
        assert_eq!(c.intent, QueryIntent::Conceptual);
        assert!(c.vector_weight > c.bm25_weight, "conceptual leans vector");
    }

    #[test]
    fn classifies_conceptual_for_what_is() {
        let c = classify_query("what is RRF");
        assert_eq!(c.intent, QueryIntent::Conceptual);
    }

    #[test]
    fn classifies_bug_debt_for_todo() {
        let c = classify_query("TODO finish search");
        assert_eq!(c.intent, QueryIntent::BugDebt);
        assert!(c.bm25_weight > c.vector_weight, "bug-debt is BM25-heavy");
    }

    #[test]
    fn classifies_bug_debt_for_panic() {
        let c = classify_query("panic in indexer");
        assert_eq!(c.intent, QueryIntent::BugDebt);
    }

    #[test]
    fn classifies_unknown_for_balanced_query() {
        let c = classify_query("rate limit retry policy");
        assert_eq!(c.intent, QueryIntent::Unknown);
        assert!((c.vector_weight - 0.6).abs() < 1e-6);
        assert!((c.bm25_weight - 0.4).abs() < 1e-6);
    }

    #[test]
    fn usage_takes_priority_over_definition() {
        // "where is foo" looks like Definition (ends in identifier) but
        // is really Usage. Usage check runs first and must win.
        let c = classify_query("where is foo");
        assert_eq!(c.intent, QueryIntent::Usage);
    }
}
