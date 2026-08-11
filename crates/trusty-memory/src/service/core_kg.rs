//! `MemoryService` knowledge-graph / dream / activity methods.
//!
//! Why: the `MemoryService` impl exceeded the 500-SLOC production cap once the
//! former monolithic `service.rs` was split (issue #607); its KG, dream-cycle,
//! and activity-listing methods form a cohesive second half hosted here.
//! What: a continuation `impl MemoryService` block whose methods were moved
//! verbatim from the original single impl.
//! Test: covered by the corresponding `web::tests` / `service::tests`.

use crate::{ActivityFilter, ActivitySource, DaemonEvent};
use std::sync::Arc;
use trusty_common::memory_core::dream::{DreamConfig, Dreamer, PersistedDreamStats};
use trusty_common::memory_core::palace::PalaceId;
// #4670: ExpandDirection drives the progressive `kg/graph/neighbors` route.
use trusty_common::memory_core::store::kg::{ExpandDirection, Triple};
use trusty_common::memory_core::{PalaceHandle, PalaceRegistry};

use super::core::KG_GRAPH_MAX_TRIPLES;
use super::helpers::{list_palaces_blocking, refresh_gaps_cache};
use super::types::{
    DreamStatusPayload, KgAssertBody, KgGraphPayload, KgNeighborsPayload, KgNodeView,
    KgSeedPayload, ServiceError, ServiceResult,
};
use super::MemoryService;

// ---------------------------------------------------------------------------
// KG list bounds
// ---------------------------------------------------------------------------
//
// #4776: these live on the service layer, not on either consumer, because both
// readers of the subject list must agree on them — the HTTP explorer routes in
// `web::kg_routes` (compiled only under `axum-server`) and the MCP
// `kg_list_subjects` tool in `tools::kg_ops` (always compiled). Defining them
// in `web` would put them on the far side of that feature gate from the tool.

/// Default page size for KG subject listings when the caller omits `limit`.
///
/// Why: 50 is large enough to feel responsive in the KG Explorer and to answer
/// "what is in this graph?" in one call, without dumping a full graph.
pub(crate) const DEFAULT_KG_LIST_LIMIT: usize = 50;

/// Hard ceiling on `limit` for KG subject listings.
///
/// Why: prevent a misconfigured client from asking the daemon to materialize
/// thousands of rows in one go; matches the spec's max=200.
pub(crate) const MAX_KG_LIST_LIMIT: usize = 200;

impl MemoryService {
    // -----------------------------------------------------------------
    // Knowledge graph
    // -----------------------------------------------------------------

    /// Query the KG for all active triples whose subject matches.
    pub async fn kg_query(&self, id: &str, subject: &str) -> ServiceResult<Vec<Triple>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .query_active(subject)
            .await
            .map_err(|e| ServiceError::internal(format!("kg query: {e:#}")))
    }

    /// Assert a triple in the KG.
    ///
    /// #4888: this is the HTTP counterpart of the `kg_assert` MCP tool
    /// (`POST /api/v1/palaces/{id}/kg`) and accepts an arbitrary predicate, so
    /// it can write a hot predicate and must carry the same Tier S gate. It is
    /// not a hypothetical path — `trusty-mpm`'s provisioner seeds its identity
    /// fact through exactly this endpoint. A gate on the MCP tools alone would
    /// read as protection while leaving the surface writable.
    pub async fn kg_assert(&self, id: &str, body: KgAssertBody) -> ServiceResult<()> {
        let handle = self.open_handle(id)?;
        // `_admission` holds the admission lock until after `kg.assert` below.
        let _admission = crate::prompt_facts::check_tier_s_admission(
            &self.state,
            &handle,
            &body.subject,
            &body.predicate,
            &body.object,
        )
        .await
        .map_err(|e| ServiceError::bad_request(format!("{e:#}")))?;
        let triple = Triple {
            subject: body.subject,
            predicate: body.predicate,
            object: body.object,
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: body.confidence.unwrap_or(1.0),
            provenance: body.provenance,
        };
        handle
            .kg
            .assert(triple)
            .await
            .map_err(|e| ServiceError::internal(format!("kg assert: {e:#}")))
    }

    /// Retract the single active triple identified by `(subject, predicate)`.
    ///
    /// Why: Issue #278 — the `DELETE /kg/triples/<id>` HTTP endpoint needs a
    /// service-layer method so the HTTP handler stays a thin adapter.
    /// What: Opens the palace handle, calls `KnowledgeGraph::retract`, and
    /// maps the closed count to a 204/404 signal: `Ok(true)` when at least
    /// one interval was closed, `Ok(false)` when no active triple matched.
    /// Test: Covered by `kg_delete_triple_returns_204_on_success` in
    /// `web::tests`.
    pub async fn kg_retract_triple(
        &self,
        id: &str,
        subject: &str,
        predicate: &str,
    ) -> ServiceResult<bool> {
        let handle = self.open_handle(id)?;
        let closed = handle
            .kg
            .retract(subject, predicate)
            .await
            .map_err(|e| ServiceError::internal(format!("kg retract: {e:#}")))?;
        Ok(closed > 0)
    }

    /// List distinct subjects in the KG.
    pub async fn kg_list_subjects(&self, id: &str, limit: usize) -> ServiceResult<Vec<String>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .list_subjects(limit)
            .map_err(|e| ServiceError::internal(format!("kg list_subjects: {e:#}")))
    }

    /// List distinct subjects in the KG paired with their active-triple count.
    pub async fn kg_list_subjects_with_counts(
        &self,
        id: &str,
        limit: usize,
    ) -> ServiceResult<Vec<(String, u64)>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .list_subjects_with_counts(limit)
            .map_err(|e| ServiceError::internal(format!("kg list_subjects_with_counts: {e:#}")))
    }

    /// Page through every active triple.
    pub async fn kg_list_all(
        &self,
        id: &str,
        limit: usize,
        offset: usize,
    ) -> ServiceResult<Vec<Triple>> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .list_active(limit, offset)
            .await
            .map_err(|e| ServiceError::internal(format!("kg list_active: {e:#}")))
    }

    /// Return the count of currently-active triples.
    ///
    /// #5384: a failed count read is a 500, not `{"active": 0}` — the badge
    /// this feeds cannot tell those apart.
    pub async fn kg_count(&self, id: &str) -> ServiceResult<usize> {
        let handle = self.open_handle(id)?;
        handle
            .kg
            .count_active_triples()
            .map_err(|e| ServiceError::internal(format!("kg count_active_triples: {e:#}")))
    }

    /// Build the per-palace visual graph payload.
    ///
    /// Why (issue #4670): the `node_count` / `edge_count` / `community_count`
    /// here are computed over the FULL adjacency while `triples` is capped at
    /// [`KG_GRAPH_MAX_TRIPLES`]. That mismatch used to be invisible — the UI
    /// rendered 5,000 triples under a "9,311 nodes" badge — and because
    /// `list_active` orders by `valid_from` DESC the dropped triples were
    /// silently the oldest. The payload now reports what it actually returned
    /// alongside what exists, so truncation is machine-detectable.
    /// What: unchanged query; adds `returned_triple_count`,
    /// `active_triple_count`, and the derived `truncated` flag.
    /// Test: `kg_graph_signals_truncation`, `kg_graph_returns_active_triples`.
    pub async fn kg_graph(&self, id: &str) -> ServiceResult<KgGraphPayload> {
        self.kg_graph_with_cap(id, KG_GRAPH_MAX_TRIPLES).await
    }

    /// [`Self::kg_graph`] with an explicit triple cap.
    ///
    /// Why (issue #4670): the truncation-signalling branch is only reachable
    /// above `KG_GRAPH_MAX_TRIPLES` (5,000), and seeding 5,001 triples costs
    /// ~90 s of test time — expensive enough that the branch would in practice
    /// go untested. Taking the cap as a parameter makes it provable with five
    /// triples and a cap of three, at no cost to the production call path.
    /// What: the real implementation; `kg_graph` is a thin wrapper that passes
    /// the production constant.
    /// Test: `kg_graph_signals_truncation`.
    pub async fn kg_graph_with_cap(
        &self,
        id: &str,
        max_triples: usize,
    ) -> ServiceResult<KgGraphPayload> {
        let handle = self.open_handle(id)?;
        let triples = handle
            .kg
            .list_active(max_triples, 0)
            .await
            .map_err(|e| ServiceError::internal(format!("kg list_active: {e:#}")))?;
        // #4670: compare against the true active count, not the cap, so a
        // palace sitting exactly on the cap is not falsely flagged.
        // #5384: a failed read would come back as 0 and make `truncated` false
        // for every payload, which is the flag's exact failure mode.
        let active_triple_count = handle
            .kg
            .count_active_triples()
            .map_err(|e| ServiceError::internal(format!("kg count_active_triples: {e:#}")))?
            as u64;
        let returned_triple_count = triples.len() as u64;
        Ok(KgGraphPayload {
            triples,
            node_count: handle.kg.node_count() as u64,
            edge_count: handle.kg.edge_count() as u64,
            community_count: handle.kg.community_count() as u64,
            returned_triple_count,
            active_triple_count,
            truncated: returned_triple_count < active_triple_count,
        })
    }

    /// Top-`limit` nodes by degree plus the edges among them (issue #4670).
    ///
    /// Why: first paint must show the graph's skeleton, not 9,311 nodes in an
    /// O(n²) layout. Measured on the live 8,266-triple palace, 90.2% of nodes
    /// are degree-1 leaves and only 7.2% have degree >= 5, so a
    /// top-degree slice carries essentially all of the visible structure and
    /// everything else stays one click away.
    /// What: runs [`KnowledgeGraph::top_degree_subgraph`] over the resident
    /// adjacency (O(V log V + E), no disk I/O) and pairs the result with the
    /// palace-wide totals the header needs to report honestly.
    /// Test: `kg_graph_seed_ranks_by_degree`, `kg_graph_seed_clamps_limit`.
    pub async fn kg_graph_seed(&self, id: &str, limit: usize) -> ServiceResult<KgSeedPayload> {
        let handle = self.open_handle(id)?;
        let (nodes, triples) = handle
            .kg
            .top_degree_subgraph(limit)
            .map_err(|e| ServiceError::internal(format!("kg top_degree_subgraph: {e:#}")))?;
        let node_count = handle.kg.node_count() as u64;
        let returned_node_count = nodes.len() as u64;
        Ok(KgSeedPayload {
            nodes: nodes.into_iter().map(KgNodeView::from).collect(),
            returned_triple_count: triples.len() as u64,
            triples,
            node_count,
            edge_count: handle.kg.edge_count() as u64,
            community_count: handle.kg.community_count() as u64,
            returned_node_count,
            limit: limit as u64,
            truncated: returned_node_count < node_count,
        })
    }

    /// Direction-aware, hop-bounded expansion around one node (issue #4670).
    ///
    /// Why: click-to-expand needs "what points AT this node", which no HTTP
    /// endpoint could answer — `kg_query` is a subject prefix scan. Bounding
    /// the hops keeps one click on a hub from pulling the whole palace.
    /// What: delegates to [`KnowledgeGraph::expand_neighbors`]. `direction`
    /// and `max_hops` are already validated/clamped by the HTTP layer; they
    /// are echoed back so the client can see what actually ran.
    /// Test: `kg_neighbors_returns_incoming_edges`, `kg_neighbors_clamps_max_hops`.
    pub async fn kg_neighbors(
        &self,
        id: &str,
        node: &str,
        direction: ExpandDirection,
        max_hops: usize,
    ) -> ServiceResult<KgNeighborsPayload> {
        let handle = self.open_handle(id)?;
        let (nodes, triples) = handle
            .kg
            .expand_neighbors(node, direction, max_hops)
            .map_err(|e| ServiceError::internal(format!("kg expand_neighbors: {e:#}")))?;
        Ok(KgNeighborsPayload {
            origin: node.to_string(),
            returned_node_count: nodes.len() as u64,
            returned_triple_count: triples.len() as u64,
            nodes: nodes.into_iter().map(KgNodeView::from).collect(),
            triples,
            direction: match direction {
                ExpandDirection::In => "in",
                ExpandDirection::Out => "out",
                ExpandDirection::Both => "both",
            }
            .to_string(),
            max_hops: max_hops as u64,
        })
    }

    // -----------------------------------------------------------------
    // Dream cycle
    // -----------------------------------------------------------------

    /// Aggregate dream stats across every persisted palace.
    pub async fn dream_status_aggregate(&self) -> DreamStatusPayload {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root).unwrap_or_default();
        let mut out = DreamStatusPayload::default();
        let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
        for p in palaces {
            let data_dir = self.state.data_root.join(p.id.as_str());
            let snap = match PersistedDreamStats::load(&data_dir) {
                Ok(Some(s)) => s,
                _ => continue,
            };
            out.merged = out.merged.saturating_add(snap.stats.merged);
            out.pruned = out.pruned.saturating_add(snap.stats.pruned);
            out.compacted = out.compacted.saturating_add(snap.stats.compacted);
            out.closets_updated = out
                .closets_updated
                .saturating_add(snap.stats.closets_updated);
            out.duration_ms = out.duration_ms.saturating_add(snap.stats.duration_ms);
            latest = match latest {
                Some(t) if t >= snap.last_run_at => Some(t),
                _ => Some(snap.last_run_at),
            };
        }
        out.last_run_at = latest;
        out
    }

    /// Per-palace dream stats snapshot.
    pub async fn dream_status_for_palace(&self, id: &str) -> ServiceResult<DreamStatusPayload> {
        let data_dir = self.state.data_root.join(id);
        if !data_dir.exists() {
            return Err(ServiceError::not_found(format!("palace not found: {id}")));
        }
        match PersistedDreamStats::load(&data_dir) {
            Ok(Some(s)) => Ok(s.into()),
            Ok(None) => Ok(DreamStatusPayload::default()),
            Err(e) => Err(ServiceError::internal(format!("read dream stats: {e:#}"))),
        }
    }

    /// Run a dream cycle across every palace.
    ///
    /// Why (issue #4637): like `recall_all`, this route is deliberately NOT
    /// converted to `PalaceRegistry::peek`. Dreaming is a maintenance pass —
    /// consolidating only the 64 palaces that happen to be cache-resident
    /// would silently stop maintaining the other ~5,730, which is a worse
    /// failure than a slow run because nothing reports it. Opening every
    /// palace stays correct; what changes is that the blocking open no longer
    /// runs inline on a tokio worker thread. A long dream run is expected —
    /// this is an explicitly-triggered `POST`, not a page load.
    /// What: lists palaces on the blocking pool, then per palace hops to
    /// `spawn_blocking` for the open before awaiting the async dream cycle.
    /// Test: `dream_run_aggregates_stats`.
    pub async fn dream_run(&self) -> ServiceResult<DreamStatusPayload> {
        let palaces = list_palaces_blocking(&self.state)
            .await
            .map_err(|e| ServiceError::internal(format!("{e:#}")))?;
        let dreamer = Dreamer::new(DreamConfig::default());
        let mut out = DreamStatusPayload::default();
        for p in palaces {
            // #4637: open_palace (not peek) is deliberate — a dream cycle must
            // maintain every palace; the spawn_blocking hop keeps the cold open
            // off the async executor.
            let registry = std::sync::Arc::clone(&self.state.registry);
            let root = self.state.data_root.clone();
            let pid = p.id.clone();
            let opened =
                tokio::task::spawn_blocking(move || registry.open_palace(&root, &pid)).await;
            let handle = match opened {
                Ok(Ok(h)) => h,
                Ok(Err(e)) => {
                    tracing::warn!(palace = %p.id, "dream_run: open failed: {e:#}");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(palace = %p.id, "dream_run: join open failed: {e}");
                    continue;
                }
            };
            match dreamer.dream_cycle(&handle).await {
                Ok(stats) => {
                    out.merged = out.merged.saturating_add(stats.merged);
                    out.pruned = out.pruned.saturating_add(stats.pruned);
                    out.compacted = out.compacted.saturating_add(stats.compacted);
                    out.closets_updated = out.closets_updated.saturating_add(stats.closets_updated);
                    out.duration_ms = out.duration_ms.saturating_add(stats.duration_ms);
                }
                Err(e) => tracing::warn!(palace = %p.id, "dream_run: cycle failed: {e:#}"),
            }
            refresh_gaps_cache(&self.state, &handle).await;
        }
        out.last_run_at = Some(chrono::Utc::now());
        self.state.emit(DaemonEvent::DreamCompleted {
            palace_id: None,
            merged: out.merged,
            pruned: out.pruned,
            compacted: out.compacted,
            closets_updated: out.closets_updated,
            duration_ms: out.duration_ms,
            source: ActivitySource::Http,
        });
        self.state.emit(self.aggregate_status_event());
        Ok(out)
    }

    // -----------------------------------------------------------------
    // Activity log
    // -----------------------------------------------------------------

    /// Paginated activity-log read.
    pub async fn list_activity(
        &self,
        filter: ActivityFilter,
        limit: usize,
        offset: usize,
    ) -> ServiceResult<(Vec<crate::ActivityEntry>, u64)> {
        let entries = self
            .state
            .activity_log
            .list(&filter, limit, offset)
            .map_err(|e| ServiceError::internal(format!("activity list: {e:#}")))?;
        let total = self
            .state
            .activity_log
            .count()
            .map_err(|e| ServiceError::internal(format!("activity count: {e:#}")))?;
        Ok((entries, total))
    }

    // -----------------------------------------------------------------
    // Internal helper — open a palace handle or return 404.
    // -----------------------------------------------------------------

    /// Open the named palace, returning `ServiceError::NotFound` on failure.
    pub fn open_handle(&self, id: &str) -> ServiceResult<Arc<PalaceHandle>> {
        self.state
            .registry
            .open_palace(&self.state.data_root, &PalaceId::new(id))
            .map_err(|e| ServiceError::not_found(format!("palace not found: {id} ({e:#})")))
    }
}
