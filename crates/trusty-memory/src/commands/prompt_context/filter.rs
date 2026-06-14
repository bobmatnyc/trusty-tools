//! Filtering and data-type helpers for `prompt-context`.
//!
//! Why: the deny-tag filter and triple overlap selector are the load-bearing
//! quality gates for recall; keeping them in a dedicated file makes unit-
//! testing and future tuning straightforward without touching the network or
//! orchestration layers.
//! What: exports `RecalledDrawer`, `RawTriple`, `filter_drawers_by_deny_tags`,
//! `select_relevant_triples`, and `triple_overlaps`.
//! Test: `filter_drawers_by_deny_tags_handles_edge_cases` and
//! `select_relevant_triples_filters_by_prompt_overlap` in `mod.rs`.

use serde_json::Value;

/// A drawer parsed from the `/recall` endpoint's flat JSON shape.
///
/// Why: the recall endpoint hoists drawer fields to the top level (see
/// `web::recall_entry_json`), so we don't need the full `Drawer` schema —
/// only the fields the injection renders.
/// What: holds the content string, tag list, and recall layer. Implements
/// `from_recall_entry` for safe extraction from `serde_json::Value`.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
#[derive(Debug, Clone)]
pub(super) struct RecalledDrawer {
    pub(super) content: String,
    pub(super) tags: Vec<String>,
    pub(super) layer: Option<u8>,
}

impl RecalledDrawer {
    /// Parse a drawer from a `/recall` JSON entry.
    ///
    /// Why: centralises safe extraction so callers use `filter_map` without
    /// inline field-access boilerplate.
    /// What: returns `Some` when `content` is present and non-empty; `None`
    /// otherwise so malformed entries are silently skipped.
    /// Test: indirectly via fetch integration tests.
    pub(super) fn from_recall_entry(v: &Value) -> Option<Self> {
        let content = v.get("content")?.as_str()?.to_string();
        let tags = v
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let layer = v.get("layer").and_then(|l| l.as_u64()).map(|n| n as u8);
        if content.trim().is_empty() {
            return None;
        }
        Some(Self {
            content,
            tags,
            layer,
        })
    }
}

/// A KG triple parsed from the `/kg/all` endpoint.
///
/// Why: same as [`RecalledDrawer`] — the daemon's Triple JSON has more
/// fields (timestamps, confidence, provenance) than the injection needs.
/// What: subject/predicate/object as owned strings.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
#[derive(Debug, Clone)]
pub(super) struct RawTriple {
    pub(super) subject: String,
    pub(super) predicate: String,
    pub(super) object: String,
}

impl RawTriple {
    /// Parse a triple from a KG JSON entry.
    ///
    /// Why: centralises field extraction so callers use `filter_map`.
    /// What: returns `Some` when all three fields are present strings; `None`
    /// on any missing field so malformed entries are silently skipped.
    /// Test: indirectly via `select_relevant_triples_filters_by_prompt_overlap`.
    pub(super) fn from_value(v: &Value) -> Option<Self> {
        let subject = v.get("subject")?.as_str()?.to_string();
        let predicate = v.get("predicate")?.as_str()?.to_string();
        let object = v.get("object")?.as_str()?.to_string();
        Some(Self {
            subject,
            predicate,
            object,
        })
    }
}

/// Filter recalled drawers, dropping any whose tag list intersects with
/// `deny_tags`.
///
/// Why (issue #139): the live trusty-tools palace was injecting raw past
/// user prompts ("yes", "status?", "let's minor version bump") on every
/// `UserPromptSubmit` because the recall result was dominated by drawers
/// tagged `claude-session` / `user-prompt` from an upstream auto-capture
/// hook. Tag-based exclusion is the cheapest and lowest-risk fix — empty
/// tag lists pass through, and the global hot-facts fallback still kicks
/// in when filtering empties the result.
/// What: returns a new `Vec<RecalledDrawer>` containing only the drawers
/// whose tags do NOT contain any entry from `deny_tags` (case-insensitive
/// match). If `deny_tags` is empty, returns the input unchanged. If a
/// drawer has no tags, it is always kept (no excluded tag can match).
/// Failure isolation: never panics — case folding uses `to_lowercase` which
/// allocates but cannot fail.
/// Test: `prompt_context_recall_filters_deny_tags`,
/// `prompt_context_recall_env_override_extends_deny_list`,
/// `prompt_context_recall_all_filtered_falls_back_to_global`.
pub(super) fn filter_drawers_by_deny_tags(
    drawers: Vec<RecalledDrawer>,
    deny_tags: &[String],
) -> Vec<RecalledDrawer> {
    if deny_tags.is_empty() {
        return drawers;
    }
    drawers
        .into_iter()
        .filter(|d| {
            // Treat missing / empty tag lists as "no excluded tag can match"
            // — we keep the drawer rather than discard it on absence of
            // metadata.
            if d.tags.is_empty() {
                return true;
            }
            !d.tags
                .iter()
                .any(|t| deny_tags.iter().any(|deny| deny.eq_ignore_ascii_case(t)))
        })
        .collect()
}

/// Filter a triple list down to those whose subject or object appears in
/// the prompt (case-insensitive, word-ish substring match), capped at
/// `top_k`.
///
/// Why: dumping every active triple would shred the byte budget on a
/// palace with hundreds of triples. Limiting to subjects/objects the user
/// actually mentioned keeps signal high and noise low. We accept both
/// directions (`subject ∈ prompt` and `object ∈ prompt`) so a query like
/// "what is tga?" matches `tga is_alias_for trusty-git-analytics`.
/// What: lowercase-tokenises the prompt into a `HashSet<String>` of words
/// (≥ 3 chars), then keeps any triple whose normalised subject or object
/// (split by `:` or whitespace) overlaps the set. Returns at most `top_k`
/// entries.
/// Test: `select_relevant_triples_filters_by_prompt_overlap`.
pub(super) fn select_relevant_triples(
    triples: &[RawTriple],
    prompt: &str,
    top_k: usize,
) -> Vec<RawTriple> {
    use std::collections::HashSet;
    let words: HashSet<String> = prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<RawTriple> = Vec::with_capacity(top_k);
    for t in triples {
        if triple_overlaps(t, &words) {
            out.push(t.clone());
            if out.len() >= top_k {
                break;
            }
        }
    }
    out
}

/// Return `true` when any normalised token of a triple's subject or object
/// is present in the prompt's word set.
///
/// Why: factored from `select_relevant_triples` to be unit-testable and to
/// keep the loop body small.
/// What: splits both subject and object by common delimiters and checks each
/// token (≥ 3 chars) against the word set.
/// Test: indirectly via `select_relevant_triples_filters_by_prompt_overlap`.
pub(super) fn triple_overlaps(
    t: &RawTriple,
    prompt_words: &std::collections::HashSet<String>,
) -> bool {
    let candidates = [t.subject.as_str(), t.object.as_str()];
    for candidate in candidates {
        for tok in candidate
            .to_lowercase()
            .split(|c: char| c == ':' || c.is_whitespace() || c == '_' || c == '-' || c == '/')
        {
            if tok.len() >= 3 && prompt_words.contains(tok) {
                return true;
            }
        }
    }
    false
}
