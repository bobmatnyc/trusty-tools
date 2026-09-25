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
//! and writes a history row even though nothing changed. Under `--update`,
//! [`retract_stale`] then closes kuzu edges the source no longer carries.
//! With no [`PalaceSink`] it only counts.
//! Test: `import_twice_is_idempotent_on_palace_state`,
//! `changed_hash_is_flagged_then_updated_in_place`,
//! `partial_write_failure_is_reported_and_resumable`,
//! `stamp_or_triple_failure_is_partial_and_the_rerun_completes_it`,
//! `update_retracts_kuzu_edges_absent_from_the_source`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use trusty_common::memory_core::palace::{Drawer, DrawerType, RoomType};
use trusty_common::memory_core::retrieval::{shared_embedder, PalaceHandle, RememberOptions};
use trusty_common::memory_core::store::{Triple, VectorStore as _};
use uuid::Uuid;

use super::bridge::KuzuExport;
use super::ledger::{Ledger, MemoryPlan};
use super::mapping::{
    drawer_subject, entity_subject, entity_triples, map_memory, merge_tags, relates_to_predicate,
    source_key, triple, MappedMemory,
};
use super::retract::{retract_stale, Retraction};
use super::screen::{screen, tally, RuleTally, SecretRule};
use super::KuzuImportError;

/// What the report lists for a memory refused because its id is itself
/// secret-shaped, in place of the id.
pub const SECRET_SHAPED_ID: &str = "(secret-shaped id)";

/// Read access to a palace: enough to plan and to count a dry run.
#[async_trait]
pub trait PalaceView: Send + Sync {
    /// Every drawer in the palace (imported or not).
    fn drawers(&self) -> Vec<Drawer>;
    /// Every active triple whose subject is `subject`.
    async fn active_triples(&self, subject: &str) -> Result<Vec<Triple>, KuzuImportError>;
    /// Whether `(subject, predicate, object)` is an active triple.
    async fn triple_is_active(&self, t: &Triple) -> Result<bool, KuzuImportError> {
        let active = self.active_triples(&t.subject).await?;
        Ok(active
            .iter()
            .any(|a| a.predicate == t.predicate && a.object == t.object))
    }
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
    /// Close the active triple `t` (#277 MEDIUM-2).
    async fn retract_triple(&self, t: &Triple) -> Result<(), KuzuImportError>;
}

/// Counts for one store; the only thing the CLI prints about a store's data.
///
/// `refused_ids`, `shared_ids` and `retracted` carry `Memory.id` values only,
/// never content; `refusal_rules` carries rule labels, never the token.
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
    /// Store-supplied tag values dropped by the secret screen (#277 MEDIUM-1).
    pub refused_tags: usize,
    /// Triples dropped because a string in them failed the secret screen.
    pub refused_triples: usize,
    /// Every refusal above (memories, tags, triples) by rule (#277 MEDIUM-3).
    pub refusal_rules: RuleTally,
    /// Kuzu edges closed because the source no longer has them (#277 MEDIUM-2).
    pub retracted: Vec<Retraction>,
    /// Edge rows per relationship table the import does not map (#277 LOW-2).
    pub unsupported_edges: BTreeMap<String, usize>,
    /// Why the export could not count those tables, when it could not.
    pub unsupported_error: Option<String>,
}

impl StoreCounts {
    /// Record one refusal of kind `bucket` under `rule`.
    fn refuse_under(&mut self, rule: SecretRule, bucket: fn(&mut Self) -> &mut usize) {
        *bucket(self) += 1;
        tally(&mut self.refusal_rules, rule);
    }
}

/// One store's work, decided before anything is written.
#[derive(Debug, Default)]
pub struct StorePlan {
    pub memories: Vec<(MappedMemory, MemoryPlan)>,
    /// Memory ids refused before mapping; their edges are skipped.
    pub refused_memory_ids: Vec<String>,
    /// Memory ids whose drawer `--update` may retract stale edges from.
    pub retract_scope: HashSet<String>,
    pub entity_triples: Vec<Triple>,
    /// `(memory id, entity id, confidence)`.
    pub mentions: Vec<(String, String, Option<f64>)>,
    /// `(from memory id, to memory id, predicate, confidence)`.
    pub relates: Vec<(String, String, String, Option<f64>)>,
    pub counts: StoreCounts,
}

/// Map `export` against `ledger`. Pure apart from `store_is_live`.
///
/// What: also runs the secret screen over every store-supplied string that
/// would become a tag or a triple (#277 MEDIUM-1): a refused `Memory.id`
/// refuses its memory, and a refused entity id, relationship type or edge
/// endpoint drops the triple; each is counted with its rule. A drawer is in
/// `retract_scope` unless its recorded store is another store that still
/// exists, whose edges this store must not retract.
/// Test: `ledger_plans_new_unchanged_changed`, `import_twice_is_idempotent_on_palace_state`,
/// `store_supplied_tags_and_triples_pass_the_secret_screen`.
pub fn plan_store(
    export: &KuzuExport,
    store: &str,
    ledger: &Ledger,
    store_is_live: &dyn Fn(&str) -> bool,
) -> StorePlan {
    let mut plan = StorePlan::default();
    let c = &mut plan.counts;
    c.memories = export.memories.len();
    c.edges = export.edge_count();
    c.entities = export.entities.len();
    (c.unsupported_edges, c.unsupported_error) = unsupported_edges(export);
    for row in &export.memories {
        if let Some(rule) = row.id.as_deref().and_then(screen) {
            // The id is the secret-shaped part, so the report lists neither.
            tally(&mut c.refusal_rules, rule);
            c.refused_ids.push(SECRET_SHAPED_ID.to_string());
            plan.refused_memory_ids.extend(row.id.clone());
            continue;
        }
        let Some(m) = map_memory(row, store) else {
            c.skipped_empty += 1;
            continue;
        };
        for rule in &m.refused_tags {
            c.refuse_under(*rule, |c| &mut c.refused_tags);
        }
        let own = ledger
            .get(&source_key(&m.memory_id))
            .and_then(|d| d.store.as_deref());
        if own.is_none_or(|s| s == store || !store_is_live(s)) {
            plan.retract_scope.insert(m.memory_id.clone());
        }
        let p = ledger.plan(&m, store_is_live);
        plan.memories.push((m, p));
    }
    for e in &export.entities {
        let (triples, refused) = entity_triples(e);
        plan.entity_triples.extend(triples);
        for rule in refused {
            c.refuse_under(rule, |c| &mut c.refused_triples);
        }
    }
    for m in &export.mentions {
        let (Some(mem), Some(ent)) = (&m.memory_id, &m.entity_id) else {
            c.dangling_edges += 1;
            continue;
        };
        match screen(ent) {
            Some(rule) => c.refuse_under(rule, |c| &mut c.refused_triples),
            None => plan.mentions.push((mem.clone(), ent.clone(), m.confidence)),
        }
    }
    for r in &export.relates_to {
        let (Some(a), Some(b)) = (&r.from_id, &r.to_id) else {
            c.dangling_edges += 1;
            continue;
        };
        match r.relationship_type.as_deref().and_then(screen) {
            Some(rule) => c.refuse_under(rule, |c| &mut c.refused_triples),
            None => {
                let pred = relates_to_predicate(r.relationship_type.as_deref());
                plan.relates.push((a.clone(), b.clone(), pred, r.strength));
            }
        }
    }
    plan
}

/// The export's unmapped relationship tables with a row, and its count error.
fn unsupported_edges(export: &KuzuExport) -> (BTreeMap<String, usize>, Option<String>) {
    let tables = export
        .other_edges
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(t, n)| (t.clone(), *n))
        .collect();
    (tables, export.other_edges_error.clone())
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

/// The rule by which the secret screen refuses `m`'s content (#277 M6).
fn content_rule(m: &MappedMemory) -> Option<SecretRule> {
    screen(m.content.trim())
}

fn refuse(c: &mut StoreCounts, m: &MappedMemory, rule: SecretRule) {
    // The id only: the content is the secret-shaped part.
    tracing::warn!(memory_id = %m.memory_id, rule = rule.label(), "kuzu import: refused a secret-shaped memory");
    c.refused_ids.push(m.memory_id.clone());
    tally(&mut c.refusal_rules, rule);
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
    for id in &plan.refused_memory_ids {
        ids.insert(id.clone(), Endpoint::Skipped);
    }
    let memories = std::mem::take(&mut plan.memories);
    let c = &mut plan.counts;
    for (m, p) in &memories {
        let rule = content_rule(m);
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
                if let (true, Some(r)) = (update, rule) {
                    refuse(c, m, r);
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
            _ if rule.is_some() => {
                if let Some(r) = rule {
                    refuse(c, m, r);
                }
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
    // #277 MEDIUM-2: every edge the source still names, written or not.
    let keep: HashSet<(String, String, String)> = edges
        .iter()
        .flatten()
        .map(|t| (t.subject.clone(), t.predicate.clone(), t.object.clone()))
        .collect();
    let scope: Vec<(String, Uuid)> = memories
        .iter()
        .filter(|(m, _)| plan.retract_scope.contains(&m.memory_id))
        .filter_map(|(m, _)| match ids.get(&m.memory_id) {
            Some(Endpoint::Drawer(id)) => Some((m.memory_id.clone(), *id)),
            _ => None,
        })
        .collect();
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
    if update {
        retract_stale(&scope, &keep, view, sink, c).await;
    }
    plan.counts
}

/// Count one failed read or write; logs the error kind only.
pub(super) fn fail(c: &mut StoreCounts, what: &str, e: &KuzuImportError) {
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

    async fn active_triples(&self, subject: &str) -> Result<Vec<Triple>, KuzuImportError> {
        self.handle.kg.query_active(subject).await.map_err(sink_err)
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

    async fn retract_triple(&self, t: &Triple) -> Result<(), KuzuImportError> {
        // One object only: a sibling edge on the same predicate stays (#5396).
        self.handle
            .kg
            .retract_triple(&t.subject, &t.predicate, &t.object)
            .await
            .map(drop)
            .map_err(sink_err)
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

    async fn active_triples(&self, subject: &str) -> Result<Vec<Triple>, KuzuImportError> {
        match &self.kg {
            Some(kg) => kg.query_active(subject).await.map_err(sink_err),
            None => Ok(Vec::new()),
        }
    }
}
