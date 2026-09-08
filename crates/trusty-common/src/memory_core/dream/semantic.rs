//! Inference-backed semantic consolidation: the idle pass and the on-demand,
//! room-scoped `dream_consolidate_room` / `palace_dream` pipeline.
//!
//! Why: split out of `cycle.rs` (#7106), which reached 459 SLOC against the
//! 500 cap and could not absorb the chunked-embedding rewrite. The two halves
//! were already independent — everything here is gated behind
//! `semantic.enabled`, which is off by default (#5188), while `cycle.rs` holds
//! the passes that run on every cycle.
//! What: `RoomConsolidationStats`, `build_consolidator_from_config`,
//! `apply_consolidation_result`, `record_provenance_and_collect_superseded`,
//! `apply_alias_and_flag_passes`, `semantic_consolidation_pass`, and
//! `consolidate_scoped`. Behaviour is unchanged by the move; the one addition
//! is the #7106 concurrency permit `consolidate_scoped` now takes.
//! Test: `dream::tests::consolidate_scoped_*`,
//! `dream::tests::dream_cycle_semantic_consolidation_with_mock`,
//! `dream::tests::apply_consolidation_result_keeps_original_when_kg_write_fails`.

use super::concurrency::acquire_dream_permit_within;
use super::config::DreamConfig;
use super::helpers::char_safe_prefix;
use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::semantic_consolidation::{
    SemanticConsolidator, resolve_consolidation_provider, validate_ollama_model,
};
use crate::memory_core::timeouts;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

/// Result of an on-demand, room-scoped consolidation (spec-001 Phase 3).
///
/// Why: the `dream_consolidate_room` MCP tool reports how much work it did so
/// the calling application can log progress / decide whether to run again.
/// What: `summary_facts_created` is the number of canonical summary drawers
/// added; `facts_evicted` is the number of superseded originals removed.
/// Test: `consolidate_scoped_*` in `dream::tests`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoomConsolidationStats {
    pub summary_facts_created: usize,
    pub facts_evicted: usize,
}

/// Build the production semantic consolidator from config, resolving the
/// provider and validating the model against it.
///
/// Why: both the idle dream pass and the on-demand room consolidation need the
/// identical "which backend, if any?" logic; sharing it keeps the decision in
/// one place. It is also where the model is validated against the resolved
/// provider (#2593), so a model id that cannot work is caught before the first
/// of many doomed network calls.
/// What: returns `Ok(None)` — a silent, per-cycle no-op — only when
/// `config.semantic.enabled` is false, which is the default (#5188). When the
/// phase IS enabled, [`resolve_consolidation_provider`] picks the backend from
/// the model id: an `ollama/`/`local/` prefix builds an `OllamaInference`
/// against the resolved local host, any other id builds an
/// `OpenRouterInference` from the resolved key, and no usable provider returns
/// `Err` naming the provider and what is missing. `Err` is also what a
/// local model id that Ollama cannot serve produces (`validate_ollama_model`).
/// Both `Err` cases mean "enabled but unusable": the caller logs once and
/// parks, rather than retrying a config that cannot change under it.
/// Test: `dream_cycle_semantic_consolidation_disabled_by_default_builds_nothing`
/// (`Ok(None)` path), `dream_semantic_no_provider_parks_without_local_fallback`
/// and `dream_cycle_semantic_consolidation_invalid_model_disables_once`
/// (`Err` paths); the env-tier read is pinned by
/// `dream::tests::dedup_only_config_ignores_an_ambient_openrouter_key`.
///
/// `pub(super)` only so those tests can call it directly — reaching it through
/// `dream_cycle` cannot distinguish "no backend was built" from "a backend was
/// built and its network call failed", which is the whole distinction they
/// exist to pin. Still private to `dream`.
pub(super) fn build_consolidator_from_config(
    config: &DreamConfig,
) -> Result<Option<Arc<SemanticConsolidator>>> {
    if !config.semantic.enabled {
        return Ok(None);
    }
    use crate::memory_core::semantic_consolidation::{
        ConsolidationProvider, OllamaInference, OpenRouterInference,
    };
    // #5188: the model id selects the backend — a local endpoint is never a
    // fallback for a missing key.
    let backend: Arc<dyn crate::memory_core::semantic_consolidation::Inference> =
        match resolve_consolidation_provider(
            &config.semantic.model,
            &config.openrouter_api_key,
            config.local_model_enabled,
        ) {
            ConsolidationProvider::Local { base_url, model } => {
                validate_ollama_model(&model)?;
                Arc::new(OllamaInference::new(base_url, model))
            }
            ConsolidationProvider::OpenRouter { api_key, model } => {
                Arc::new(OpenRouterInference::new(api_key, model))
            }
            ConsolidationProvider::Unavailable { reason, .. } => anyhow::bail!(reason),
        };
    Ok(Some(Arc::new(SemanticConsolidator::new(
        backend,
        config.semantic.clone(),
    ))))
}

/// Apply a consolidation result: add canonical drawers + record KG provenance.
///
/// Why: the canonical-add / `superseded_by` / `alias_of` / flagged-log logic is
/// identical for the idle pass and the on-demand room consolidation; extracting
/// it removes duplication and keeps side-effect ordering consistent.
/// What: for each canonical drawer, writes it via `handle.remember` and
/// delegates provenance recording to
/// [`record_provenance_and_collect_superseded`], which only marks an original
/// as evictable when its `superseded_by` triple write actually succeeded
/// (issue #1713); stores `alias_of` triples; logs flagged contradictions.
/// Returns `(canonical_count, superseded_ids)` where `superseded_ids` are the
/// original drawer ids that were folded into a canonical AND durably linked
/// via KG provenance (callers that compact — e.g. the room tool — evict
/// these).
/// Test: `dream_cycle_semantic_consolidation_with_mock`, `consolidate_scoped_*`,
/// `apply_consolidation_result_keeps_original_when_kg_write_fails`.
async fn apply_consolidation_result(
    handle: &Arc<PalaceHandle>,
    result: &crate::memory_core::semantic_consolidation::ConsolidationResult,
) -> (usize, Vec<Uuid>) {
    let mut canonical_count = 0usize;
    let mut superseded_ids: Vec<Uuid> = Vec::new();

    for canonical in &result.canonical_drawers {
        match handle
            .remember(
                canonical.content.clone(),
                RoomType::General,
                canonical.tags.clone(),
                canonical.importance,
            )
            .await
        {
            Ok(canonical_id) => {
                canonical_count += 1;
                record_provenance_and_collect_superseded(
                    handle,
                    canonical_id,
                    &canonical.canonical_for,
                    &mut superseded_ids,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(
                    // #5187: same defect as the merge cap — a `.min(80)` byte
                    // clamp splits multi-byte content and panics the pass.
                    content = char_safe_prefix(&canonical.content, 80),
                    "dream semantic: failed to add canonical drawer: {e:#}"
                );
            }
        }
    }

    apply_alias_and_flag_passes(handle, result).await;

    (canonical_count, superseded_ids)
}

/// Record `superseded_by` KG provenance for one canonical's originals, only
/// marking an original id as evictable once its triple write actually lands.
///
/// Why (issue #1713): consolidation previously pushed every original in
/// `canonical.canonical_for` onto the eviction list unconditionally, even
/// when the following `superseded_by` KG-triple write failed (only logged as
/// a warning, never propagated). Once `consolidate_scoped` started evicting
/// originals (the idle pass is additive-only and never hit this path), that
/// meant the canonical drawer could exist with no recorded provenance link
/// back to the original it replaced — the original was gone and the history
/// silently unrecoverable. Extracting this loop into its own function also
/// gives it a real, direct test seam (`pub(super)`) so the failure path can
/// be exercised without needing to fake a `remember()` failure on the same
/// handle used for the KG write.
/// What: Asserts a `superseded_by` triple per `orig_id`; appends `orig_id` to
/// `superseded_ids` (the caller's eviction-candidate accumulator) ONLY when
/// `handle.kg.assert` returns `Ok`. A failed assert is logged at WARN and the
/// original is left out of `superseded_ids` entirely, so it is never evicted
/// and remains a live, recoverable drawer.
/// Test: `apply_consolidation_result_keeps_original_when_kg_write_fails`.
pub(super) async fn record_provenance_and_collect_superseded(
    handle: &Arc<PalaceHandle>,
    canonical_id: Uuid,
    originals: &[Uuid],
    superseded_ids: &mut Vec<Uuid>,
) {
    for &orig_id in originals {
        // #5902: routed through the shared writer so the share path and this one
        // assert the same edge with the same shape. The #1713 rule — an original
        // joins `superseded_ids` only on a durable write — stays here, because
        // only this caller evicts.
        match crate::memory_core::share::assert_superseded_by(
            &handle.kg,
            orig_id,
            canonical_id,
            "dream:semantic_consolidation",
        )
        .await
        {
            Ok(()) => superseded_ids.push(orig_id),
            Err(e) => {
                tracing::warn!(
                    orig = %orig_id,
                    canonical = %canonical_id,
                    "failed to write superseded_by triple: {e:#} — original \
                     retained (no eviction without recorded provenance)"
                );
            }
        }
    }
}

/// Alias-triple and flagged-contradiction passes of `apply_consolidation_result`.
///
/// Why: split out of `apply_consolidation_result` purely to keep that
/// function's body focused on the canonical/provenance loop; no behavioural
/// change from the original inline code.
/// What: Asserts an `alias_of` triple per `(from, to)` pair (logging failures
/// at WARN, non-fatal), then logs each flagged contradiction at INFO for
/// human review.
/// Test: Covered transitively by `dream_cycle_semantic_consolidation_with_mock`
/// and `consolidate_scoped_*`.
async fn apply_alias_and_flag_passes(
    handle: &Arc<PalaceHandle>,
    result: &crate::memory_core::semantic_consolidation::ConsolidationResult,
) {
    for (from, to) in &result.aliases {
        let triple = crate::memory_core::store::kg::Triple {
            subject: from.clone(),
            predicate: "alias_of".to_string(),
            object: to.clone(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: Some("dream:semantic_consolidation".to_string()),
        };
        if let Err(e) = handle.kg.assert(triple).await {
            tracing::warn!(
                from,
                to,
                "dream semantic: failed to write alias triple: {e:#}"
            );
        }
    }

    for (id, reason) in &result.flagged_ids {
        tracing::info!(
            palace = %handle.id,
            drawer_id = %id,
            reason,
            "dream semantic: flagged drawer for human review (contradiction)"
        );
    }
}

/// Optional inference-backed semantic consolidation pass.
///
/// Why: the NLP-only passes miss semantic equivalence (aliases, paraphrases,
/// near-duplicate triples expressed differently). This phase delegates
/// canonicalization to a cheap LLM, preserving original drawers and adding
/// canonical replacements with `superseded_by` links in the KG.
/// What: returns `(0, 0, 0)` at DEBUG when `semantic.enabled` is false, which
/// is the default (#5188). Otherwise runs consolidation on all current non-Task
/// drawers, writes each canonical drawer via `handle.remember`, and records the
/// `superseded_by` KG triple so the original drawers are traceable.
/// Additive-only — originals are preserved. When
/// `build_consolidator_from_config` reports the phase as enabled but unusable —
/// no provider resolved (#5188) or a model the resolved provider cannot serve
/// (#2593) — logs ONE `warn!` naming the model, the provider, and the config
/// knob to fix, sets `disabled`, and returns `(0, 0, 0)`; every subsequent call
/// short-circuits on that flag instead of rebuilding and failing every cycle.
/// Returns `(canonical_count, llm_calls, cache_hits)`.
/// Test: `dream_cycle_semantic_consolidation_with_mock` (injected
/// consolidator); `dream_cycle_semantic_consolidation_no_inference`;
/// `dream_semantic_no_provider_parks_without_local_fallback`;
/// `dream_cycle_semantic_consolidation_invalid_model_disables_once`.
pub(super) async fn semantic_consolidation_pass(
    handle: &Arc<PalaceHandle>,
    config: &DreamConfig,
    injected: Option<Arc<SemanticConsolidator>>,
    disabled: &AtomicBool,
) -> (usize, usize, usize) {
    // The idle cycle honours the `semantic.enabled` switch even when a
    // consolidator is injected (tests rely on this): disabling the phase in
    // config must skip it entirely.
    if !config.semantic.enabled {
        tracing::debug!(
            palace = %handle.id,
            "skipping semantic consolidation: disabled in config"
        );
        return (0, 0, 0);
    }

    // Use the injected consolidator (test path) or build one from config.
    let consolidator: Arc<SemanticConsolidator> = match injected {
        Some(c) => c,
        None => {
            if disabled.load(Ordering::Relaxed) {
                // Already failed loud once for this palace's Dreamer
                // lifetime; skip without rebuilding or retrying the
                // known-bad config every cycle (issue #2593).
                return (0, 0, 0);
            }
            match build_consolidator_from_config(config) {
                Ok(Some(c)) => c,
                Ok(None) => {
                    tracing::debug!(
                        palace = %handle.id,
                        "skipping semantic consolidation: disabled in config"
                    );
                    return (0, 0, 0);
                }
                Err(e) => {
                    // #5188: one warn, then park — never a per-cycle retry of a
                    // config that cannot change while the daemon runs.
                    tracing::warn!(
                        palace = %handle.id,
                        model = %config.semantic.model,
                        "semantic consolidation disabled for this palace: {e:#}"
                    );
                    disabled.store(true, Ordering::Relaxed);
                    return (0, 0, 0);
                }
            }
        }
    };

    // spec-001: exclude protected Task drawers from consolidation entirely so
    // they are never folded into a canonical summary or superseded.
    let snapshot: Vec<Drawer> = handle
        .drawers
        .read()
        .iter()
        .filter(|d| !d.drawer_type.is_protected())
        .cloned()
        .collect();
    if snapshot.is_empty() {
        return (0, 0, 0);
    }

    let result = consolidator.consolidate(&snapshot).await;
    let (canonical_count, _superseded) = apply_consolidation_result(handle, &result).await;

    tracing::debug!(
        palace = %handle.id,
        canonical_added = canonical_count,
        aliases = result.aliases.len(),
        flagged = result.flagged_ids.len(),
        llm_calls = result.llm_calls,
        cache_hits = result.cache_hits,
        "semantic consolidation phase complete"
    );

    (canonical_count, result.llm_calls, result.cache_hits)
}

/// On-demand, room-scoped semantic consolidation that compacts older history
/// (spec-001 Phase 3, the `dream_consolidate_room` MCP tool).
///
/// Why: applications driving trusty-memory as a chat-session manager want to
/// compact a single room's older turns on demand rather than waiting for the
/// idle dream cycle, and — unlike the additive idle pass — they want the
/// superseded originals evicted so history actually shrinks.
/// What: selects non-Task drawers in `room` (or all rooms when `None`) whose
/// `created_at` is older than `max_age_days`, runs the consolidator over them,
/// applies the result (canonical drawers + KG provenance), then evicts every
/// superseded original via `handle.forget`. Returns the created/evicted counts.
/// `max_age_days <= 0` is treated as an explicit guard value (no-op, zero
/// counts): the contract is "consolidate facts *older* than N days", and a
/// non-positive window selects no history rather than the whole room — without
/// the guard a cutoff of `now` would make every drawer (even ones created this
/// instant) eligible and evict the entire room.
/// `injected` lets tests supply a `MockInference`-backed consolidator; in
/// production it is `None` and the consolidator is built from `config`. This
/// is an on-demand, user-triggered call (not the recurring background dream
/// loop), so a misconfigured model/provider combination (issue #2593) is
/// surfaced as `Err` to the caller immediately rather than silently no-op'd.
///
/// #7106: the call takes a slot in the process-wide dream bound, and waits at
/// most [`timeouts::dream_permit_wait_timeout`] for one. When every slot stays
/// busy for that long it returns [`super::concurrency::DreamBusy`] rather than
/// waiting behind idle cycles the caller cannot see. The idle loop's own wait
/// stays unbounded.
/// Test: `consolidate_scoped_filters_by_room`,
/// `consolidate_scoped_skips_task_drawers`,
/// `consolidate_scoped_no_inference_is_noop`,
/// `consolidate_scoped_non_positive_age_is_noop`,
/// `consolidate_scoped_invalid_model_returns_err` in `dream::tests`;
/// `dream::concurrency_tests::an_interactive_dream_errors_when_the_dreamer_is_busy`.
pub async fn consolidate_scoped(
    handle: &Arc<PalaceHandle>,
    config: &DreamConfig,
    room: Option<RoomType>,
    max_age_days: i64,
    injected: Option<Arc<SemanticConsolidator>>,
) -> Result<RoomConsolidationStats> {
    consolidate_scoped_within(
        handle,
        config,
        room,
        max_age_days,
        injected,
        timeouts::dream_permit_wait_timeout(),
    )
    .await
}

/// [`consolidate_scoped`] with the permit wait supplied by the caller.
///
/// Why: the #7106 contract this path gained is that the wait is BOUNDED — that
/// a busy dreamer produces [`super::concurrency::DreamBusy`] rather than a
/// hang. Proving it through the env-backed default would mean a 30 s test, and
/// overriding that env var mid-run would race every other test in the binary
/// that reads a timeout. Taking the wait as a parameter gives the contract a
/// direct, fast, env-free test seam.
/// What: identical to [`consolidate_scoped`]; `permit_wait` replaces
/// `timeouts::dream_permit_wait_timeout()`.
/// Test: `dream::concurrency_tests::an_interactive_dream_errors_when_the_dreamer_is_busy`.
pub(super) async fn consolidate_scoped_within(
    handle: &Arc<PalaceHandle>,
    config: &DreamConfig,
    room: Option<RoomType>,
    max_age_days: i64,
    injected: Option<Arc<SemanticConsolidator>>,
    permit_wait: std::time::Duration,
) -> Result<RoomConsolidationStats> {
    // Guard value: a non-positive window means "consolidate nothing" rather than
    // "consolidate everything". Return before building the consolidator so the
    // call is a true no-op (no inference backend touched).
    if max_age_days <= 0 {
        tracing::debug!(
            palace = %handle.id,
            max_age_days,
            "dream_consolidate_room: non-positive age window; no-op"
        );
        return Ok(RoomConsolidationStats::default());
    }

    // #7106: an on-demand consolidation costs the same working set as an idle
    // cycle, so it queues behind the same process-wide bound. Without this the
    // cap would be advisory — a burst of `palace_dream` calls could reproduce
    // the herd the scheduler no longer creates. The wait is BOUNDED, unlike the
    // idle loop's: a user is watching this call, and one semantic-consolidation
    // request alone is bounded at 120 s per palace, so two slow cycles ahead in
    // the queue could hold every slot for minutes. The permit releases on drop,
    // so every `?`, `return`, and panic below returns it.
    let _permit = acquire_dream_permit_within(permit_wait)
        .await
        .map_err(|busy| {
            tracing::warn!(palace = %handle.id, "dream_consolidate_room: {busy}");
            busy
        })?;

    let consolidator: Arc<SemanticConsolidator> = match injected {
        Some(c) => c,
        None => match build_consolidator_from_config(config) {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::debug!(
                    palace = %handle.id,
                    "dream_consolidate_room: inference unavailable; no-op"
                );
                return Ok(RoomConsolidationStats::default());
            }
            Err(e) => {
                // On-demand call (not a recurring background job): surface
                // the misconfiguration to the caller immediately rather than
                // silently no-op'ing or retrying (issue #2593).
                tracing::error!(
                    palace = %handle.id,
                    model = %config.semantic.model,
                    "dream_consolidate_room: semantic consolidation misconfigured: {e:#}"
                );
                return Err(e);
            }
        },
    };

    // Select candidates: room-scoped (list_drawers handles the room filter),
    // older than the age cutoff, and never protected Task drawers.
    // `max_age_days` is guaranteed positive here (the `<= 0` guard above
    // returned early), so the cutoff is strictly in the past.
    let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days);
    let snapshot: Vec<Drawer> = handle
        .list_drawers(room, None, usize::MAX)
        .into_iter()
        .filter(|d| !d.drawer_type.is_protected())
        .filter(|d| d.created_at <= cutoff)
        .collect();
    if snapshot.is_empty() {
        return Ok(RoomConsolidationStats::default());
    }

    let result = consolidator.consolidate(&snapshot).await;
    let (summary_facts_created, superseded_ids) = apply_consolidation_result(handle, &result).await;

    // Compaction step: evict the superseded originals so history shrinks.
    // Task drawers were excluded from the snapshot, so they can never appear
    // here. De-duplicate ids defensively in case two canonicals claim one.
    let mut evicted = 0usize;
    let mut seen: HashSet<Uuid> = HashSet::new();
    for id in superseded_ids {
        if !seen.insert(id) {
            continue;
        }
        // #5231: a superseded id that is no longer in the drawer table was not
        // evicted by this pass and must not be counted as one.
        match handle.forget(id).await {
            Ok(outcome) if outcome.is_deleted() => evicted += 1,
            Ok(_) => tracing::warn!(?id, "dream_consolidate_room: superseded id not in palace"),
            Err(e) => tracing::warn!(?id, "dream_consolidate_room: evict failed: {e:#}"),
        }
    }

    if let Err(e) = handle.flush() {
        tracing::warn!(palace = %handle.id, "dream_consolidate_room flush failed: {e:#}");
    }

    Ok(RoomConsolidationStats {
        summary_facts_created,
        facts_evicted: evicted,
    })
}
