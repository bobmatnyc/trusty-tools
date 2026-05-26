use regex::Regex;
use std::sync::OnceLock;

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

pub struct QueryClassifier;

static DEFINITION_RE: OnceLock<Regex> = OnceLock::new();
static USAGE_RE: OnceLock<Regex> = OnceLock::new();
static CONCEPTUAL_RE: OnceLock<Regex> = OnceLock::new();
static BUG_DEBT_RE: OnceLock<Regex> = OnceLock::new();
// Entity-relationship keyword patterns (issue #21). Matched alongside the
// existing intent regexes; a hit overrides the default to bias the query
// toward the routing best-suited for that entity relationship.
static ENTITY_DEF_RE: OnceLock<Regex> = OnceLock::new();
static ENTITY_USAGE_RE: OnceLock<Regex> = OnceLock::new();
static ENTITY_BUG_RE: OnceLock<Regex> = OnceLock::new();
// Domain-term definition patterns (issue #88): PascalCase identifiers followed
// by structural keywords, and standalone structural vocabulary words.
static DOMAIN_DEF_RE: OnceLock<Regex> = OnceLock::new();
// Extended bug/debt vocabulary (issue #88).
static EXTENDED_BUG_RE: OnceLock<Regex> = OnceLock::new();
// Long natural-language conceptual queries (issue #88): ≥6 words, no code tokens.
static LONG_NL_RE: OnceLock<Regex> = OnceLock::new();
// Short identifier-dominated queries (issue #91): PascalCase identifier with
// no other intent verb → Definition.
static PASCAL_IDENT_RE: OnceLock<Regex> = OnceLock::new();
// Single snake_case identifier (issue #119): a one-token query whose only
// content is a snake_case function name → Definition.
static SNAKE_IDENT_RE: OnceLock<Regex> = OnceLock::new();
// All-caps acronym used as a struct/type hint (issue #119): one of the
// query tokens is an ALL_CAPS acronym (with optional embedded digits, e.g.
// `BM25`) that is also a plausible struct/type name. Routes to Definition
// so the structural lane lifts the canonical declaration over usage sites.
static ACRONYM_HINT_RE: OnceLock<Regex> = OnceLock::new();
// Multi-word natural-language queries (issue #119): ≥3 whitespace-separated
// tokens of which none are identifier tokens (no snake_case, no PascalCase,
// no leading-acronym identifier, no code punctuation). Lower bar than the
// existing 6-word `LONG_NL_RE` so 3-5 word concept queries also classify
// as Conceptual instead of Unknown.
static MULTI_NOUN_RE: OnceLock<Regex> = OnceLock::new();
// SCREAMING_SNAKE_CASE single-identifier pattern (issue #142): the WHOLE
// query is an ALL_CAPS identifier with underscores, e.g. `BRUSILOV_EPOCH`,
// `MAX_BATCH_SIZE`, `HNSW_EF_CONSTRUCTION`. These are Rust / Java / Python
// constants — first-class symbol names that belong in the Definition lane.
//
// Distinct from `ACRONYM_HINT_RE` which fires when an ALL_CAPS token appears
// *inside* a multi-word query. `SCREAM_IDENT_RE` requires the entire trimmed
// query to be a single constant identifier (no whitespace allowed).
//
// Pattern: starts with an uppercase letter, followed by uppercase letters,
// digits, or underscores, contains at least one underscore (to distinguish
// from pure acronyms like `HNSW` that are already handled by
// `ACRONYM_HINT_RE`), and is at least 2 characters total.
static SCREAM_IDENT_RE: OnceLock<Regex> = OnceLock::new();

impl QueryClassifier {
    /// Classify a query string into a `QueryIntent` for routing weight selection.
    ///
    /// Why: different query shapes benefit from different BM25/vector balance;
    /// classifying up-front lets the search pipeline pick optimal weights without
    /// per-result heuristics.
    /// What: applies a priority-ordered chain of regex patterns; the first match
    /// wins. Entity-relationship keywords (issue #21) and domain-term definitions
    /// (issue #88) are checked before the generic structural patterns.
    /// Test: see the `#[cfg(test)]` module below for representative examples per intent.
    pub fn classify(query: &str) -> QueryIntent {
        let def_re = DEFINITION_RE.get_or_init(|| {
            Regex::new(
                r"(?i)\b(fn |struct |impl |trait |enum |type |def |class |function |define)\b",
            )
            .expect("static regex pattern must compile")
        });
        let usage_re = USAGE_RE.get_or_init(|| {
            Regex::new(r"(?i)\b(where is|callers of|who calls|uses of|usages|called by)\b")
                .expect("static regex pattern must compile")
        });
        let conceptual_re = CONCEPTUAL_RE.get_or_init(|| {
            Regex::new(r"(?i)\b(how does|what is|explain|overview|architecture|design|why)\b")
                .expect("static regex pattern must compile")
        });
        let bug_re = BUG_DEBT_RE.get_or_init(|| {
            Regex::new(r"(?i)\b(TODO|FIXME|HACK|panic!|unwrap\(\)|bug|error|crash|fail)\b")
                .expect("static regex pattern must compile")
        });
        // Entity-relationship keyword regexes (issue #21).
        let entity_def_re = ENTITY_DEF_RE.get_or_init(|| {
            Regex::new(r"(?i)\b(implements|derives from|aliased as)\b")
                .expect("static regex pattern must compile")
        });
        let entity_usage_re = ENTITY_USAGE_RE.get_or_init(|| {
            Regex::new(r"(?i)\b(tested by|co-occurs)\b").expect("static regex pattern must compile")
        });
        let entity_bug_re = ENTITY_BUG_RE.get_or_init(|| {
            Regex::new(r"(?i)\b(raises|documented by)\b")
                .expect("static regex pattern must compile")
        });

        // Domain-term definition patterns (issue #88).
        //
        // Pattern A: a standalone structural/schema keyword used as a noun
        //   → "definition", "interface", "schema", "enum", "model" as a whole word.
        // Pattern B: a PascalCase identifier immediately followed by a structural
        //   keyword → "RoomType definition", "Hotel struct", "UserRole enum".
        let domain_def_re = DOMAIN_DEF_RE.get_or_init(|| {
            Regex::new(
                r"(?x)
                # Pattern A — standalone structural vocabulary word
                (?i)\b(definition|interface|schema|model|enum)\b
                |
                # Pattern B — PascalCase identifier + structural keyword
                \b[A-Z][a-zA-Z0-9]+\s+(?i)(definition|struct|class|interface|type|schema|enum|trait|model)\b
                ",
            )
            .expect("static regex pattern must compile")
        });

        // Extended bug/debt vocabulary (issue #88).
        let extended_bug_re = EXTENDED_BUG_RE.get_or_init(|| {
            Regex::new(
                r"(?i)\b(error\s+handling|deprecated|legacy|missing\s+validation|hardcoded)\b",
            )
            .expect("static regex pattern must compile")
        });

        // Long natural-language conceptual query (issue #88): ≥6 whitespace-
        // separated tokens with no code punctuation (`(`, `:`, `_`, `.`).
        let long_nl_re = LONG_NL_RE.get_or_init(|| {
            // A "code token" is any character in: ( ) : _ .
            // We match queries that have ≥6 word characters separated by spaces and
            // contain none of the above punctuation.
            Regex::new(r"^[^():_.]+(?:\s+[^():_.]+){5,}$")
                .expect("static regex pattern must compile")
        });

        // Priority chain — most-specific patterns first.

        // Entity-keyword hits take precedence over generic patterns (issue #21).
        if entity_usage_re.is_match(query) {
            return QueryIntent::Usage;
        }
        if entity_def_re.is_match(query) {
            return QueryIntent::Definition;
        }
        if entity_bug_re.is_match(query) {
            return QueryIntent::BugDebt;
        }

        // Domain-term definition patterns (issue #88) evaluated before the
        // generic `def_re` so "enum" / "interface" etc. are not missed.
        if domain_def_re.is_match(query) {
            return QueryIntent::Definition;
        }

        // Extended bug/debt vocabulary (issue #88).
        if extended_bug_re.is_match(query) {
            return QueryIntent::BugDebt;
        }

        if usage_re.is_match(query) {
            return QueryIntent::Usage;
        }
        if def_re.is_match(query) {
            return QueryIntent::Definition;
        }
        if conceptual_re.is_match(query) {
            return QueryIntent::Conceptual;
        }
        if bug_re.is_match(query) {
            return QueryIntent::BugDebt;
        }

        // Long natural-language query with no code tokens → Conceptual (issue #88).
        // Checked after all keyword patterns so that long queries containing
        // code tokens still fall through to the generic patterns.
        let trimmed = query.trim();
        if long_nl_re.is_match(trimmed) {
            return QueryIntent::Conceptual;
        }

        // Identifier-dominated queries (issue #91): when no explicit intent
        // verb matched, a query containing a PascalCase identifier (e.g.
        // "QueryClassifier intent classification") is most often a symbol
        // lookup → Definition.
        //
        // Two alternatives are accepted:
        //   1. CamelCase: capital letter, ≥1 lower-case letter, then another
        //      capital (`QueryClassifier`, `CodeChunk`). This guards against
        //      single-cap acronyms like "API" or "TODO" matching.
        //   2. Leading-acronym: ≥2 consecutive capitals, optional digits, then
        //      another capital + lower-case run (`BM25Index`, `IOError`,
        //      `URLParser`). Identifiers whose acronym prefix runs into
        //      digits and PascalCase are common in Rust/Python and should be
        //      treated as symbol lookups.
        let pascal_ident_re = PASCAL_IDENT_RE.get_or_init(|| {
            Regex::new(
                r"\b(?:[A-Z][a-z]+[A-Z][a-zA-Z0-9]*|[A-Z]{2,}(?:[0-9]+[A-Za-z][a-zA-Z0-9]*|[0-9]+|[A-Z][a-z][a-zA-Z0-9]*))\b",
            )
            .expect("static regex pattern must compile")
        });
        if pascal_ident_re.is_match(trimmed) {
            return QueryIntent::Definition;
        }

        // Single snake_case identifier (issue #119): one whitespace-separated
        // token containing at least one underscore and only ASCII identifier
        // chars (lowercase letters, digits, underscores). This is the shape
        // of a function-name query like `apply_archive_downrank` or
        // `get_call_chain`. Multi-word queries that *contain* a snake_case
        // token are intentionally NOT matched here — they go to the
        // multi-noun branch below or stay Unknown. Single tokens without an
        // underscore are not matched (avoids treating a bare `foo` as a
        // definition lookup).
        let snake_ident_re = SNAKE_IDENT_RE.get_or_init(|| {
            Regex::new(r"^[a-z][a-z0-9_]*_[a-z0-9_]+$").expect("static regex pattern must compile")
        });
        if snake_ident_re.is_match(trimmed) {
            return QueryIntent::Definition;
        }

        // SCREAMING_SNAKE_CASE single identifier (issue #142): the entire
        // query is an ALL_CAPS constant name like `BRUSILOV_EPOCH` or
        // `MAX_BATCH_SIZE`. These are first-class symbol names in Rust, Java,
        // and Python; routing to Definition engages the BM25-heavy lane so
        // the file containing the constant declaration outranks usage sites.
        //
        // Checked after `snake_ident_re` (disjoint patterns — no overlap)
        // and before `acronym_hint_re` (which handles ALL_CAPS tokens *inside*
        // multi-word queries rather than whole-query identifiers).
        let scream_ident_re = SCREAM_IDENT_RE.get_or_init(|| {
            // Whole query: one or more uppercase letters/digits, must contain
            // at least one underscore, no lowercase letters, no whitespace.
            // Minimum two chars. Does NOT match single lowercase segments.
            Regex::new(r"^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$")
                .expect("static regex pattern must compile")
        });
        if scream_ident_re.is_match(trimmed) {
            return QueryIntent::Definition;
        }

        // All-caps acronym hint (issue #119): the query contains a token that
        // is ≥2 uppercase ASCII letters, optionally followed by digits
        // (e.g. `HNSW`, `BM25`, `RRF`, `ORT`, `LRU`). These almost always
        // refer to a struct or module name in the codebase; routing to
        // Definition lets the structural lane surface `hnsw_store.rs` /
        // `bm25.rs` over usage sites that merely mention the concept.
        //
        // Word-boundary anchored so a query containing the acronym as a
        // substring of a larger word (`URLParser` — already matched by
        // `pascal_ident_re` above) doesn't re-fire here.
        //
        // Token-count guard (issue #197): only fire when the query is short
        // (≤2 tokens) OR has no natural-language (lowercase-leading) tokens.
        // Multi-word queries with both an ALL_CAPS acronym AND lowercase NL
        // words (e.g. "HNSW vector similarity search") read as concept
        // questions, not symbol lookups — they should fall through to the
        // multi-noun branch and classify as Conceptual so the semantic lane
        // can surface the canonical struct file (`store.rs`) over docs that
        // merely mention the acronym (regression from PR #162 dense docs in
        // `classifier.rs`).
        let acronym_hint_re = ACRONYM_HINT_RE.get_or_init(|| {
            Regex::new(r"\b[A-Z]{2,}[0-9]*\b").expect("static regex pattern must compile")
        });
        if acronym_hint_re.is_match(trimmed) {
            let token_count = trimmed.split_whitespace().count();
            let has_nl_words = trimmed
                .split_whitespace()
                .any(|t| t.chars().next().is_some_and(|c| c.is_lowercase()));
            if token_count <= 2 || !has_nl_words {
                return QueryIntent::Definition;
            }
            // fall through — multi-word acronym phrase with NL words → let
            // the multi-noun branch route this to Conceptual.
        }

        // Multi-noun query with no identifier tokens (issue #119): ≥4
        // whitespace-separated words, none of which are snake_case, no
        // PascalCase identifier (already handled above), no all-caps
        // acronym (handled just above), and no code punctuation. Lower bar
        // than the 6-word `LONG_NL_RE` so concept queries like
        // "axum middleware concurrency limiter",
        // "redb persistence write transaction", or
        // "embed batch async worker pool" still classify as Conceptual.
        // Threshold is 4 (not 3) so 3-word queries like "reservation
        // booking flow" stay Unknown — see the existing
        // `test_short_nl_query_not_forced_conceptual` regression test.
        let multi_noun_re = MULTI_NOUN_RE.get_or_init(|| {
            // 4+ tokens separated by whitespace; tokens contain no code
            // punctuation (no `_`, `(`, `)`, `:`, `.`, `/`, `-`) so we never
            // misclassify "axum-server" or "fn::foo" as a conceptual query.
            Regex::new(r"^[A-Za-z0-9]+(?:\s+[A-Za-z0-9]+){3,}$")
                .expect("static regex pattern must compile")
        });
        if multi_noun_re.is_match(trimmed) {
            return QueryIntent::Conceptual;
        }

        QueryIntent::Unknown
    }

    /// Classify with per-repo domain vocabulary.
    ///
    /// Why: repos define jargon (e.g. "PMS", "rate strategy", "RoomType") that
    /// the generic regex chain can't recognise. When `classify` returns
    /// `Unknown` and the query mentions a domain term, we nudge the result to
    /// `Definition` (the safe default — pulls the entity's defining chunk in
    /// via BM25 weight and lets the user iterate). All other intents survive
    /// untouched so explicit signals (`fn`, `callers of`, `TODO`) keep their
    /// routing.
    /// What: runs `classify`, then upgrades `Unknown` to `Definition` if any
    /// non-empty `domain_terms` entry appears case-insensitively as a substring
    /// of the query.
    /// Test: `test_domain_term_upgrades_unknown_to_definition`,
    /// `test_domain_term_does_not_override_explicit_intent`.
    pub fn classify_with_domain(query: &str, domain_terms: &[String]) -> QueryIntent {
        let base = Self::classify(query);
        if base != QueryIntent::Unknown {
            return base;
        }
        if domain_terms.is_empty() {
            return base;
        }
        let q = query.to_lowercase();
        for term in domain_terms {
            let t = term.trim();
            if t.is_empty() {
                continue;
            }
            if q.contains(&t.to_lowercase()) {
                return QueryIntent::Definition;
            }
        }
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_definition_intent() {
        assert_eq!(
            QueryClassifier::classify("fn search_hybrid"),
            QueryIntent::Definition
        );
        assert_eq!(
            QueryClassifier::classify("struct CodeIndexer"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_usage_intent() {
        assert_eq!(
            QueryClassifier::classify("callers of search_hybrid"),
            QueryIntent::Usage
        );
        assert_eq!(
            QueryClassifier::classify("where is CodeIndexer used"),
            QueryIntent::Usage
        );
    }

    #[test]
    fn test_conceptual_intent() {
        assert_eq!(
            QueryClassifier::classify("how does the search work"),
            QueryIntent::Conceptual
        );
        assert_eq!(
            QueryClassifier::classify("what is BM25"),
            QueryIntent::Conceptual
        );
    }

    #[test]
    fn test_bug_debt_intent() {
        assert_eq!(
            QueryClassifier::classify("TODO items in search"),
            QueryIntent::BugDebt
        );
        assert_eq!(
            QueryClassifier::classify("FIXME authentication"),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_usage_beats_definition() {
        assert_eq!(
            QueryClassifier::classify("callers of fn search_hybrid"),
            QueryIntent::Usage
        );
    }

    #[test]
    fn test_entity_implements_is_definition() {
        assert_eq!(
            QueryClassifier::classify("which types implements Embedder"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_entity_derives_from_is_definition() {
        assert_eq!(
            QueryClassifier::classify("structs that derives from Default"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_entity_aliased_as_is_definition() {
        assert_eq!(
            QueryClassifier::classify("Result aliased as anyhow::Result"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_entity_tested_by_is_usage() {
        assert_eq!(
            QueryClassifier::classify("authenticate tested by login_test"),
            QueryIntent::Usage
        );
    }

    #[test]
    fn test_entity_co_occurs_is_usage() {
        assert_eq!(
            QueryClassifier::classify("symbols that co-occurs in test fixtures"),
            QueryIntent::Usage
        );
    }

    #[test]
    fn test_entity_raises_is_bug_debt() {
        assert_eq!(
            QueryClassifier::classify("functions that raises ConfigError"),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_entity_documented_by_is_bug_debt() {
        assert_eq!(
            QueryClassifier::classify("ParseError documented by docs/errors.md"),
            QueryIntent::BugDebt
        );
    }

    // ── Domain-term definition tests (issue #88) ────────────────────────────

    #[test]
    fn test_domain_word_definition_is_definition() {
        assert_eq!(
            QueryClassifier::classify("RoomType definition"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_domain_pascal_struct_is_definition() {
        assert_eq!(
            QueryClassifier::classify("Hotel struct"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_domain_pascal_interface_is_definition() {
        assert_eq!(
            QueryClassifier::classify("BookingRepository interface"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_domain_pascal_enum_is_definition() {
        assert_eq!(
            QueryClassifier::classify("UserRole enum"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_standalone_schema_is_definition() {
        assert_eq!(
            QueryClassifier::classify("schema for reservations"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_standalone_interface_is_definition() {
        assert_eq!(
            QueryClassifier::classify("interface for payment processor"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_standalone_model_is_definition() {
        assert_eq!(
            QueryClassifier::classify("model for user accounts"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_domain_pascal_type_is_definition() {
        assert_eq!(
            QueryClassifier::classify("ReservationStatus type"),
            QueryIntent::Definition
        );
    }

    // ── Extended BugDebt tests (issue #88) ──────────────────────────────────

    #[test]
    fn test_error_handling_is_bug_debt() {
        assert_eq!(
            QueryClassifier::classify("error handling in payment flow"),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_deprecated_is_bug_debt() {
        assert_eq!(
            QueryClassifier::classify("deprecated authentication methods"),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_legacy_is_bug_debt() {
        assert_eq!(
            QueryClassifier::classify("legacy session management code"),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_missing_validation_is_bug_debt() {
        assert_eq!(
            QueryClassifier::classify("missing validation in user input"),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_hardcoded_is_bug_debt() {
        assert_eq!(
            QueryClassifier::classify("hardcoded connection strings"),
            QueryIntent::BugDebt
        );
    }

    // ── Long natural-language conceptual tests (issue #88) ──────────────────

    #[test]
    fn test_long_nl_query_is_conceptual() {
        assert_eq!(
            QueryClassifier::classify("how the reservation system handles overbooking scenarios"),
            QueryIntent::Conceptual
        );
    }

    #[test]
    fn test_long_nl_query_without_code_tokens_is_conceptual() {
        assert_eq!(
            QueryClassifier::classify("what happens when a payment method expires"),
            QueryIntent::Conceptual
        );
    }

    #[test]
    fn test_long_query_with_code_token_not_long_nl_conceptual() {
        // Contains `_` so the long-NL path should NOT fire. The query may still
        // be classified Conceptual via "how does" in the generic pattern — that
        // is correct behaviour. This test verifies that the long-NL path requires
        // no code tokens by using a query that has no other conceptual keywords.
        let result =
            QueryClassifier::classify("the payment_processor retries failed attempts five times");
        // Contains `_` → long-NL rule is suppressed; no other keyword match → Unknown
        assert_eq!(result, QueryIntent::Unknown);
    }

    // ── Domain-term routing tests (trusty-search.yaml config) ──────────────

    #[test]
    fn test_domain_term_upgrades_unknown_to_definition() {
        // Bare "rezo customers" — lowercase domain jargon, no generic
        // pattern matches → Unknown. With `domain_terms = ["rezo"]`,
        // upgrade to Definition.
        // (Updated for issue #119: the original test used "PMS integration"
        // which now classifies as Definition directly via the all-caps
        // acronym hint — see `test_acronym_struct_hint_is_definition`.
        // We switched to a lowercase jargon term to keep this test focused
        // on the domain-vocabulary upgrade path rather than the acronym
        // hint.)
        let terms = vec!["rezo".to_string()];
        assert_eq!(
            QueryClassifier::classify("rezo customers"),
            QueryIntent::Unknown
        );
        assert_eq!(
            QueryClassifier::classify_with_domain("rezo customers", &terms),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_domain_term_case_insensitive() {
        let terms = vec!["RateStrategy".to_string()];
        // Conceptual keyword "what is" wins regardless of domain match.
        assert_eq!(
            QueryClassifier::classify_with_domain("what is the ratestrategy applies", &terms),
            QueryIntent::Conceptual
        );
        // No conceptual keyword → falls through Unknown → upgraded.
        // Lowercase usage of a PascalCase domain term still matches.
        assert_eq!(
            QueryClassifier::classify_with_domain("ratestrategy fields", &terms),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_domain_term_does_not_override_explicit_intent() {
        let terms = vec!["PMS".to_string()];
        // Explicit "callers of" → Usage; domain term must not overwrite.
        assert_eq!(
            QueryClassifier::classify_with_domain("callers of PMS handler", &terms),
            QueryIntent::Usage
        );
        // Explicit `fn` → Definition (already correct).
        assert_eq!(
            QueryClassifier::classify_with_domain("fn handle_pms", &terms),
            QueryIntent::Definition
        );
        // Explicit TODO → BugDebt.
        assert_eq!(
            QueryClassifier::classify_with_domain("TODO refactor PMS adapter", &terms),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_domain_term_empty_list_passthrough() {
        // With no domain terms, behaviour matches plain `classify`.
        // Use the same lowercase jargon as the upgrade test so the
        // baseline path is exercised symmetrically.
        let terms: Vec<String> = vec![];
        assert_eq!(
            QueryClassifier::classify_with_domain("rezo customers", &terms),
            QueryIntent::Unknown
        );
    }

    // ── PascalCase identifier-dominated tests (issue #91) ──────────────────

    #[test]
    fn test_pascal_identifier_alone_is_definition() {
        assert_eq!(
            QueryClassifier::classify("QueryClassifier intent classification"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_camel_case_with_extra_words_is_definition() {
        assert_eq!(
            QueryClassifier::classify("CodeIndexer pipeline"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_pascal_identifier_loses_to_conceptual_verb() {
        // "how does" wins — Conceptual must take precedence over the
        // PascalCase fallback.
        assert_eq!(
            QueryClassifier::classify("how does QueryClassifier work"),
            QueryIntent::Conceptual
        );
    }

    #[test]
    fn test_standalone_enum_is_definition() {
        assert_eq!(
            QueryClassifier::classify("enum for reservation status"),
            QueryIntent::Definition
        );
    }

    // ── Leading-acronym identifier tests (issue #91) ───────────────────────

    #[test]
    fn test_leading_acronym_with_digits_is_definition() {
        // BM25Index — acronym + digits + CamelCase suffix.
        assert_eq!(
            QueryClassifier::classify("BM25Index lookup"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_leading_acronym_io_error_is_definition() {
        // IOError — two-letter acronym + CamelCase suffix.
        assert_eq!(
            QueryClassifier::classify("IOError handling path"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_leading_acronym_url_parser_is_definition() {
        // URLParser — three-letter acronym + CamelCase suffix.
        assert_eq!(
            QueryClassifier::classify("URLParser implementation"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_bm25_alone_is_definition_via_pascal_fallback() {
        // Standalone identifier `BM25` (acronym + digits) — no conceptual
        // verb, so the PascalCase fallback should classify as Definition.
        assert_eq!(
            QueryClassifier::classify("BM25 ranking"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_pure_acronym_now_triggers_definition() {
        // Issue #119: ALL_CAPS acronyms (`HNSW`, `BM25`, `RRF`, `ORT`, `API`,
        // ...) almost always refer to a struct or module name in the
        // codebase, so route them to Definition. This is the policy reversal
        // that closes #117 — "HNSW vector similarity search" must surface
        // `hnsw_store.rs` (struct) over `retrieval.rs` (usage), which
        // requires Definition intent for the structural lane to fire.
        // Previously (before #119) this query classified as Unknown and the
        // structural lane never engaged.
        assert_eq!(
            QueryClassifier::classify("API endpoints"),
            QueryIntent::Definition
        );
        // TODO is still BugDebt because `bug_re` is checked before the
        // acronym fallback.
        assert_eq!(
            QueryClassifier::classify("TODO items"),
            QueryIntent::BugDebt
        );
    }

    #[test]
    fn test_short_nl_query_not_forced_conceptual() {
        // Only 3 words — should not match the ≥4-word pattern
        let result = QueryClassifier::classify("reservation booking flow");
        // May be Unknown, not forced to Conceptual
        assert_ne!(result, QueryIntent::Conceptual);
    }

    // ── Single snake_case identifier tests (issue #119) ────────────────────

    #[test]
    fn test_single_snake_case_is_definition() {
        // Three real function names from the trusty-search codebase that
        // were producing `intent: Unknown` on the v0.8.1 benchmark.
        assert_eq!(
            QueryClassifier::classify("apply_archive_downrank"),
            QueryIntent::Definition
        );
        assert_eq!(
            QueryClassifier::classify("is_default_doc_excluded"),
            QueryIntent::Definition
        );
        assert_eq!(
            QueryClassifier::classify("get_call_chain"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_bare_snake_identifier_with_digits_is_definition() {
        // Digit-suffixed snake_case like `bm25_search` or `parse_v2_response`.
        assert_eq!(
            QueryClassifier::classify("bm25_search"),
            QueryIntent::Definition
        );
        assert_eq!(
            QueryClassifier::classify("parse_v2_response"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_multi_word_with_snake_does_not_match_snake_branch() {
        // Multi-token queries containing a snake_case word do NOT trip the
        // single-snake_case rule — only the lone identifier shape does.
        // They may still be classified by other branches; the assertion
        // here is that they aren't *forced* to Definition by this rule.
        // `the payment_processor retries failed attempts five times` —
        // 7 words with `_` — Unknown (existing regression test covers this).
        assert_eq!(
            QueryClassifier::classify("the payment_processor retries failed attempts five times"),
            QueryIntent::Unknown
        );
    }

    // ── ALL-CAPS acronym tests (issue #119 / #117) ──────────────────────────

    #[test]
    fn test_acronym_struct_hint_is_definition() {
        // Short acronym queries (≤2 tokens) still route to Definition.
        // Acronyms in 2-token phrases stay Definition because they read as
        // symbol lookups (e.g. "BM25 index" / "RRF fusion" / "ORT").
        // Issue #197 added a token-count guard so longer multi-word queries
        // with NL words (e.g. "HNSW vector similarity search") fall through
        // to the multi-noun / Conceptual path — see
        // `test_multi_word_acronym_with_nl_words_is_conceptual` below.
        assert_eq!(
            QueryClassifier::classify("BM25 index"),
            QueryIntent::Definition
        );
        assert_eq!(
            QueryClassifier::classify("RRF fusion"),
            QueryIntent::Definition
        );
        assert_eq!(QueryClassifier::classify("ORT"), QueryIntent::Definition);
        assert_eq!(QueryClassifier::classify("HNSW"), QueryIntent::Definition);
    }

    #[test]
    fn test_multi_word_acronym_with_nl_words_is_conceptual() {
        // Regression for issue #197: multi-word queries that combine an
        // ALL_CAPS acronym with natural-language tokens (≥3 tokens AND ≥1
        // lowercase-leading token) read as concept questions, not symbol
        // lookups. Routing them to Definition with beta=0.7 BM25 caused
        // dense-doc files (e.g. `classifier.rs` after PR #162's verbose
        // docs) to outrank the canonical struct file (`store.rs`) for
        // queries like "HNSW vector similarity search".
        //
        // After the token-count guard, ACRONYM_HINT_RE falls through for
        // these queries, letting the multi-noun branch classify them as
        // Conceptual (alpha=0.8 semantic). The semantic lane surfaces the
        // canonical struct file over docs that merely mention the acronym.
        assert_eq!(
            QueryClassifier::classify("HNSW vector similarity search"),
            QueryIntent::Conceptual
        );
        // Note: `BM25` (acronym + digits) matches `pascal_ident_re` before
        // reaching the acronym branch, so multi-word BM25 queries still
        // classify as Definition via the PascalCase fallback. The guard
        // only affects pure ALL_CAPS acronyms like HNSW, RRF, ORT, BFS.
        assert_eq!(
            QueryClassifier::classify("RRF fusion algorithm explanation"),
            QueryIntent::Conceptual
        );
    }

    // ── Multi-noun conceptual tests (issue #119) ────────────────────────────

    #[test]
    fn test_four_word_lowercase_is_conceptual() {
        // Concept queries with no identifier tokens at all. ≥4 words.
        assert_eq!(
            QueryClassifier::classify("axum middleware concurrency limiter"),
            QueryIntent::Conceptual
        );
        assert_eq!(
            QueryClassifier::classify("redb persistence write transaction"),
            QueryIntent::Conceptual
        );
        assert_eq!(
            QueryClassifier::classify("embed batch async worker pool"),
            QueryIntent::Conceptual
        );
        assert_eq!(
            QueryClassifier::classify("Louvain community detection modularity"),
            QueryIntent::Conceptual
        );
    }

    // ── SCREAMING_SNAKE_CASE identifier tests (issue #142) ─────────────────

    #[test]
    fn test_screaming_snake_brusilov_epoch_is_definition() {
        // Reproduces the exact failing query from the #142 bug report.
        assert_eq!(
            QueryClassifier::classify("BRUSILOV_EPOCH"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_screaming_snake_max_batch_size_is_definition() {
        assert_eq!(
            QueryClassifier::classify("MAX_BATCH_SIZE"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_screaming_snake_foo_bar_baz_is_definition() {
        assert_eq!(
            QueryClassifier::classify("FOO_BAR_BAZ"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_screaming_snake_is_default_doc_excluded_is_definition() {
        // Acceptance criterion from #142: all-caps version of a known identifier.
        assert_eq!(
            QueryClassifier::classify("IS_DEFAULT_DOC_EXCLUDED"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_screaming_snake_does_not_change_multiword_query() {
        // "HNSW vector similarity" contains an ALL_CAPS token but is NOT a
        // whole-query SCREAMING_SNAKE identifier (it has whitespace + mixed
        // case). The SCREAM_IDENT path therefore must not fire on it.
        //
        // Updated for issue #197: this query is 3 tokens with NL words
        // ("vector", "similarity"), so the ACRONYM_HINT token-count guard
        // suppresses the Definition route. The multi-noun branch needs ≥4
        // tokens, so this falls through to Unknown — which is the expected
        // outcome. (The corresponding 4-word variant
        // "HNSW vector similarity search" classifies as Conceptual; see
        // `test_multi_word_acronym_with_nl_words_is_conceptual`.)
        assert_eq!(
            QueryClassifier::classify("HNSW vector similarity"),
            QueryIntent::Unknown
        );
    }

    #[test]
    fn test_regular_snake_case_unaffected_by_scream_rule() {
        // `authenticate_user` must still be Definition via the snake_ident path.
        assert_eq!(
            QueryClassifier::classify("authenticate_user"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_fn_authenticate_unaffected_by_scream_rule() {
        // `fn authenticate` should remain Definition via `def_re`.
        assert_eq!(
            QueryClassifier::classify("fn authenticate"),
            QueryIntent::Definition
        );
    }

    #[test]
    fn test_lowercase_mixed_words_unaffected_by_scream_rule() {
        // A plain multi-word lowercase query must not be affected.
        // It would stay Unknown (3 words, no identifiers).
        assert_eq!(
            QueryClassifier::classify("reservation booking flow"),
            QueryIntent::Unknown
        );
    }

    // ── Canonical benchmark pinning (issue #119) ────────────────────────────

    /// Pin the canonical 14-query benchmark from the v0.8.1 grep-equivalency
    /// report. Of these, ≥12 must produce a non-`Unknown` intent so the
    /// downstream intent-aware ranking, lane selection, and mode override all
    /// engage on real queries. `install via cargo` (3 words) is the
    /// intentional Unknown — too short for the multi-noun rule, no
    /// identifier — and stays as a known limitation.
    #[test]
    fn test_canonical_benchmark_at_least_12_of_14_classified() {
        let queries: &[&str] = &[
            "SearchMode",
            "WalkOptions",
            "apply_archive_downrank",
            "is_default_doc_excluded",
            "get_call_chain",
            "symbol graph BFS expansion",
            "Louvain community detection modularity",
            "axum middleware concurrency limiter",
            "redb persistence write transaction",
            "embed batch async worker pool",
            "chunker AST tree-sitter code split",
            "HNSW vector similarity search",
            "install via cargo",
            "what is BM25",
        ];
        let non_unknown = queries
            .iter()
            .filter(|q| QueryClassifier::classify(q) != QueryIntent::Unknown)
            .count();
        assert!(
            non_unknown >= 12,
            "expected ≥12/14 queries to classify as non-Unknown; got {non_unknown}/14. \
             Per-query intents: {:?}",
            queries
                .iter()
                .map(|q| (*q, QueryClassifier::classify(q)))
                .collect::<Vec<_>>()
        );
    }
}
