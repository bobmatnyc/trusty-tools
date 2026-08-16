//! Filtering and data-type helpers for `prompt-context`.
//!
//! Why: the deny-tag filter and triple overlap selector are the load-bearing
//! quality gates for recall; keeping them in a dedicated file makes unit-
//! testing and future tuning straightforward without touching the network or
//! orchestration layers.
//! What: exports `RecalledDrawer`, `RawTriple`, `filter_drawers_by_deny_tags`,
//! `filter_drawers_by_project_scope`, `filter_drawers_by_relevance_floor`,
//! `select_relevant_triples`, and `triple_overlaps`.
//!
//! Three drawer filters run in a fixed order, and the order is the contract:
//! provenance (deny tags), then project scope, then relevance. Both provenance
//! gates must run before the relevance floor so a drawer excluded for *where it
//! came from* is never also counted as "withheld below the floor" — that count
//! is rendered to the model as a promise that `memory_recall` would show more,
//! and a provenance-excluded drawer is not something the reader wants back.
//! Test: `filter_drawers_by_deny_tags_handles_edge_cases`,
//! `relevance_floor_drops_all_noise_drawers`,
//! `project_scope_drops_foreign_cwd_drawer`, and
//! `select_relevant_triples_filters_by_prompt_overlap` in `mod.rs`.

use serde_json::Value;
use trusty_common::memory_core::retrieval::{apply_relevance_floor, FloorOutcome};

/// A drawer parsed from the `/recall` endpoint's flat JSON shape.
///
/// Why: the recall endpoint hoists drawer fields to the top level (see
/// `web::recall_entry_json`), so we don't need the full `Drawer` schema —
/// only the fields the injection renders.
/// What: holds the content string, tag list, recall layer, and the recall
/// score. Implements `from_recall_entry` for safe extraction from
/// `serde_json::Value`.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
#[derive(Debug, Clone)]
pub(super) struct RecalledDrawer {
    pub(super) content: String,
    pub(super) tags: Vec<String>,
    pub(super) layer: Option<u8>,
    // #5037: the wire has always carried `score` (`service::recall_entry_json`
    // inserts it); nothing parsed it, so noise and signal were indistinguishable
    // by the time they reached the injection.
    pub(super) score: Option<f32>,
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
        // #5037: `None` here means "the daemon did not tell us" — the floor
        // keeps such an entry rather than dropping what it cannot judge.
        let score = v.get("score").and_then(|s| s.as_f64()).map(|n| n as f32);
        if content.trim().is_empty() {
            return None;
        }
        Some(Self {
            content,
            tags,
            layer,
            score,
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

/// Drop recalled drawers whose score falls below `floor`, counting them.
///
/// Why (issue #5037): the deny-tag filter above judges *provenance*; nothing
/// judged *relevance*. A probe of "what is the capital of France" against the
/// live palace returned five drawers all scoring exactly `0.15` — the
/// `L1_NO_SIMILARITY_PENALTY` given to an essential drawer the vector search
/// never returned — rendered identically to a genuine hit at `0.56`. `top_k`
/// cannot tell those apart because it is a length cap, not a quality gate.
/// What: delegates to `trusty_common`'s [`apply_relevance_floor`] so the
/// comparison and the unknown-score policy have exactly one implementation
/// shared with any other recall consumer. Returns the survivors plus the count
/// of what was withheld, which the injection renders as the "more" signal so a
/// suppressed recall is never mistaken for an empty palace.
/// Test: `relevance_floor_drops_all_noise_drawers`,
/// `relevance_floor_keeps_high_scoring_drawer`.
pub(super) fn filter_drawers_by_relevance_floor(
    drawers: Vec<RecalledDrawer>,
    floor: f32,
) -> FloorOutcome<RecalledDrawer> {
    apply_relevance_floor(drawers, floor, |d| d.score)
}

/// Filter a triple list down to knowledge-bearing triples whose subject or
/// object appears in the prompt (case-insensitive, word-ish substring match),
/// capped at `top_k`.
///
/// Why: word overlap alone judges *aboutness*, never *whether the triple says
/// anything*. Without a predicate gate the selector happily returns storage
/// plumbing: `tag:creator:client=trusty-memory-mcp --tags--> drawer:5814…`,
/// `topic:12fbc5c8… --mentioned-in--> drawer:3887…`, `room:General --contains-->
/// drawer:…`. Those are how the store indexes itself, not knowledge, and a bare
/// commit SHA is not a topic. Measured against the live `trusty-tools` palace on
/// 2026-08-17, **54 of 54** stored triples carry a structural or extraction
/// predicate and **zero** carry a hot one, so an unguarded KG section on that
/// palace is 100% noise by count. ADR-0028 C10 measured the same shape estate-
/// wide at 93.5% and D7 settled it: the injected KG section admits hot
/// predicates only, structural predicates excluded entirely.
/// What: applies [`crate::prompt_facts::is_hot_predicate`] as an allow-list
/// *before* the overlap test, then keeps any triple whose normalised subject or
/// object (split by `:` or whitespace) overlaps the prompt's word set (≥ 3
/// chars). Returns at most `top_k` entries. Nothing here filters by subject
/// shape: `tag:*`, `topic:*`, and `room:*` subjects disappear because their
/// predicates are structural, which is the one rule rather than three.
/// Test: `select_relevant_triples_filters_by_prompt_overlap`,
/// `select_relevant_triples_drops_structural_predicates`,
/// `select_relevant_triples_drops_creator_provenance_triples`.
///
/// # Spec References
/// - [ADR-0028 D7](docs/adr/0028-memory-recall-tiers-standing-current-episodic.md)
// #5817: the allow-list is the whole fix for the provenance/SHA noise.
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
        if !crate::prompt_facts::is_hot_predicate(&t.predicate) {
            continue;
        }
        if triple_overlaps(t, &words) {
            out.push(t.clone());
            if out.len() >= top_k {
                break;
            }
        }
    }
    out
}

/// Drop recalled drawers that were written while working on a different
/// project.
///
/// Why: a drawer tagged `creator:cwd=/Users/bob/Duetto/cto` — a note about
/// another repository entirely — was recalled into `trusty-tools` sessions turn
/// after turn. The drawer really does live in the `trusty-tools` palace, so
/// palace resolution cannot exclude it; the write put it in the wrong place and
/// only its own provenance tag records where it came from. Reading that tag is
/// the one signal available at recall time.
/// What: keeps every drawer that carries no `creator:cwd=` tag, and every
/// drawer whose recorded cwd sits inside `session_root`. `session_root` and each
/// recorded cwd are compared after [`normalise_project_path`], so a worktree
/// under `.claude/worktrees/` and a crate subdirectory both resolve to the same
/// root as the main checkout. Fails OPEN in every ambiguous case — absent tag,
/// absent `session_root`, unparseable path — because dropping a drawer the
/// filter cannot judge would lose real knowledge to a heuristic.
/// Test: `project_scope_drops_foreign_cwd_drawer`,
/// `project_scope_keeps_untagged_and_in_tree_drawers`,
/// `project_scope_keeps_worktree_and_subdirectory_writers`.
// #5817: cross-project leakage, read-side mitigation only — the write that put
// the drawer in this palace is the actual defect.
pub(super) fn filter_drawers_by_project_scope(
    drawers: Vec<RecalledDrawer>,
    session_root: Option<&str>,
) -> Vec<RecalledDrawer> {
    let Some(root) = session_root else {
        return drawers;
    };
    drawers
        .into_iter()
        .filter(|d| match drawer_creator_root(d) {
            Some(drawer_root) => path_contains(root, &drawer_root),
            None => true,
        })
        .collect()
}

/// Extract a drawer's normalised `creator:cwd` project path, if it records one.
///
/// Why: split out so the tag-shape handling is asserted directly rather than
/// only through the filter.
/// What: finds the first tag starting with [`crate::attribution::CREATOR_CWD_PREFIX`]
/// (case-insensitively — the tag is lowercased on some write paths) and returns
/// its value through [`normalise_project_path`]. `None` when absent or empty.
/// Test: `project_scope_drops_foreign_cwd_drawer`.
fn drawer_creator_root(d: &RecalledDrawer) -> Option<String> {
    const PREFIX: &str = crate::attribution::CREATOR_CWD_PREFIX;
    let raw = d.tags.iter().find_map(|t| {
        let (head, rest) = t.split_at_checked(PREFIX.len())?;
        head.eq_ignore_ascii_case(PREFIX).then_some(rest)
    })?;
    let normalised = normalise_project_path(raw);
    if normalised.is_empty() {
        None
    } else {
        Some(normalised)
    }
}

/// Reduce a filesystem path to the project tree it belongs to.
///
/// Why: the same project is written from three shapes of path — the main
/// checkout, a crate subdirectory under it, and an agent worktree under
/// `.claude/worktrees/<name>` (ADR-0036). Comparing raw strings would call a
/// worktree writer "another project", which is the false positive that would
/// make this filter lose real content.
/// What: lowercases, trims a trailing `/`, and truncates at the
/// `/.claude/worktrees/` segment when present. macOS is case-insensitive and
/// some write paths lowercase the tag, so the comparison is lowercase on both
/// sides.
/// Test: `project_scope_keeps_worktree_and_subdirectory_writers`.
pub(super) fn normalise_project_path(path: &str) -> String {
    let lower = path.trim().trim_end_matches('/').to_lowercase();
    match lower.find("/.claude/worktrees/") {
        Some(i) => lower[..i].to_string(),
        None => lower,
    }
}

/// Return `true` when `candidate` is `root` or sits inside it.
///
/// Why: a plain `starts_with` on strings would treat `/a/foo-other` as inside
/// `/a/foo`. The separator check is what makes the containment test a path
/// test rather than a prefix test.
/// Test: `project_scope_keeps_worktree_and_subdirectory_writers`.
fn path_contains(root: &str, candidate: &str) -> bool {
    candidate == root || candidate.starts_with(&format!("{root}/"))
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
