//! Method extraction — the OQ-1 seam.
//!
//! Why: a ticket/spec body is free prose; the matrix (spec §4) needs a
//! *method* (a prescribed approach/technique/constraint), not the whole body.
//! Extraction must be **conservative**: ambiguous text → `None`, never a
//! hallucinated constraint (spec §6.2).
//!
//! **OQ-1 decision (settled in C1, per spec §9 OQ-1 — "leans hybrid"):**
//! C1 ships a **heuristic-first, pluggable-extractor** design.
//! - The DEFAULT path is [`HeuristicMethodExtractor`] — pure-Rust pattern
//!   matching over imperative-method phrasing. It requires **no network**, so
//!   the resolver (and its tests) work fully offline. This is the right C1
//!   default: zero cost/latency, no false-method risk from an LLM.
//! - The [`MethodExtractor`] trait is the pluggable seam. A future LLM-backed
//!   extractor (adapting `trusty_common::chat::ChatProvider` with the fixed
//!   classification prompt from spec §6.2) can be supplied by a caller without
//!   touching the resolver — realising the "heuristic first, LLM fallback"
//!   hybrid the spec recommends. We deliberately do NOT wire an LLM call by
//!   default: it would make `resolve` network-dependent and is out of C1 scope.
//!
//! What: the `MethodExtractor` trait + the heuristic default + the standalone
//! `heuristic_method` function the spec-resolver reuses.
//! Test: `super::tests::extract_*`.

use super::types::{Method, MethodKind};

/// Pluggable method-extraction strategy (the OQ-1 seam).
///
/// Why: extraction policy (heuristic vs. LLM vs. hybrid) is the one genuinely
/// open design question (spec OQ-1). Modelling it as a trait lets a caller swap
/// strategies — including an LLM-backed one — without changing the resolver.
/// What: one method, `extract`, mapping a free-prose body to an optional
/// `Method`. Implementors MUST be conservative: ambiguous → `None`.
/// Test: `super::tests::extract_*` exercise the default impl.
pub trait MethodExtractor: Send + Sync {
    /// Extract a prescribed method from a ticket/spec body, or `None`.
    ///
    /// Why: callers compare a *method*, not free prose (spec §4, §6.2).
    /// What: returns `Some(Method)` only when the body unambiguously prescribes
    /// an approach/constraint; `None` otherwise.
    /// Test: `super::tests::extract_*`.
    fn extract(&self, body: &str) -> Option<Method>;
}

/// The default, network-free heuristic extractor (OQ-1 default path).
///
/// Why: the common case ("use cursor-based pagination", "no new dependency",
/// "reuse the existing `ContextSource` trait") is recognisable from imperative
/// phrasing without an LLM, and a heuristic carries zero cost/latency and no
/// hallucination risk (spec §6.2, OQ-1).
/// What: a zero-field marker whose [`MethodExtractor::extract`] delegates to
/// [`heuristic_method`].
/// Test: `super::tests::extract_heuristic_*`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicMethodExtractor;

impl MethodExtractor for HeuristicMethodExtractor {
    fn extract(&self, body: &str) -> Option<Method> {
        heuristic_method(body)
    }
}

/// Conservative, pure-Rust method extraction over imperative phrasing.
///
/// Why: shared by the ticket path (default extractor) and the spec path
/// (`spec_resolve::extract_spec_method`) so "what counts as a method" is
/// defined once (spec §6.2).
/// What: scans each line for a recognised method cue and, on the first match,
/// returns a `Method` classified by cue kind with the matched line as the
/// verbatim `source_excerpt`. Cues recognised:
/// - **Constraint:** "no new dep…", "do not …", "don't …", "must not …",
///   "never …", "without …".
/// - **Reuse:** "reuse …", "use the existing …".
/// - **Approach:** a narrow `use …` directive (a known technique lead like
///   "use cursor…"/"use offset…", or an article form like "use the existing
///   X" — *not* a Rust `use std::…;` import), "cursor[- ]based pagination", a
///   "method:"/"approach:" labelled line, or "gate … behind a feature flag".
///
/// Ambiguous prose with no cue → `None` (conservative).
///
/// Test: `super::tests::extract_heuristic_*`.
#[must_use]
pub fn heuristic_method(body: &str) -> Option<Method> {
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();

        // Strip common markdown list / heading prefixes for cue matching, but
        // keep the original `line` as the verbatim excerpt.
        let cue = lower
            .trim_start_matches(['-', '*', '#', '>', ' '])
            .trim_start_matches("method:")
            .trim_start_matches("approach:")
            .trim();

        if let Some(kind) = classify_cue(&lower, cue) {
            // `text` is the prose with leading markdown list/heading markers
            // stripped, so the same method stated as `- use X` (spec list item)
            // and `use X` (ticket prose) compares equal under precedence
            // (resolve::methods_agree). `source_excerpt` keeps the line verbatim.
            let text = line
                .trim_start_matches(['-', '*', '#', '>', ' '])
                .trim()
                .to_string();
            return Some(Method {
                text,
                kind,
                source_excerpt: line.to_string(),
            });
        }
    }
    None
}

/// Classify a method cue, or return `None` when the line is not a method.
///
/// Why: keeping the cue taxonomy in one small function keeps
/// [`heuristic_method`] readable and makes the conservative bias auditable.
/// What: returns `Some(MethodKind)` for a recognised constraint/reuse/approach
/// cue; `None` otherwise. `lower` is the whole lowercased line (for phrase
/// matches); `cue` is the prefix-stripped variant (for leading-keyword matches).
/// Test: `super::tests::extract_heuristic_*`.
fn classify_cue(lower: &str, cue: &str) -> Option<MethodKind> {
    // Constraints / prohibitions — checked first (strongest signal).
    const CONSTRAINT_PHRASES: [&str; 6] = [
        "no new dep",
        "must not",
        "do not ",
        "don't ",
        "never ",
        "without adding",
    ];
    if CONSTRAINT_PHRASES.iter().any(|p| lower.contains(p)) {
        return Some(MethodKind::Constraint);
    }

    // Reuse directives.
    if cue.starts_with("reuse ")
        || lower.contains("reuse the existing")
        || lower.contains("use the existing")
    {
        return Some(MethodKind::Reuse);
    }

    // Approach / technique cues.
    //
    // The `use …` cue is deliberately NARROW: a bare `cue.starts_with("use ")`
    // would misclassify ordinary Rust import statements (`use
    // std::collections::HashMap;`) and throwaway phrasing (`use caution`) as a
    // prescribed approach. We instead require the `use` to introduce a
    // recognised approach phrasing — a known technique follow-word (`use
    // cursor`, `use offset`) or an article/determiner that signals an
    // imperative directive (`use the existing X`, `use a token bucket`). Rust
    // `use` imports never match (their next token is a `::`-qualified path), and
    // `use X;` import lines are additionally rejected by the path/semicolon
    // guard in `is_approach_use_cue`.
    if is_approach_use_cue(cue)
        || lower.contains("cursor-based pagination")
        || lower.contains("cursor pagination")
        || (lower.contains("gate") && lower.contains("feature flag"))
    {
        return Some(MethodKind::Approach);
    }

    None
}

/// Whether a `use …` cue is a *prescribed approach*, not a Rust import.
///
/// Why: `cue.starts_with("use ")` is too broad — it captures Rust import
/// statements (`use std::collections::HashMap;`) and casual phrasing (`use
/// caution`) as `MethodKind::Approach`, fabricating a method the author never
/// prescribed (spec §6.2 conservative bias).
/// What: returns `true` only when the token after `use ` is a recognised
/// approach lead — a known technique noun (`cursor`, `offset`, `pagination`,
/// `token`, `cache`, `index`, …) or an article/determiner that introduces an
/// imperative directive (`the`, `a`, `an`). Rust import lines are excluded
/// because their follow-token is a `::`-qualified path or ends in `;`.
/// Test: `super::tests::extract_heuristic_use_import_not_approach`.
fn is_approach_use_cue(cue: &str) -> bool {
    let Some(rest) = cue.strip_prefix("use ") else {
        return false;
    };
    let rest = rest.trim();
    // Reject obvious Rust import statements: a `::`-qualified path, or a line
    // that terminates in `;` (an import/use-declaration, never prose).
    if rest.contains("::") || rest.ends_with(';') {
        return false;
    }
    let Some(first) = rest.split_whitespace().next() else {
        return false;
    };
    // Strip trailing punctuation so `cursor-based` / `the,` still match.
    let head = first.trim_end_matches([',', '.', ':', ';']);
    // Articles/determiners introduce an imperative directive ("use the existing
    // trait", "use a bounded channel").
    const DETERMINERS: [&str; 3] = ["the", "a", "an"];
    // Known approach/technique nouns (extend as cues accrue; conservative set).
    const TECHNIQUE_LEADS: [&str; 8] = [
        "cursor",
        "offset",
        "pagination",
        "token",
        "cache",
        "index",
        "streaming",
        "batching",
    ];
    DETERMINERS.contains(&head) || TECHNIQUE_LEADS.iter().any(|t| head.starts_with(t))
}
