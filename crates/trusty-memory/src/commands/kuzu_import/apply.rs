//! Plan and apply one store's import against a palace (#277).
//!
//! Why: the planner must run identically in `--dry-run` and in a real run, and
//! the writer must be swappable so a partial write failure is testable.
//! What: [`plan_store`] maps an export against a [`Ledger`] with no palace
//! access. [`execute`] then walks the plan: memories first (insert new, finish
//! pending, update changed under `--update`), then Entity triples, then
//! MENTIONS / RELATES_TO edges resolved to `drawer:<uuid>` subjects. A triple
//! is asserted only when the exact `(subject, predicate, object)` is not
//! already active, because re-asserting an active triple closes its interval
//! and writes a history row even though nothing changed. With no
//! [`PalaceSink`] it only counts.
//! Test: `import_twice_is_idempotent_on_palace_state`,
//! `changed_hash_is_flagged_then_updated_in_place`,
//! `partial_write_failure_is_reported_and_resumable`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use trusty_common::memory_core::filter::check_secret;
use trusty_common::memory_core::palace::{Drawer, DrawerType, RoomType};
use trusty_common::memory_core::retrieval::{shared_embedder, PalaceHandle, RememberOptions};
use trusty_common::memory_core::store::{Triple, VectorStore as _};
use uuid::Uuid;

use super::bridge::KuzuExport;
use super::ledger::{Ledger, MemoryPlan};
use super::mapping::{
    drawer_subject, entity_subject, entity_triples, map_memory, merge_tags, relates_to_predicate,
    triple, MappedMemory,
};
use super::KuzuImportError;

/// Read access to a palace: enough to plan and to count a dry run.
#[async_trait]
pub trait PalaceView: Send + Sync {
    /// Every drawer in the palace (imported or not).
    fn drawers(&self) -> Vec<Drawer>;
    /// Whether `(subject, predicate, object)` is an active triple.
    async fn triple_is_active(&self, t: &Triple) -> Result<bool, KuzuImportError>;
}

/// Write access to a palace.
#[async_trait]
pub trait PalaceSink: PalaceView {
    /// Insert a new drawer carrying [`MappedMemory::staging_tags`] only.
    async fn insert_drawer(&self, m: &MappedMemory) -> Result<Uuid, KuzuImportError>;
    /// Write identity, hash and `created_at` onto drawer `id` in one write.
    async fn stamp_drawer(&self, id: Uuid, m: &MappedMemory) -> Result<(), KuzuImportError>;
    /// Re-embed and rewrite drawer `id` from `m`, identity included.
    async fn update_memory(&self, id: Uuid, m: &MappedMemory) -> Result<(), KuzuImportError>;
    async fn assert_triple(&self, t: Triple) -> Result<(), KuzuImportError>;
}

/// Counts for one store; the only thing the CLI prints about a store's data.
///
/// `refused_ids` and `shared_ids` carry `Memory.id` values only, never content.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StoreCounts {
    pub memories: usize,
    pub edges: usize,
    pub entities: usize,
    pub skipped_empty: usize,
    pub new_memories: usize,
    pub unchanged: usize,
    pub changed: usize,
    pub updated: usize,
    pub new_triples: usize,
    pub existing_triples: usize,
    pub dangling_edges: usize,
    /// Edges not written because an endpoint was refused or shared.
    pub skipped_edges: usize,
    pub failed_writes: usize,
    /// Memories the secret screen refused (#277 M6).
    pub refused_ids: Vec<String>,
    /// Memories whose id another live store holds with other content (#277 H4).
    pub shared_ids: Vec<String>,
}

/// One store's work, decided before anything is written.
#[derive(Debug, Default)]
pub struct StorePlan {
    pub memories: Vec<(MappedMemory, MemoryPlan)>,
    pub entity_triples: Vec<Triple>,
    /// `(memory id, entity id, confidence)`.
    pub mentions: Vec<(String, String, Option<f64>)>,
    /// `(from memory id, to memory id, predicate, confidence)`.
    pub relates: Vec<(String, String, String, Option<f64>)>,
    pub counts: StoreCounts,
}

/// Map `export` against `ledger`. Pure apart from `store_is_live`.
///
/// Test: `ledger_plans_new_unchanged_changed`, `import_twice_is_idempotent_on_palace_state`.
pub fn plan_store(
    export: &KuzuExport,
    store: &str,
    ledger: &Ledger,
    store_is_live: &dyn Fn(&str) -> bool,
) -> StorePlan {
    let mut plan = StorePlan::default();
    plan.counts.memories = export.memories.len();
    plan.counts.edges = export.edge_count();
    plan.counts.entities = export.entities.len();
    for row in &export.memories {
        match map_memory(row, store) {
            Some(m) => {
                let p = ledger.plan(&m, store_is_live);
                plan.memories.push((m, p));
            }
            None => plan.counts.skipped_empty += 1,
        }
    }
    plan.entity_triples = export.entities.iter().flat_map(entity_triples).collect();
    for m in &export.mentions {
        if let (Some(mem), Some(ent)) = (&m.memory_id, &m.entity_id) {
            plan.mentions.push((mem.clone(), ent.clone(), m.confidence));
        } else {
            plan.counts.dangling_edges += 1;
        }
    }
    for r in &export.relates_to {
        if let (Some(a), Some(b)) = (&r.from_id, &r.to_id) {
            let pred = relates_to_predicate(r.relationship_type.as_deref());
            plan.relates.push((a.clone(), b.clone(), pred, r.strength));
        } else {
            plan.counts.dangling_edges += 1;
        }
    }
    plan
}

/// Where an edge endpoint's drawer stands.
#[derive(Debug, Clone, Copy)]
enum Endpoint {
    Drawer(Uuid),
    /// Would be written by this run (dry run only).
    Pending,
    /// Its memory failed to write this run; a re-run picks it up.
    Failed,
    /// Refused or shared: no re-run will write it, so neither are its edges.
    Skipped,
    /// Not a memory of this store (e.g. a row skipped for empty content).
    Dangling,
}

/// Whether the secret screen refuses `m`'s content (#277 M6).
fn is_secret_shaped(m: &MappedMemory) -> bool {
    check_secret(m.content.trim()).is_err()
}

fn refuse(c: &mut StoreCounts, m: &MappedMemory) {
    // The id only: the content is the secret-shaped part.
    tracing::warn!(memory_id = %m.memory_id, "kuzu import: refused a secret-shaped memory");
    c.refused_ids.push(m.memory_id.clone());
}

/// Insert then stamp a new drawer; `Err` carries the id when the insert landed.
async fn insert_new(
    s: &dyn PalaceSink,
    m: &MappedMemory,
) -> Result<Uuid, (Option<Uuid>, KuzuImportError)> {
    let id = s.insert_drawer(m).await.map_err(|e| (None, e))?;
    s.stamp_drawer(id, m).await.map_err(|e| (Some(id), e))?;
    Ok(id)
}

/// Walk `plan` against `view`, writing through `sink` when given.
///
/// What: see the module doc. `sink = None` is the dry run — every count is
/// what a real run would do, and nothing is written. A write that fails is
/// counted in `failed_writes` and the walk continues; edges that depend on a
/// failed memory are counted as failed too, so a re-run picks them up. A
/// secret-shaped memory is refused (counted, its id reported) and a shared id
/// is skipped; neither fails the store, and their edges are skipped.
/// Test: `import_twice_is_idempotent_on_palace_state`,
/// `changed_hash_is_flagged_then_updated_in_place`,
/// `partial_write_failure_is_reported_and_resumable`,
/// `secret_shaped_memory_is_refused_without_failing_the_store`.
pub async fn execute(
    mut plan: StorePlan,
    view: &dyn PalaceView,
    sink: Option<&dyn PalaceSink>,
    update: bool,
) -> StoreCounts {
    let mut ids: HashMap<String, Endpoint> = HashMap::new();
    let memories = std::mem::take(&mut plan.memories);
    let c = &mut plan.counts;
    for (m, p) in &memories {
        let end = match *p {
            MemoryPlan::Unchanged(id) => {
                c.unchanged += 1;
                Endpoint::Drawer(id)
            }
            MemoryPlan::SharedId(_) => {
                c.shared_ids.push(m.memory_id.clone());
                Endpoint::Skipped
            }
            MemoryPlan::Changed(id) => {
                c.changed += 1;
                if update && is_secret_shaped(m) {
                    refuse(c, m);
                } else if update {
                    match sink {
                        Some(s) => match s.update_memory(id, m).await {
                            Ok(()) => c.updated += 1,
                            Err(e) => fail(c, "update memory", &e),
                        },
                        None => c.updated += 1,
                    }
                }
                Endpoint::Drawer(id)
            }
            _ if is_secret_shaped(m) => {
                refuse(c, m);
                Endpoint::Skipped
            }
            // #277 M2: an earlier run inserted this drawer but its stamp failed.
            MemoryPlan::Resume(id) => match sink {
                Some(s) => match s.update_memory(id, m).await {
                    Ok(()) => {
                        c.new_memories += 1;
                        Endpoint::Drawer(id)
                    }
                    Err(e) => {
                        fail(c, "finish pending memory", &e);
                        Endpoint::Failed
                    }
                },
                None => {
                    c.new_memories += 1;
                    Endpoint::Drawer(id)
                }
            },
            MemoryPlan::New => match sink {
                Some(s) => match insert_new(s, m).await {
                    Ok(id) => {
                        c.new_memories += 1;
                        Endpoint::Drawer(id)
                    }
                    Err((_, e)) => {
                        fail(c, "insert memory", &e);
                        Endpoint::Failed
                    }
                },
                None => {
                    c.new_memories += 1;
                    Endpoint::Pending
                }
            },
        };
        ids.insert(m.memory_id.clone(), end);
    }
    let resolve = |memory_id: &str| ids.get(memory_id).copied().unwrap_or(Endpoint::Dangling);
    let mut edges: Vec<Result<Triple, Endpoint>> = Vec::new();
    for t in std::mem::take(&mut plan.entity_triples) {
        edges.push(Ok(t));
    }
    for (mem, ent, conf) in &plan.mentions {
        edges.push(match resolve(mem) {
            Endpoint::Drawer(id) => Ok(triple(
                drawer_subject(id),
                "mentions",
                entity_subject(ent),
                *conf,
            )),
            other => Err(other),
        });
    }
    for (a, b, pred, conf) in &plan.relates {
        edges.push(match (resolve(a), resolve(b)) {
            (Endpoint::Drawer(x), Endpoint::Drawer(y)) => {
                Ok(triple(drawer_subject(x), pred, drawer_subject(y), *conf))
            }
            (Endpoint::Dangling, _) | (_, Endpoint::Dangling) => Err(Endpoint::Dangling),
            (Endpoint::Skipped, _) | (_, Endpoint::Skipped) => Err(Endpoint::Skipped),
            (Endpoint::Failed, _) | (_, Endpoint::Failed) => Err(Endpoint::Failed),
            _ => Err(Endpoint::Pending),
        });
    }
    let c = &mut plan.counts;
    for edge in edges {
        match edge {
            Err(Endpoint::Pending) => c.new_triples += 1,
            Err(Endpoint::Dangling) => c.dangling_edges += 1,
            Err(Endpoint::Skipped) => c.skipped_edges += 1,
            Err(_) => c.failed_writes += 1,
            Ok(t) => match view.triple_is_active(&t).await {
                Ok(true) => c.existing_triples += 1,
                Ok(false) => match sink {
                    None => c.new_triples += 1,
                    Some(s) => match s.assert_triple(t).await {
                        Ok(()) => c.new_triples += 1,
                        Err(e) => fail(c, "assert triple", &e),
                    },
                },
                Err(e) => fail(c, "read triple", &e),
            },
        }
    }
    plan.counts
}

fn fail(c: &mut StoreCounts, what: &str, e: &KuzuImportError) {
    // Counts only: an error can quote the drawer, and memory content from a
    // private store must not reach a log.
    tracing::warn!(error_kind = %e.kind(), "kuzu import: {what} failed");
    c.failed_writes += 1;
}

/// [`PalaceSink`] over a live [`PalaceHandle`] opened by this process.
///
/// Why: inserts go through `remember_with_options`, the palace's own write
/// pipeline, so the drawer is embedded, secret-screened and persisted exactly
/// like any other memory. The type is pinned to `Unknown` so the heuristic
/// classifier cannot give an imported memory a `SessionEvent` TTL.
pub struct HandleSink {
    pub handle: Arc<PalaceHandle>,
}

fn sink_err(e: anyhow::Error) -> KuzuImportError {
    KuzuImportError::Palace(format!("{e:#}"))
}

#[async_trait]
impl PalaceView for HandleSink {
    fn drawers(&self) -> Vec<Drawer> {
        self.handle.drawers.read().clone()
    }

    async fn triple_is_active(&self, t: &Triple) -> Result<bool, KuzuImportError> {
        let active = self
            .handle
            .kg
            .query_active(&t.subject)
            .await
            .map_err(sink_err)?;
        Ok(active
            .iter()
            .any(|a| a.predicate == t.predicate && a.object == t.object))
    }
}

#[async_trait]
impl PalaceSink for HandleSink {
    async fn insert_drawer(&self, m: &MappedMemory) -> Result<Uuid, KuzuImportError> {
        let opts = RememberOptions {
            force: true,
            enforce_min_tokens: false,
            classify_as: Some(DrawerType::Unknown),
            ..RememberOptions::default()
        };
        self.handle
            .remember_with_options(
                m.content.clone(),
                RoomType::General,
                m.staging_tags(),
                m.importance,
                opts,
            )
            .await
            .map_err(sink_err)
    }

    async fn stamp_drawer(&self, id: Uuid, m: &MappedMemory) -> Result<(), KuzuImportError> {
        // #277 M2: identity, hash and created_at land in one drawer write.
        let (tags, created) = (m.tags.clone(), m.created_at);
        self.rewrite(id, move |d| {
            d.tags = merge_tags(&d.tags, &tags);
            if let Some(c) = created {
                d.created_at = c;
            }
        })
        .await
    }

    async fn update_memory(&self, id: Uuid, m: &MappedMemory) -> Result<(), KuzuImportError> {
        let embedder = shared_embedder().await.map_err(sink_err)?;
        let vectors = embedder
            .embed_batch(std::slice::from_ref(&m.content))
            .await
            .map_err(sink_err)?;
        if let Some(v) = vectors.into_iter().next() {
            self.handle
                .vector_store
                .upsert(id, v)
                .await
                .map_err(sink_err)?;
        }
        let (content, tags, importance, created) = (
            m.content.clone(),
            m.tags.clone(),
            m.importance,
            m.created_at,
        );
        self.rewrite(id, move |d| {
            d.set_content(content);
            // #277 L5: keep tags the importer did not generate.
            d.tags = merge_tags(&d.tags, &tags);
            d.importance = importance;
            if let Some(c) = created {
                d.created_at = c;
            }
        })
        .await
    }

    async fn assert_triple(&self, t: Triple) -> Result<(), KuzuImportError> {
        self.handle.kg.assert(t).await.map_err(sink_err)
    }
}

impl HandleSink {
    /// Edit drawer `id` in place: persist it, then replace the in-memory row.
    async fn rewrite(
        &self,
        id: Uuid,
        edit: impl FnOnce(&mut Drawer) + Send,
    ) -> Result<(), KuzuImportError> {
        let current = self
            .handle
            .drawers
            .read()
            .iter()
            .find(|d| d.id == id)
            .cloned();
        let mut drawer = match current {
            Some(d) => d,
            None => self
                .handle
                .kg
                .load_drawer(id)
                .map_err(sink_err)?
                .ok_or_else(|| KuzuImportError::Palace(format!("drawer {id} not found")))?,
        };
        edit(&mut drawer);
        self.handle
            .kg
            .upsert_drawer(&drawer)
            .await
            .map_err(sink_err)?;
        let mut table = self.handle.drawers.write();
        if let Some(slot) = table.iter_mut().find(|d| d.id == id) {
            *slot = drawer;
        }
        Ok(())
    }
}

/// [`PalaceView`] over a throw-away copy of a palace's KG.
///
/// Why (#277 M1): `PalaceHandle::open` sweeps expired drawers as it opens, and
/// even a `ReadOnlyClient` KG open of the live `kg.redb` takes the write lock
/// and runs a table-init write transaction when no daemon holds it. A dry run
/// must not write, so it reads a copy in a temp directory instead.
/// What: `drawers` and `kg` come from the copy; `snapshot_dir` owns the temp
/// directory and is declared last so the KG closes before it is removed.
pub struct SnapshotView {
    pub drawers: Vec<Drawer>,
    pub kg: Option<trusty_common::memory_core::store::KnowledgeGraph>,
    pub snapshot_dir: Option<tempfile::TempDir>,
}

#[async_trait]
impl PalaceView for SnapshotView {
    fn drawers(&self) -> Vec<Drawer> {
        self.drawers.clone()
    }

    async fn triple_is_active(&self, t: &Triple) -> Result<bool, KuzuImportError> {
        let Some(kg) = &self.kg else {
            return Ok(false);
        };
        let active = kg.query_active(&t.subject).await.map_err(sink_err)?;
        Ok(active
            .iter()
            .any(|a| a.predicate == t.predicate && a.object == t.object))
    }
}
