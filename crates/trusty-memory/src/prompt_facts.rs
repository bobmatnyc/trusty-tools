//! Prompt-facts surface: hot KG predicates exposed via a per-message tool.
//!
//! Why: Certain KG triples — aliases, project conventions, ambient facts —
//! belong in the model's working context so it doesn't have to discover them
//! via blind searches. The original design surfaced them via MCP prompts
//! (`prompts/list` + `prompts/get`) at session init, but hosts only read
//! those once per connection. Switching to a tool (`get_prompt_context`) the
//! model can invoke per-turn lets it pull fresh, query-filtered context on
//! demand without the staleness of a session-init snapshot.
//! What: Defines the `HOT_PREDICATES` allow-list, the grouping/formatting
//! logic that turns `(subject, predicate, object)` triples into a Markdown
//! context block, the `PromptFactsCache` struct holding raw triples + a
//! pre-formatted string, and helpers used by the MCP `get_prompt_context`
//! tool to fetch (and optionally filter) the cached context. Also owns the
//! Tier S injection budget (#4888) — `TIER_S_MAX_FACTS`,
//! `TIER_S_MAX_OBJECT_CHARS`, and the `check_tier_s_admission` write gate that
//! enforces both, since this surface is injected on every turn of every
//! session and its size is therefore a hard budget (ADR-0028 D2/D8).
//! Test: see the `tests` module — covers `is_hot_predicate`, the formatter
//! grouping/sections, and the empty-input shortcut. The admission gate is
//! proven at the handler surface instead: `dispatch_kg_assert_*`,
//! `dispatch_add_alias_*`, `dispatch_discover_aliases_stops_at_tier_s_cap` in
//! `tools::tests`, and `*_endpoint_enforces_tier_s_*` in
//! `web::tests::prompt_tests`.

use crate::AppState;
use anyhow::{anyhow, Context, Result};

/// Cached prompt-facts surface: raw triples and a pre-formatted Markdown block.
///
/// Why: The `get_prompt_context` tool serves two access modes — unfiltered
/// (returns the pre-formatted block directly) and filtered (re-runs the
/// formatter on a `query`-matching subset). Caching only the formatted string
/// would force a fresh `gather_hot_triples` pass for every filtered call;
/// caching only the triples would force re-formatting for every unfiltered
/// call. Holding both lets the hot path stay O(1) and the filtered path stay
/// O(n) without ever re-walking the KG.
/// What: A plain `Default + Clone` struct. `triples` holds the active
/// `(subject, predicate, object)` rows for every hot predicate across every
/// palace; `formatted` is `build_prompt_context(&triples)` cached for the
/// no-filter case.
/// Test: `rebuild_prompt_cache_populates_triples_and_formatted` (in
/// `tools::tests`); `get_prompt_context_filters_by_query`.
#[derive(Default, Clone)]
pub struct PromptFactsCache {
    /// All active hot-predicate triples: (subject, predicate, object).
    pub triples: Vec<(String, String, String)>,
    /// Pre-formatted string of all triples (used when no query filter).
    pub formatted: String,
}

/// Predicates whose currently-active triples are always included in the
/// session-init prompt context.
///
/// Why: Aliases, conventions, and standalone facts are the categories users
/// reach for when they want a model to "just know" something at the start of
/// every conversation. Other predicates (`works_at`, `lives_in`, …) are
/// retrieval-driven and don't belong in the always-on prompt.
/// What: A static slice of predicate strings; order here drives section
/// order in `build_prompt_context`.
/// Test: `is_hot_predicate_matches_listed`.
pub const HOT_PREDICATES: &[&str] = &[
    "is_alias_for",
    "has_convention",
    "is_fact",
    "is_shorthand_for",
];

/// Hard ceiling on the number of simultaneously-active Tier S facts.
///
/// Why: this is an injection *budget*, not a style preference. Every hot
/// triple is unranked, unfiltered, and placed first in the prompt block, so
/// it is paid for on **every turn of every agent session** — the one surface
/// where cost is multiplied by the entire estate's turn count. ADR-0028 D8
/// makes the cap the load-bearing control precisely because every softer
/// mechanism already failed here: the drawer estate accreted to 1,098 rows
/// with no cap and 12 expiry dates. A hard cap makes promotion zero-sum, so
/// each addition is an explicit trade rather than an accretion.
/// What: 20. Paired with [`TIER_S_MAX_OBJECT_CHARS`] this bounds the worst
/// case at 20 × 80 = 1,600 bytes, which is the Tier S row of the ADR-0028 D7
/// budget table. Enforced at write time by [`check_tier_s_admission`] — never
/// at read time, because silently dropping a standing rule is the exact
/// failure the tier exists to prevent.
/// Test: `dispatch_kg_assert_rejects_twenty_first_fact`,
/// `dispatch_kg_assert_accepts_twenty_facts`.
pub const TIER_S_MAX_FACTS: usize = 20;

/// Hard ceiling on the character length of a Tier S fact's object.
///
/// Why: a forcing function, not formatting fussiness (ADR-0028 D2). A rule
/// that cannot be stated in 80 characters is a *document*, and belongs in
/// `CLAUDE.md` with a pointer to it — not on a surface that re-transmits it
/// every turn. It also makes the budget arithmetic exact rather than
/// estimated, since count × length is a true worst case.
/// What: 80, measured in `char`s (Unicode scalar values) rather than bytes so
/// a rule written in non-ASCII is judged by the length a human sees.
/// Test: `dispatch_kg_assert_rejects_object_over_char_limit`,
/// `dispatch_kg_assert_accepts_object_at_char_limit`.
pub const TIER_S_MAX_OBJECT_CHARS: usize = 80;

/// Check whether `p` is one of the hot predicates surfaced via the prompt.
///
/// Why: Callers (the `kg_assert` dispatch, the `add_alias` tool) need to
/// decide whether a write should invalidate the prompt cache. A free
/// function avoids `HashSet` allocation for a four-element constant list.
/// What: Linear scan over `HOT_PREDICATES` — at four entries this is faster
/// than any hashed alternative and keeps the API copy-free.
/// Test: `is_hot_predicate_matches_listed`.
pub fn is_hot_predicate(p: &str) -> bool {
    HOT_PREDICATES.contains(&p)
}

/// Friendly section heading for each hot predicate.
///
/// Why: Predicate identifiers (`is_alias_for`) are machine-friendly but read
/// poorly in a prompt; "Aliases" reads naturally to a model and a human
/// auditing the prompt content.
/// What: Maps each known predicate to its display heading. Unknown
/// predicates fall back to the predicate name itself so an accidentally
/// added hot predicate still renders coherently.
/// Test: indirectly via `build_prompt_context_groups_and_formats`.
fn section_heading(predicate: &str) -> &str {
    match predicate {
        "is_alias_for" => "Aliases",
        "has_convention" => "Conventions",
        "is_fact" => "Facts",
        "is_shorthand_for" => "Shorthands",
        other => other,
    }
}

/// Build the prompt-context Markdown block from a flat list of triples.
///
/// Why: The MCP `prompts/get` handler returns a single text block; keeping
/// the formatter pure (in: `(subject, predicate, object)` tuples; out:
/// `String`) makes the cache rebuild trivial and the unit tests cheap.
/// What: Filters to hot predicates, groups by predicate in `HOT_PREDICATES`
/// order, emits a top-level header followed by a `###` section per
/// non-empty group with `- subject → object` bullets (for aliases /
/// shorthands) or `- object` bullets (for conventions / facts). Returns an
/// empty `String` when no hot triples are present, so the caller can fall
/// back to a "no context stored yet" message without inspecting the
/// internals.
/// Test: `build_prompt_context_groups_and_formats`,
/// `build_prompt_context_empty_when_no_hot_triples`.
pub fn build_prompt_context(triples: &[(String, String, String)]) -> String {
    // Filter and group preserving HOT_PREDICATES ordering.
    // `(predicate, triples-in-that-section)`; aliased to satisfy clippy's
    // `type_complexity` lint.
    type Section<'a> = (&'a str, Vec<&'a (String, String, String)>);
    let mut sections: Vec<Section<'_>> = HOT_PREDICATES.iter().map(|p| (*p, Vec::new())).collect();
    for triple in triples {
        if let Some(slot) = sections.iter_mut().find(|(p, _)| *p == triple.1.as_str()) {
            slot.1.push(triple);
        }
    }

    // Bail early when nothing matched — callers render a placeholder.
    if sections.iter().all(|(_, v)| v.is_empty()) {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("## Project Context (from memory palace)\n");
    for (predicate, items) in sections {
        if items.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str("### ");
        out.push_str(section_heading(predicate));
        out.push('\n');
        for (subject, _predicate, object) in items {
            // Aliases / shorthands read best as "short → full"; conventions
            // and facts are self-contained so we drop the subject (which is
            // typically a synthetic "convention-1" id with no value to the
            // model).
            match predicate {
                "is_alias_for" | "is_shorthand_for" => {
                    out.push_str("- ");
                    out.push_str(subject);
                    out.push_str(" → ");
                    out.push_str(object);
                    out.push('\n');
                }
                _ => {
                    out.push_str("- ");
                    out.push_str(object);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Fetch every currently-active hot-predicate triple across every palace in
/// the registry.
///
/// Why: The prompt cache surfaces context regardless of which palace stored
/// the fact, so a single MCP connection sees aliases / conventions from
/// every project namespace. Reading once into a `Vec<(String, String,
/// String)>` keeps the formatter side-effect-free and lets tests build
/// fixtures without touching redb.
/// What: Iterates every palace handle currently registered, calls
/// `list_active` with a generous limit, and filters each batch through
/// `is_hot_predicate`. A palace whose KG fails to read is logged at `warn`
/// and skipped — one bad palace must not blank the prompt context.
/// Test: `gather_hot_triples_skips_non_hot` (integration in `tools::tests`).
pub async fn gather_hot_triples(state: &AppState) -> Result<Vec<(String, String, String)>> {
    // Why: `list_active` requires a finite limit; HOT_PREDICATES facts are
    // small in count by design (aliases / conventions, not free-form
    // memory), so 1024 is generous without risking unbounded reads on a
    // misuse where someone stores thousands of "facts".
    const PER_PALACE_LIMIT: usize = 1024;

    let mut out = Vec::new();
    for palace_id in state.registry.list() {
        let handle = match state.registry.get(&palace_id) {
            Some(h) => h,
            None => continue, // raced with removal; nothing to read
        };
        match handle.kg.list_active(PER_PALACE_LIMIT, 0).await {
            Ok(triples) => {
                for t in triples {
                    if is_hot_predicate(&t.predicate) {
                        out.push((t.subject, t.predicate, t.object));
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    palace = %palace_id.as_str(),
                    "skipping palace during prompt-fact gather: {e:#}",
                );
            }
        }
    }
    Ok(out)
}

/// Render the active Tier S facts as a numbered, retirement-ready list.
///
/// Why: "cap exceeded" is an error the caller cannot act on. To retire a fact
/// the caller needs its `(subject, predicate)` pair, because that pair — not a
/// row id — is what `remove_prompt_fact` takes. Naming all 20 in the rejection
/// turns a dead end into a menu.
/// What: one `N. subject predicate → object` line per fact, 1-indexed.
/// Test: `dispatch_kg_assert_rejection_names_existing_facts`.
fn render_tier_s_inventory(triples: &[(String, String, String)]) -> String {
    let mut out = String::new();
    for (i, (subject, predicate, object)) in triples.iter().enumerate() {
        out.push_str(&format!("\n  {}. {subject} {predicate} → {object}", i + 1));
    }
    out
}

/// Gate a prospective hot-predicate write against the Tier S budget.
///
/// Why (#4888): Tier S reaches every turn of every agent session, so its size
/// is a hard budget, not a preference (ADR-0028 D8). Enforcement has to happen
/// at **write** time: a read-time cap would silently drop a standing rule, and
/// read-time truncation would silently corrupt one — both are worse than a
/// refused write, because the author never learns their rule is not in effect.
/// The gate therefore fails closed and says exactly what to do next.
///
/// What: a no-op for non-hot predicates. For a hot predicate it enforces two
/// rules in order:
/// 1. **Form** — `object` must be at most [`TIER_S_MAX_OBJECT_CHARS`] chars.
/// 2. **Cap** — at most [`TIER_S_MAX_FACTS`] facts may be simultaneously
///    active across *every* palace, since the surface spans all of them.
///
/// The cap counts only **active** triples: `gather_hot_triples` reads through
/// `list_active`, and retraction closes an interval by setting `valid_to`, so a
/// retracted or superseded fact is already excluded and cannot consume a slot.
///
/// A write whose `(subject, predicate)` is already active **in the target
/// palace** is a *replacement*, not an addition — `KnowledgeGraph::assert`
/// closes the prior interval, so occupancy is unchanged. Such writes are
/// admitted even at the cap; without this an author who filled the surface
/// could never correct a typo in an existing rule. The check is deliberately
/// palace-scoped because supersession is: the same `(subject, predicate)` in a
/// *different* palace is a genuinely new slot on the shared surface.
///
/// Test: `dispatch_kg_assert_accepts_twenty_facts`,
/// `dispatch_kg_assert_rejects_twenty_first_fact`,
/// `dispatch_kg_assert_rejection_names_existing_facts`,
/// `dispatch_kg_assert_allows_replacing_existing_fact_at_cap`,
/// `dispatch_kg_assert_retracted_fact_frees_a_slot`,
/// `dispatch_kg_assert_rejects_object_over_char_limit`.
pub async fn check_tier_s_admission(
    state: &AppState,
    handle: &std::sync::Arc<trusty_common::memory_core::PalaceHandle>,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<()> {
    if !is_hot_predicate(predicate) {
        return Ok(());
    }

    // Rule 1 — form. Checked first and independently of occupancy so an
    // over-long rule is reported as over-long even when the surface is full.
    let len = object.chars().count();
    if len > TIER_S_MAX_OBJECT_CHARS {
        return Err(anyhow!(
            "Tier S fact rejected: object is {len} characters, limit is \
             {TIER_S_MAX_OBJECT_CHARS} (ADR-0028 D2). A standing rule that does not fit \
             in {TIER_S_MAX_OBJECT_CHARS} characters is a document — put it in CLAUDE.md \
             (or a spec) and store a short pointer to it here instead. \
             Rejected object: {object:?}"
        ));
    }

    // Rule 2 — occupancy. A replacement of an already-active
    // `(subject, predicate)` in this palace supersedes rather than adds, so it
    // never grows the surface and is always admitted.
    let existing = handle
        .kg
        .query_active(subject)
        .await
        .context("kg.query_active (Tier S admission)")?;
    if existing.iter().any(|t| t.predicate == predicate) {
        return Ok(());
    }

    let active = gather_hot_triples(state).await?;
    if active.len() >= TIER_S_MAX_FACTS {
        return Err(anyhow!(
            "Tier S is full: {} of {TIER_S_MAX_FACTS} standing facts are active, so \
             `{subject} {predicate}` cannot be admitted (ADR-0028 D8). Tier S is injected \
             into every turn of every session, so the cap is a hard budget and promotion \
             is zero-sum: retire one of the facts below before adding another. \
             Retire with the `remove_prompt_fact` tool, passing the `subject` and \
             `predicate` of the row you are dropping. Currently active Tier S facts:{}",
            active.len(),
            render_tier_s_inventory(&active),
        ));
    }
    Ok(())
}

/// Refresh `AppState.prompt_context_cache` from the live palace registry.
///
/// Why: Every write that touches a hot predicate (`kg_assert`, `add_alias`,
/// `remove_prompt_fact`) must update the cache so the next
/// `get_prompt_context` tool call returns the fresh content. Centralising
/// the refresh here means the dispatch sites only have to call one function.
/// The lock is `tokio::sync::RwLock` (issue #229) so the rebuild yields to
/// the runtime instead of blocking a worker thread for the full KG walk.
/// What: Calls `gather_hot_triples`, formats via `build_prompt_context`,
/// then takes the cache's async write lock and replaces both the raw
/// triples and the pre-formatted string in a single assignment. The write
/// is non-blocking from the caller's perspective: the lock is held only
/// for the assignment, not the gather/format work.
/// Test: `rebuild_prompt_cache_reflects_writes` (in `tools::tests`).
pub async fn rebuild_prompt_cache(state: &AppState) -> Result<()> {
    let triples = gather_hot_triples(state).await?;
    let formatted = build_prompt_context(&triples);
    let cache = state.prompt_context_cache.clone();
    let mut guard = cache.write().await;
    *guard = PromptFactsCache { triples, formatted };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_hot_predicate_matches_listed() {
        for p in HOT_PREDICATES {
            assert!(is_hot_predicate(p), "expected hot: {p}");
        }
        assert!(!is_hot_predicate("works_at"));
        assert!(!is_hot_predicate(""));
    }

    #[test]
    fn build_prompt_context_empty_when_no_hot_triples() {
        let triples: Vec<(String, String, String)> = vec![
            ("alice".into(), "works_at".into(), "Acme".into()),
            ("bob".into(), "lives_in".into(), "Paris".into()),
        ];
        assert_eq!(build_prompt_context(&triples), "");
    }

    #[test]
    fn build_prompt_context_groups_and_formats() {
        let triples: Vec<(String, String, String)> = vec![
            (
                "tga".into(),
                "is_alias_for".into(),
                "trusty-git-analytics".into(),
            ),
            ("tm".into(), "is_alias_for".into(), "trusty-memory".into()),
            (
                "conv-1".into(),
                "has_convention".into(),
                "No unwrap() in library code".into(),
            ),
            ("fact-1".into(), "is_fact".into(), "MSRV is 1.88".into()),
            // Non-hot — must be ignored entirely.
            ("alice".into(), "works_at".into(), "Acme".into()),
        ];
        let out = build_prompt_context(&triples);
        assert!(out.starts_with("## Project Context (from memory palace)"));
        assert!(out.contains("### Aliases"));
        assert!(out.contains("- tga → trusty-git-analytics"));
        assert!(out.contains("- tm → trusty-memory"));
        assert!(out.contains("### Conventions"));
        assert!(out.contains("- No unwrap() in library code"));
        assert!(out.contains("### Facts"));
        assert!(out.contains("- MSRV is 1.88"));
        // Non-hot triple omitted.
        assert!(!out.contains("Acme"));
        // Aliases section must come before Conventions (HOT_PREDICATES order).
        let aliases_idx = out.find("### Aliases").unwrap();
        let conventions_idx = out.find("### Conventions").unwrap();
        let facts_idx = out.find("### Facts").unwrap();
        assert!(aliases_idx < conventions_idx);
        assert!(conventions_idx < facts_idx);
    }
}
