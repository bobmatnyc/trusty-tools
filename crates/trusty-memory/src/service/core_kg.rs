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
use trusty_common::memory_core::store::kg::Triple;
use trusty_common::memory_core::{PalaceHandle, PalaceRegistry};

use super::core::KG_GRAPH_MAX_TRIPLES;
use super::helpers::refresh_gaps_cache;
use super::types::{
    DreamStatusPayload, KgAssertBody, KgGraphPayload, ServiceError, ServiceResult,
};
use super::MemoryService;

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
    pub async fn kg_assert(&self, id: &str, body: KgAssertBody) -> ServiceResult<()> {
        let handle = self.open_handle(id)?;
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
    pub async fn kg_count(&self, id: &str) -> ServiceResult<usize> {
        let handle = self.open_handle(id)?;
        Ok(handle.kg.count_active_triples())
    }

    /// Build the per-palace visual graph payload.
    pub async fn kg_graph(&self, id: &str) -> ServiceResult<KgGraphPayload> {
        let handle = self.open_handle(id)?;
        let triples = handle
            .kg
            .list_active(KG_GRAPH_MAX_TRIPLES, 0)
            .await
            .map_err(|e| ServiceError::internal(format!("kg list_active: {e:#}")))?;
        Ok(KgGraphPayload {
            triples,
            node_count: handle.kg.node_count() as u64,
            edge_count: handle.kg.edge_count() as u64,
            community_count: handle.kg.community_count() as u64,
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
    pub async fn dream_run(&self) -> ServiceResult<DreamStatusPayload> {
        let palaces = PalaceRegistry::list_palaces(&self.state.data_root)
            .map_err(|e| ServiceError::internal(format!("list palaces: {e:#}")))?;
        let dreamer = Dreamer::new(DreamConfig::default());
        let mut out = DreamStatusPayload::default();
        for p in palaces {
            let handle = match self
                .state
                .registry
                .open_palace(&self.state.data_root, &p.id)
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(palace = %p.id, "dream_run: open failed: {e:#}");
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
