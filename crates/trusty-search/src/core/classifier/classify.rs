use regex::Regex;
use std::sync::OnceLock;

use super::intent::QueryIntent;

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
    /// Test: see the `tests` submodule for representative examples per intent.
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
        // `classify.rs`).
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
