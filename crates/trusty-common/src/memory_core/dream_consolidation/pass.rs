//! The tool-calling dream-consolidation pass: cluster → LLM tool call →
//! summary drawer + KG facts + tombstones (epic #2866).
//!
//! Why: The legacy `semantic_consolidation_pass` free-text path adds
//! canonical drawers but never hides the originals, so palaces only grow.
//! This pass uses the existing `ChatProvider` tool-calling surface for a
//! typed contract, stores the structured result, and tombstones the source
//! drawers via `superseded_by` triples so default recall stops surfacing
//! superseded content.
//! What: `dream_consolidation_pass` (fail-open orchestrator, never returns
//! an error), `build_provider_from_config` (mirrors the legacy pass's
//! provider-resolution precedent), cluster building (room-snapshot v0,
//! per Bob's 2026-07-16 decision), one-tool-call collection, and the
//! summary/facts/tombstone storage sequence (tombstone is always the LAST
//! write — spec §6 partial-failure ordering).
//! Test: `dream_consolidation::tests` — full mock-provider pass, fail-open
//! paths (disabled / no provider / provider error), recall filtering.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::ChatMessage;
use crate::chat::{ChatEvent, ChatProvider, ToolCall};
use crate::memory_core::dream::DreamConfig;
use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::semantic_consolidation::{build_consolidation_prompt, inference_available};
use crate::memory_core::store::kg::Triple;

use super::types::{
    ConsolidationOutput, DREAM_CONSOLIDATION_PROVENANCE, DREAM_SUMMARY_TAG,
    DreamConsolidationStats, EMIT_CONSOLIDATION_TOOL, SUPERSEDED_BY_PREDICATE,
    emit_consolidation_tool, parse_emit_consolidation,
};

/// Hard wall-clock ceiling for one cluster's LLM invocation.
///
/// Why: The providers carry their own 120s request timeouts, but the pass
/// must never be able to wedge a dream cycle if a provider implementation
/// misbehaves; a belt-and-braces outer timeout guarantees forward progress.
/// What: 180 seconds per cluster invocation, enforced with `tokio::time::timeout`.
/// Test: Implicit — every mock-provider test completes well inside it.
const CLUSTER_CALL_TIMEOUT: Duration = Duration::from_secs(180);

/// Outcome of one cluster's model invocation.
enum ClusterOutcome {
    /// The model called `emit_consolidation`; carries the raw tool call.
    ToolCall(ToolCall),
    /// The stream completed without a tool call — a defined no-op.
    NoToolCall,
    /// Provider error / stream failure / timeout — swallowed fail-open.
    Failed,
}

/// Run the tool-calling consolidation pass over a palace (or one room).
///
/// Why: Single entry point for both the idle dream cycle and the on-demand
/// `dream_consolidate_room` MCP tool. FAIL-OPEN is the contract: disabled
/// config, missing provider, provider/network/parse errors, and storage
/// failures must all degrade to counters in the returned stats — this
/// function can never propagate an error or panic the dream cycle.
/// What: Gates on `config.consolidation.enabled` (even when a provider is
/// injected — tests rely on this, mirroring the legacy pass); resolves the
/// provider (`injected` first, else `build_provider_from_config`); snapshots
/// live drawers (`room`-scoped when given) excluding protected AND
/// already-archived drawers; groups by room (cluster v0 = room snapshot,
/// Bob 2026-07-16) and chunks each room by `max_batch_size`; invokes the
/// model once per cluster under the `max_calls_per_cycle` budget; stores
/// each validated result via [`apply_cluster_output`]. Returns telemetry.
/// Test: `pass_stores_summary_facts_and_tombstones`,
/// `pass_disabled_is_noop`, `pass_without_provider_is_noop`,
/// `pass_swallows_provider_error`, `pass_counts_no_tool_call_as_noop`.
pub async fn dream_consolidation_pass(
    handle: &Arc<PalaceHandle>,
    config: &DreamConfig,
    room: Option<RoomType>,
    injected: Option<Arc<dyn ChatProvider>>,
) -> DreamConsolidationStats {
    let mut stats = DreamConsolidationStats::default();

    if !config.consolidation.enabled {
        tracing::debug!(
            palace = %handle.id,
            "dream consolidation: disabled in config; skipping"
        );
        return stats;
    }

    let provider: Arc<dyn ChatProvider> =
        match injected.or_else(|| build_provider_from_config(config)) {
            Some(p) => p,
            None => {
                tracing::debug!(
                    palace = %handle.id,
                    "dream consolidation: no provider available; skipping"
                );
                return stats;
            }
        };

    // Archived-id preload: one bulk KG scan per pass (spec §4.3 mitigation)
    // so tombstoned drawers are never re-clustered. Fail-open: on KG error
    // the helper returns an empty set and the pass proceeds.
    let archived = handle.kg.archived_drawer_ids().await;

    let snapshot: Vec<Drawer> = handle
        .list_drawers(room, None, usize::MAX)
        .into_iter()
        .filter(|d| !d.drawer_type.is_protected())
        .filter(|d| !archived.contains(&d.id))
        .collect();
    if snapshot.is_empty() {
        return stats;
    }

    // Cluster v0 = room snapshot: group by room_id (BTreeMap for
    // deterministic iteration order), then fixed-size chunks per room.
    let mut by_room: BTreeMap<Uuid, Vec<Drawer>> = BTreeMap::new();
    for d in snapshot {
        by_room.entry(d.room_id).or_default().push(d);
    }

    let batch_size = config.consolidation.max_batch_size.max(1);
    'outer: for drawers in by_room.values() {
        for cluster in drawers.chunks(batch_size) {
            if stats.llm_calls >= config.consolidation.max_calls_per_cycle {
                tracing::debug!(
                    palace = %handle.id,
                    budget = config.consolidation.max_calls_per_cycle,
                    "dream consolidation: call budget exhausted"
                );
                break 'outer;
            }
            stats.clusters_processed += 1;
            stats.llm_calls += 1;

            match invoke_cluster(&provider, cluster).await {
                ClusterOutcome::ToolCall(tc) => match parse_emit_consolidation(&tc.arguments) {
                    Ok(output) => {
                        apply_cluster_output(handle, cluster, output, &mut stats).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            palace = %handle.id,
                            error = %e,
                            raw = %tc.arguments,
                            "dream consolidation: invalid tool-call arguments; skipping cluster"
                        );
                        stats.errors += 1;
                    }
                },
                ClusterOutcome::NoToolCall => {
                    tracing::debug!(
                        palace = %handle.id,
                        cluster_size = cluster.len(),
                        "dream consolidation: model made no tool call; cluster is a no-op"
                    );
                    stats.no_tool_call += 1;
                }
                ClusterOutcome::Failed => {
                    stats.errors += 1;
                }
            }
        }
    }

    tracing::debug!(
        palace = %handle.id,
        clusters = stats.clusters_processed,
        llm_calls = stats.llm_calls,
        summaries = stats.summaries_created,
        facts = stats.facts_asserted,
        tombstoned = stats.sources_tombstoned,
        no_tool_call = stats.no_tool_call,
        errors = stats.errors,
        "dream consolidation pass complete"
    );
    stats
}

/// Build a `ChatProvider` from dream config, gating on inference availability.
///
/// Why: The pass needs the same provider-resolution precedent the legacy
/// consolidator uses (`build_consolidator_from_config`, `dream/cycle.rs`) so
/// operators configure exactly one backend for both passes.
/// What: Returns `None` when no backend is available (fail-open gate).
/// Key resolution: `config.openrouter_api_key`, else the
/// `OPENROUTER_API_KEY` env var. Backend: local model enabled AND no key →
/// `OllamaProvider` at the standard localhost binding; otherwise
/// `OpenRouterProvider`. Model comes from `config.consolidation.model`.
/// Test: `pass_without_provider_is_noop` (None path); the Some path is
/// covered in production and by provider unit tests in `chat::openai_compat`.
fn build_provider_from_config(config: &DreamConfig) -> Option<Arc<dyn ChatProvider>> {
    let api_key = if !config.openrouter_api_key.is_empty() {
        config.openrouter_api_key.clone()
    } else {
        std::env::var(crate::env_vars::ENV_OPENROUTER_API_KEY).unwrap_or_default()
    };
    if !inference_available(&api_key, config.local_model_enabled) {
        return None;
    }
    if config.local_model_enabled && api_key.is_empty() {
        Some(Arc::new(crate::chat::OllamaProvider::new(
            "http://localhost:11434",
            &config.consolidation.model,
        )))
    } else {
        Some(Arc::new(crate::chat::OpenRouterProvider::new(
            api_key,
            &config.consolidation.model,
        )))
    }
}

/// Invoke the model once for a cluster and collect its single tool call.
///
/// Why: Isolates all streaming/channel mechanics so the pass body reads as
/// a plain state machine over `ClusterOutcome`.
/// What: Builds the prompt with the shared `build_consolidation_prompt`
/// drawer formatting, offers exactly one tool (`emit_consolidation`), spawns
/// `chat_stream` into a bounded channel, and drains events under
/// [`CLUSTER_CALL_TIMEOUT`]. First matching `ToolCall` wins; a
/// `ChatEvent::Error`, stream error, or timeout without a tool call yields
/// `Failed`; a clean stream without a tool call yields `NoToolCall`.
/// Test: All `pass_*` tests drive this through the scripted mock provider.
async fn invoke_cluster(provider: &Arc<dyn ChatProvider>, cluster: &[Drawer]) -> ClusterOutcome {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: "You consolidate clusters of related memories. Read every memory \
                      below, then call the emit_consolidation tool exactly once with a \
                      lossless summary, any cross-memory inferences, and any \
                      subject-predicate-object facts you are confident in. Do not reply \
                      with plain text."
                .to_string(),
            tool_call_id: None,
            tool_calls: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: build_consolidation_prompt(cluster),
            tool_call_id: None,
            tool_calls: None,
        },
    ];

    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(32);
    let p = provider.clone();
    let stream_task = tokio::spawn(async move {
        p.chat_stream(messages, vec![emit_consolidation_tool()], tx)
            .await
    });

    let mut tool_call: Option<ToolCall> = None;
    let mut stream_errored = false;

    let drained = tokio::time::timeout(CLUSTER_CALL_TIMEOUT, async {
        while let Some(ev) = rx.recv().await {
            match ev {
                ChatEvent::ToolCall(tc) if tc.name == EMIT_CONSOLIDATION_TOOL => {
                    if tool_call.is_none() {
                        tool_call = Some(tc);
                    }
                }
                ChatEvent::Error(msg) => {
                    tracing::warn!(error = %msg, "dream consolidation: provider stream error");
                    stream_errored = true;
                }
                ChatEvent::Done => break,
                _ => {}
            }
        }
    })
    .await;

    if drained.is_err() {
        tracing::warn!("dream consolidation: cluster invocation timed out");
        stream_task.abort();
        return ClusterOutcome::Failed;
    }
    // Drop the receiver so a provider still sending never blocks the join.
    drop(rx);
    match stream_task.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "dream consolidation: chat_stream failed");
            stream_errored = true;
        }
        Err(e) => {
            tracing::warn!(error = %e, "dream consolidation: stream task join error");
            stream_errored = true;
        }
    }

    match tool_call {
        // A completed tool call is usable even if the stream errored later.
        Some(tc) => ClusterOutcome::ToolCall(tc),
        None if stream_errored => ClusterOutcome::Failed,
        None => ClusterOutcome::NoToolCall,
    }
}

/// Store one validated cluster result: summary drawer, KG facts, tombstones.
///
/// Why: Centralises the spec §6 write ordering — the tombstone must be the
/// LAST write so a crash mid-cluster can only ever leave sources fully live
/// (safe redundancy), never archived without a reachable summary.
/// What: 1) `handle.remember` the summary (inferences folded in under an
/// "Inferences" heading, source ids under a "Sources" trailer, tagged
/// [`DREAM_SUMMARY_TAG`], importance = max source importance); on failure the
/// whole cluster is abandoned (no facts, no tombstones). 2) Assert each fact
/// with [`DREAM_CONSOLIDATION_PROVENANCE`] and the validated confidence.
/// 3) Assert one `superseded_by` triple per source drawer — sources are
/// never deleted. Individual triple failures are logged and counted; a
/// source whose tombstone write fails simply stays live.
/// Test: `pass_stores_summary_facts_and_tombstones`,
/// `summary_body_folds_inferences_and_sources`.
async fn apply_cluster_output(
    handle: &Arc<PalaceHandle>,
    sources: &[Drawer],
    output: ConsolidationOutput,
    stats: &mut DreamConsolidationStats,
) {
    stats.facts_dropped += output.facts_dropped;

    let mut body = output.summary.clone();
    if !output.inferences.is_empty() {
        body.push_str("\n\nInferences:\n");
        for inf in &output.inferences {
            body.push_str("- ");
            body.push_str(inf);
            body.push('\n');
        }
    }
    let source_ids: Vec<String> = sources.iter().map(|d| d.id.to_string()).collect();
    body.push_str("\n\nSources: ");
    body.push_str(&source_ids.join(", "));

    let importance = sources.iter().map(|d| d.importance).fold(0.5f32, f32::max);

    let summary_id = match handle
        .remember(
            body,
            RoomType::General,
            vec![DREAM_SUMMARY_TAG.to_string()],
            importance,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                palace = %handle.id,
                "dream consolidation: failed to store summary drawer: {e:#} — \
                 cluster abandoned (no facts asserted, no sources tombstoned)"
            );
            stats.errors += 1;
            return;
        }
    };
    stats.summaries_created += 1;
    stats.inferences_recorded += output.inferences.len();

    for fact in &output.facts {
        let triple = Triple {
            subject: fact.subject.clone(),
            predicate: fact.predicate.clone(),
            object: fact.object.clone(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: fact.confidence,
            provenance: Some(DREAM_CONSOLIDATION_PROVENANCE.to_string()),
        };
        match handle.kg.assert(triple).await {
            Ok(()) => stats.facts_asserted += 1,
            Err(e) => {
                tracing::warn!(
                    palace = %handle.id,
                    subject = %fact.subject,
                    "dream consolidation: fact assert failed: {e:#}"
                );
                stats.errors += 1;
            }
        }
    }

    // Tombstones LAST (spec §6): only after the summary exists and facts are
    // in. A failed tombstone leaves that source live — never the reverse.
    for src in sources {
        let triple = Triple {
            subject: format!("drawer:{}", src.id),
            predicate: SUPERSEDED_BY_PREDICATE.to_string(),
            object: format!("drawer:{summary_id}"),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: Some(DREAM_CONSOLIDATION_PROVENANCE.to_string()),
        };
        match handle.kg.assert(triple).await {
            Ok(()) => stats.sources_tombstoned += 1,
            Err(e) => {
                tracing::warn!(
                    palace = %handle.id,
                    source = %src.id,
                    summary = %summary_id,
                    "dream consolidation: tombstone assert failed: {e:#} — source stays live"
                );
                stats.errors += 1;
            }
        }
    }
}
