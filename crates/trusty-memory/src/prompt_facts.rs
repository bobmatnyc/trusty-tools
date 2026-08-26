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
//! session and its size is therefore a hard budget (ADR-0028 D2/D8). Owns the
//! re-affirmation vocabulary too (#4890) — [`TierSFact`],
//! [`TIER_S_REAFFIRM_DAYS`], and [`stale_tier_s_facts`] — so the write gate and
//! the `doctor` staleness report read the same primitives.
//! Test: see the `tests` module — covers `is_hot_predicate`, the formatter
//! grouping/sections, the empty-input shortcut, and the staleness partition.
//! The admission gate is proven at the handler surface instead:
//! `dispatch_kg_assert_*`, `dispatch_add_alias_*`,
//! `dispatch_discover_aliases_stops_at_tier_s_cap` in `tools::tests`, and
//! `*_endpoint_enforces_tier_s_*` in `web::tests::prompt_tests`.

use crate::AppState;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};

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

/// Age at which a Tier S fact is reported as needing re-affirmation.
///
/// Why (#4890): the cap stops the surface growing; it does nothing about a
/// rule that was true when written and quietly stopped being true. Nothing in
/// the system can detect that — only the author can — so the mechanism has to
/// be a prompt to a human on a cadence, not an inference. 90 days is ADR-0028
/// D8 point 4's "quarterly": long enough that a genuinely standing rule is not
/// nagged about (months is the tier's stated cadence, D3 table), short enough
/// that a rule survives at most one quarter after it stops being true.
/// What: 90, in days. Compared against [`TierSFact::affirmed_at`] by
/// [`stale_tier_s_facts`]. It is a *reporting* threshold only — nothing in the
/// codebase retires or down-ranks a fact for crossing it, because promotion and
/// retirement are deliberate human acts (D8 point 3) and a surface that
/// silently evicted standing rules would break the same guarantee the cap
/// protects.
/// Test: `stale_tier_s_facts_partitions_at_the_threshold`.
pub const TIER_S_REAFFIRM_DAYS: i64 = 90;

/// One active Tier S fact, with the moment it was last affirmed.
///
/// The method serving the Tier S surface, one row per hot fact (#6286).
///
/// Why it lives beside [`TierSFact`] rather than in `doctor::tier_s`, which is
/// its only production caller: the name and the row type are one contract, and
/// `list_prompt_facts_endpoint_returns_hot_triples` pins them together. A test
/// reaching into `doctor`'s private module to read a string it must agree with
/// would be pinning the two halves from opposite ends of the crate.
///
/// It answers `{"facts": [...]}`, NOT a bare array — the REST route #6286
/// retired returned the array itself, so every caller unwraps `facts` now.
pub const PROMPT_FACTS_METHOD: &str = "list_prompt_facts";

/// Why (#4890): ADR-0028 D8 point 4 requires each Tier S fact to carry an
/// `affirmed_at`. It is **derived, not stored** — see [`gather_hot_facts`] for
/// why that is the stronger design rather than a shortcut. Bundling it with the
/// triple in one struct means every read surface (MCP `list_prompt_facts`, the
/// HTTP endpoint, `doctor`) reports the same value from the same source instead
/// of three call sites each deciding what "last affirmed" means.
/// What: the `(subject, predicate, object)` trio plus `affirmed_at`. Serde
/// derives are present because this struct *is* the wire shape of both read
/// surfaces; `affirmed_at` serialises as RFC 3339.
/// Test: `stale_tier_s_facts_partitions_at_the_threshold`,
/// `list_prompt_facts_endpoint_returns_hot_triples`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TierSFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// When this fact was last written or re-asserted. See
    /// [`gather_hot_facts`] for the derivation and its consequences.
    pub affirmed_at: DateTime<Utc>,
}

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
    Ok(gather_hot_facts(state)
        .await?
        .into_iter()
        .map(|f| (f.subject, f.predicate, f.object))
        .collect())
}

/// Fetch every active hot-predicate fact across every palace, each carrying
/// the moment it was last affirmed.
///
/// Why (#4890): ADR-0028 D8 point 4 says each Tier S fact "carries"
/// `affirmed_at`. It is **derived from the active row's `valid_from`** rather
/// than stored as its own column, and that is the stronger design here, not a
/// shortcut:
///
/// - It is already exactly right. `KnowledgeGraph::assert` closes the prior
///   interval and writes a fresh `valid_from` on **every** assert, including
///   one whose object is byte-identical to what is already there. So the active
///   row's `valid_from` is, by construction, "when a writer last asserted this
///   fact" — which is the definition of re-affirmation this ticket wants.
/// - **Re-asserting an identical fact counts as re-affirmation.** That is the
///   deliberate choice (#4890): retyping a rule verbatim is precisely the human
///   act D8 asks for, and demanding an edit to prove it would push authors to
///   make a cosmetic change instead of a considered one. Deriving from
///   `valid_from` makes that behaviour automatic instead of a rule six write
///   paths must each remember to honour.
/// - A stored column could only ever diverge by bug. Every hot-predicate write
///   path already stamps `valid_from: Utc::now()`, so a parallel `affirmed_at`
///   would carry the same value on every correct write and a wrong value on any
///   path that forgot it. #4895 shipped a gate with a hole for exactly that
///   reason — a write path nobody enumerated. Deriving removes the class: there
///   is no path that can forget.
/// - It costs no migration. The 93 live palaces already hold a correct value,
///   so the check is meaningful on day one instead of reporting "unknown" for
///   every pre-existing fact until it is next touched.
///
/// The cost, stated plainly: the active row cannot distinguish "the rule
/// changed" from "the same rule was re-affirmed", and the original creation
/// time moves out of the active row into history on the first re-assert. Both
/// were already true before this change — `assert` has always overwritten
/// `valid_from` — so nothing is lost that the KG still had.
///
/// A fact promoted by `discover_aliases` rather than by a person gets an
/// `affirmed_at` that no human ever set. That is a real limitation and it is
/// tracked as #4896 (whether auto-discovery should reach Tier S at all), not
/// papered over here.
///
/// What: iterates every registered palace, reads `list_active`, keeps the hot
/// predicates, and pairs each with its `valid_from`. A palace whose KG fails to
/// read is logged at `warn` and skipped — one bad palace must not blank the
/// prompt context.
/// Test: `gather_hot_triples_skips_non_hot` (integration in `tools::tests`);
/// the `affirmed_at` value round-trips through
/// `list_prompt_facts_endpoint_returns_hot_triples`.
pub async fn gather_hot_facts(state: &AppState) -> Result<Vec<TierSFact>> {
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
                        out.push(TierSFact {
                            subject: t.subject,
                            predicate: t.predicate,
                            object: t.object,
                            // #4890: the active row's `valid_from` IS the last
                            // affirmation — `assert` rewrites it on every
                            // (re-)assertion. See this function's docs.
                            affirmed_at: t.valid_from,
                        });
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

/// Partition the Tier S surface into the facts overdue for re-affirmation.
///
/// Why (#4890): the doctor check and its tests need the same answer, and the
/// answer must be computable without a daemon, a registry, or a clock the test
/// cannot control — hence `now` as a parameter rather than an internal
/// `Utc::now()`. Sorting stalest-first is not cosmetic: when the surface is at
/// its cap of 20 the operator's next action is "retire one", and the oldest
/// unreviewed rule is the one to look at first.
/// What: returns `(fact, age_in_days)` for every fact whose `affirmed_at` is
/// more than [`TIER_S_REAFFIRM_DAYS`] days before `now`, stalest first. A fact
/// affirmed exactly [`TIER_S_REAFFIRM_DAYS`] days ago is **not** stale — the
/// threshold is "unaffirmed for longer than a quarter", so the boundary day
/// itself still counts as affirmed. A fact with a future `affirmed_at` (clock
/// skew, a hand-edited row) yields a negative age and is never reported.
/// Test: `stale_tier_s_facts_partitions_at_the_threshold`.
pub fn stale_tier_s_facts(facts: &[TierSFact], now: DateTime<Utc>) -> Vec<(&TierSFact, i64)> {
    let mut stale: Vec<(&TierSFact, i64)> = facts
        .iter()
        .map(|f| (f, (now - f.affirmed_at).num_days()))
        .filter(|(_, days)| *days > TIER_S_REAFFIRM_DAYS)
        .collect();
    stale.sort_by_key(|(_, days)| std::cmp::Reverse(*days));
    stale
}

/// Render the stale-fact list as a numbered, retirement-ready block.
///
/// Why (#4890): "3 facts are stale" is a fact the operator cannot act on. To
/// re-affirm a rule they need its text; to retire it they need its
/// `(subject, predicate)` pair, because that pair — not a row id — is what
/// `remove_prompt_fact` takes. This mirrors [`render_tier_s_inventory`], which
/// makes the cap's refusal actionable for the same reason.
/// What: one `N. subject predicate → object (last affirmed <D>d ago)` line per
/// fact, 1-indexed, in the order given (stalest first).
/// Test: `render_stale_tier_s_report_names_pair_and_age`.
pub fn render_stale_tier_s_report(stale: &[(&TierSFact, i64)]) -> String {
    let mut out = String::new();
    for (i, (fact, days)) in stale.iter().enumerate() {
        out.push_str(&format!(
            "\n  {}. {} {} → {} (last affirmed {days}d ago)",
            i + 1,
            fact.subject,
            fact.predicate,
            fact.object,
        ));
    }
    out
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

/// Proof that a hot-predicate write was admitted, and the lock that keeps the
/// decision true until the write lands.
///
/// Why (#4888): the admission decision is only valid while no other writer can
/// consume the slot it counted. Returning the mutex guard rather than dropping
/// it inside the check makes "hold the lock across your write" a property the
/// borrow checker enforces at the call site instead of a comment nobody reads.
/// What: a newtype over the guard. Callers bind it (`let _admission = …?;`)
/// and let it drop at end of scope, after `kg.assert`. Cold predicates get
/// `None` and never touch the lock.
/// Test: `tier_s_cap_holds_under_concurrent_writes`.
pub struct TierSAdmission<'a>(#[allow(dead_code)] tokio::sync::MutexGuard<'a, ()>);

/// Gate a prospective hot-predicate write against the Tier S budget.
///
/// Why (#4888): Tier S reaches every turn of every agent session, so its size
/// is a hard budget, not a preference (ADR-0028 D8). Enforcement has to happen
/// at **write** time: a read-time cap would silently drop a standing rule, and
/// read-time truncation would silently corrupt one — both are worse than a
/// refused write, because the author never learns their rule is not in effect.
/// The gate therefore fails closed and says exactly what to do next.
///
/// What: returns `Ok(None)` for non-hot predicates without taking any lock.
/// For a hot predicate it takes the Tier S admission lock, then enforces:
/// 1. **Form** — `object` must be at most [`TIER_S_MAX_OBJECT_CHARS`] chars.
/// 2. **Cap** — at most [`TIER_S_MAX_FACTS`] facts may be simultaneously
///    active across *every* palace, since the surface spans all of them.
///
/// On success it returns `Some(TierSAdmission)` holding that lock. **The
/// caller must keep the returned guard alive until its `kg.assert` is
/// enqueued** — dropping it early reopens the check-then-write race the lock
/// exists to close.
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
/// `dispatch_kg_assert_rejects_object_over_char_limit`,
/// `tier_s_cap_holds_under_concurrent_writes`.
pub async fn check_tier_s_admission<'a>(
    state: &'a AppState,
    handle: &std::sync::Arc<trusty_common::memory_core::PalaceHandle>,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<Option<TierSAdmission<'a>>> {
    if !is_hot_predicate(predicate) {
        return Ok(None);
    }

    // Held for the rest of this function AND, via the returned guard, across
    // the caller's write. Acquired before the count is read so no other
    // admission can consume the slot this one is about to claim.
    let admission = state.tier_s_admission_lock.lock().await;

    // Rule 1 — form. Checked first and independently of occupancy so an
    // over-long rule is reported as over-long even when the surface is full.
    if let Some(e) = tier_s_form_error(object) {
        return Err(e);
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
        return Ok(Some(TierSAdmission(admission)));
    }

    let active = gather_hot_triples(state).await?;
    if let Some(e) = tier_s_cap_error(&active, subject, predicate) {
        return Err(e);
    }
    Ok(Some(TierSAdmission(admission)))
}

/// The form rule (ADR-0028 D2), as a pure function.
///
/// Why: the async daemon gate and the offline `kuzu-migrate` gate must apply
/// the same rule and produce the same message. Two hand-written copies would
/// drift; one primitive cannot.
/// What: `Some(error)` when `object` exceeds [`TIER_S_MAX_OBJECT_CHARS`]
/// chars, naming the actual length and the limit. `None` when it fits.
/// Test: `dispatch_kg_assert_rejects_object_over_char_limit`,
/// `dispatch_kg_assert_accepts_object_at_char_limit`.
fn tier_s_form_error(object: &str) -> Option<anyhow::Error> {
    let len = object.chars().count();
    if len <= TIER_S_MAX_OBJECT_CHARS {
        return None;
    }
    Some(anyhow!(
        "Tier S fact rejected: object is {len} characters, limit is \
         {TIER_S_MAX_OBJECT_CHARS} (ADR-0028 D2). A standing rule that does not fit \
         in {TIER_S_MAX_OBJECT_CHARS} characters is a document — put it in CLAUDE.md \
         (or a spec) and store a short pointer to it here instead. \
         Rejected object: {object:?}"
    ))
}

/// The cap rule (ADR-0028 D8), as a pure function over the active facts.
///
/// Why: same single-source-of-truth reason as [`tier_s_form_error`] — and the
/// actionable inventory is the expensive part of the message to get right, so
/// it is built in exactly one place.
/// What: `Some(error)` when `active` already holds [`TIER_S_MAX_FACTS`] or
/// more facts, naming every occupant and the tool that retires one. `None`
/// when there is room.
/// Test: `dispatch_kg_assert_rejects_twenty_first_fact`,
/// `dispatch_kg_assert_rejection_names_existing_facts`.
fn tier_s_cap_error(
    active: &[(String, String, String)],
    subject: &str,
    predicate: &str,
) -> Option<anyhow::Error> {
    if active.len() < TIER_S_MAX_FACTS {
        return None;
    }
    Some(anyhow!(
        "Tier S is full: {} of {TIER_S_MAX_FACTS} standing facts are active, so \
         `{subject} {predicate}` cannot be admitted (ADR-0028 D8). Tier S is injected \
         into every turn of every session, so the cap is a hard budget and promotion \
         is zero-sum: retire one of the facts below before adding another. \
         Retire with the `remove_prompt_fact` tool, passing the `subject` and \
         `predicate` of the row you are dropping. Currently active Tier S facts:{}",
        active.len(),
        render_tier_s_inventory(active),
    ))
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

    /// Build a `TierSFact` affirmed `days_ago` days before `now`.
    fn aged(subject: &str, days_ago: i64, now: DateTime<Utc>) -> TierSFact {
        TierSFact {
            subject: subject.into(),
            predicate: "has_convention".into(),
            object: format!("rule for {subject}"),
            affirmed_at: now - chrono::Duration::days(days_ago),
        }
    }

    /// Why (#4890): the whole check hinges on where the boundary sits, and an
    /// off-by-one here either nags about a rule affirmed this quarter or lets
    /// one slide a day past its review. It also has to survive the two inputs
    /// that are not "some number of days ago": a fact affirmed in the future
    /// (clock skew) must not be reported, and the order must be stalest-first
    /// so the operator's first line is the rule most overdue.
    /// What: five facts spanning both sides of the threshold plus a
    /// future-dated one; asserts exactly which are returned and in what order.
    /// Test: itself.
    #[test]
    fn stale_tier_s_facts_partitions_at_the_threshold() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("fixed now");
        let facts = vec![
            aged("fresh", 1, now),
            aged("ancient", 400, now),
            // Exactly at the threshold — affirmed, NOT stale.
            aged("boundary", TIER_S_REAFFIRM_DAYS, now),
            // One day past — the first stale value.
            aged("just-over", TIER_S_REAFFIRM_DAYS + 1, now),
            aged("skewed", -30, now),
        ];

        let stale = stale_tier_s_facts(&facts, now);
        let subjects: Vec<&str> = stale.iter().map(|(f, _)| f.subject.as_str()).collect();
        assert_eq!(
            subjects,
            vec!["ancient", "just-over"],
            "only facts strictly older than the threshold, stalest first"
        );
        assert_eq!(stale[0].1, 400, "age is reported in whole days");
        assert_eq!(stale[1].1, TIER_S_REAFFIRM_DAYS + 1);
    }

    /// Why (#4890): the report is the entire deliverable of a report-only
    /// check. If it names a fact without the `(subject, predicate)` pair the
    /// operator has no way to reach `remove_prompt_fact`, which is what the
    /// cap's own refusal message got right and what this must match.
    /// What: asserts the rendered line carries the subject, the predicate, the
    /// object, and the age.
    /// Test: itself.
    #[test]
    fn render_stale_tier_s_report_names_pair_and_age() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("fixed now");
        let facts = vec![aged("conv-1", 200, now)];
        let stale = stale_tier_s_facts(&facts, now);
        let out = render_stale_tier_s_report(&stale);
        assert!(out.contains("conv-1"), "must name the subject: {out}");
        assert!(
            out.contains("has_convention"),
            "must name the predicate: {out}"
        );
        assert!(out.contains("rule for conv-1"), "must show the rule: {out}");
        assert!(out.contains("200d ago"), "must show the age: {out}");
    }
}
