//! Intent-classification and domain-term routing tests for [`QueryClassifier`].
//!
//! Why: keeping the test suite split by concern avoids a single 674-line test
//! file; this file covers the four core intent categories and domain-term
//! upgrade logic (issues #88, #90, #119).
//! What: test functions for Definition / Usage / Conceptual / BugDebt
//! classification, entity-KG predicates, and `classify_with_domain`.
//! Test: run via `cargo test -p trusty-search core::classifier`.

use super::classify::QueryClassifier;
use super::intent::QueryIntent;

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
    assert_eq!(
        QueryClassifier::classify_with_domain("ratestrategy fields", &terms),
        QueryIntent::Definition
    );
}

#[test]
fn test_domain_term_does_not_override_explicit_intent() {
    let terms = vec!["PMS".to_string()];
    assert_eq!(
        QueryClassifier::classify_with_domain("callers of PMS handler", &terms),
        QueryIntent::Usage
    );
    assert_eq!(
        QueryClassifier::classify_with_domain("fn handle_pms", &terms),
        QueryIntent::Definition
    );
    assert_eq!(
        QueryClassifier::classify_with_domain("TODO refactor PMS adapter", &terms),
        QueryIntent::BugDebt
    );
}

#[test]
fn test_domain_term_empty_list_passthrough() {
    let terms: Vec<String> = vec![];
    assert_eq!(
        QueryClassifier::classify_with_domain("rezo customers", &terms),
        QueryIntent::Unknown
    );
}
