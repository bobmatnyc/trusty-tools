//! Per-source index journal — what the BOUND SEARCH INDEX actually holds.
//!
//! Why (#3892): an OKG store is one thing with two facets — a markdown tree and
//! a trusty-search index over it — but until now only the tree was ever
//! written. An ingest reported "N entities ingested" truthfully while
//! `vector_search` found nothing, because nothing ever pushed those entities
//! into the bound index. Feeding the index needs a durable answer to "which
//! entities, at which content, are already in the index?", and that answer must
//! NOT be folded into the item [`Ledger`]: the ledger is the record of KB-TREE
//! truth, and making a ledger line conditional on a network push would make
//! ingestion non-convergent whenever the search daemon is merely down
//! (DOC-55 SPEC-OKGIMPORT-06 §7.2.4). So index state lives in its own
//! append-only journal beside the ledger, and the two never gate each other.
//!
//! What: `_sources/<slug(id)>.index.jsonl`, one [`IndexRecord`] per push, last
//! record per entity wins. Each record pins the entity file's content hash plus
//! a cheap `(size, mtime)` stamp, which is what makes reconciliation both
//! correct and affordable: an unchanged entity is dismissed by a `stat`, a
//! changed one is caught by the hash even when its ledger fingerprint cannot
//! change (a `volatile` source item). [`KbStore::okg_index_backlog`] diffs the
//! ledger against this journal to produce the work list — everything ingested
//! but not yet searchable, plus everything tombstoned but not yet withdrawn.
//!
//! The push itself is NOT here. This crate is deterministic and network-free;
//! the HTTP call into trusty-search lives in `trusty-agents`
//! (`crate::stores::index_feed`), exactly like the Gmail/Drive fetchers. This
//! module is the part that must be unit-testable without a daemon.
//!
//! Test: `backlog_lists_freshly_ingested_entities`,
//! `recorded_entity_leaves_the_backlog`, `changed_entity_reenters_the_backlog`,
//! `tombstoned_entity_becomes_a_remove_task`,
//! `never_indexed_tombstone_needs_no_removal`,
//! `torn_index_journal_line_is_skipped_not_fatal`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::entity::slugify;
use crate::okg::jsonl::{Appender, read_journal};
use crate::okg::ledger::{ItemStatus, Ledger};
use crate::okg::registry::{SOURCES_DIR, SourceSpec};
use crate::roots::{assert_within, confine};
use crate::store::KbStore;

/// What a journal line asserts about an entity's presence in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    /// The entity's content was pushed into the bound index.
    Indexed,
    /// The entity was withdrawn from the bound index (its source item was
    /// tombstoned). The markdown file itself is preserved — a tombstone is a
    /// flag, never a deletion.
    Removed,
}

/// One journal line: the durable record of a single index push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRecord {
    /// `<collection>/<slug>` — the same identity the ledger records.
    pub entity: String,
    /// SHA-256 of the entity file's bytes AS PUSHED. This, not the source
    /// item's fingerprint, is what decides staleness: a `volatile` item's
    /// fingerprint is constant by definition, so trusting it would freeze stale
    /// content in the index forever.
    pub content_hash: String,
    /// Entity file size at push time — the cheap half of the staleness check.
    #[serde(default)]
    pub size: u64,
    /// Entity file mtime (nanos since epoch) at push time, when the filesystem
    /// reported one. A mismatch only triggers a re-hash, never a blind re-push,
    /// so a lying mtime costs work and never correctness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ns: Option<u128>,
    /// Indexed or removed.
    pub state: IndexState,
    /// When this record was written (ISO-8601).
    pub at: String,
}

/// Whether an entity needs pushing, withdrawing, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexOp {
    /// Push the file's current content into the index. `replace` is true when
    /// the index already holds an older version of this entity, which the
    /// caller must withdraw first — trusty-search chunks by content, so a
    /// shorter revision would otherwise leave the old chunks orphaned and
    /// searchable.
    Index { replace: bool },
    /// Withdraw the entity from the index.
    Remove,
}

/// One unit of index work, resolved to a concrete file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexTask {
    /// The source item that produced this entity.
    pub item_id: String,
    /// `<collection>/<slug>`.
    pub entity: String,
    /// Absolute path of the entity file — the identity the search index keys
    /// on, so `remove_file` can later withdraw exactly what was pushed.
    pub path: PathBuf,
    /// Content hash to record once the push succeeds.
    pub content_hash: String,
    /// File size to record once the push succeeds.
    pub size: u64,
    /// File mtime to record once the push succeeds.
    pub mtime_ns: Option<u128>,
    /// What to do.
    pub op: IndexOp,
}

/// The diff between the tree (per the ledger) and the index (per the journal).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IndexBacklog {
    /// Work still owed to the index, in ledger order.
    pub tasks: Vec<IndexTask>,
    /// Entities the journal says are already current in the index.
    pub synced: usize,
    /// Ledger rows that could not be resolved to a readable file (e.g. an
    /// entity deleted out from under the tree). Surfaced, never silently
    /// dropped.
    pub notes: Vec<String>,
}

/// An open, append-only index journal for one source.
pub struct IndexJournal {
    /// Last record per entity — the effective state.
    latest: BTreeMap<String, IndexRecord>,
    /// Count of unparseable lines encountered on load.
    malformed: usize,
    /// Durable append handle (heals a crash-torn final line).
    appender: Appender,
}

impl IndexJournal {
    /// The confined journal path for a source id under a KB root.
    ///
    /// Why: the id comes from a tool argument; slugging plus [`confine`] means a
    /// hostile id can never name a file outside `_sources/`.
    /// What: `<root>/_sources/<slug(id)>.index.jsonl` — a distinct suffix from
    /// the item ledger so the two can never collide.
    pub fn path_for(root: &Path, source_id: &str) -> anyhow::Result<PathBuf> {
        let file = format!("{}.index.jsonl", slugify(source_id));
        let path = confine(root, &[SOURCES_DIR, &file])?;
        assert_within(root, &path)?;
        Ok(path)
    }

    /// Read a source's index journal into a last-record-wins view.
    ///
    /// Why/What/Test: identical damage tolerance to [`Ledger::load`] — see
    /// [`crate::okg::jsonl::read_journal`].
    pub fn load(root: &Path, source_id: &str) -> anyhow::Result<Self> {
        let path = Self::path_for(root, source_id)?;
        let load = read_journal::<IndexRecord>(&path)?;
        let mut journal = Self {
            latest: BTreeMap::new(),
            malformed: load.malformed,
            appender: Appender::new(path, load.needs_newline),
        };
        for record in load.records {
            journal.latest.insert(record.entity.clone(), record);
        }
        Ok(journal)
    }

    /// The effective record for an entity, if it was ever pushed.
    pub fn get(&self, entity: &str) -> Option<&IndexRecord> {
        self.latest.get(entity)
    }

    /// Lines that failed to parse on load.
    pub fn malformed_lines(&self) -> usize {
        self.malformed
    }

    /// Durably append one record, updating the in-memory view.
    ///
    /// Why: this is called only AFTER the corresponding push succeeded, so the
    /// journal can never claim more than the index actually holds. A crash
    /// between a successful push and this append costs one redundant re-push on
    /// the next run (the push is an upsert), never a lost entity — the exact
    /// mirror of the ledger's entity-before-record ordering.
    pub fn append(&mut self, record: IndexRecord) -> anyhow::Result<()> {
        self.appender.append(&record)?;
        self.latest.insert(record.entity.clone(), record);
        Ok(())
    }
}

/// A file's staleness stamp: content hash plus the cheap `(size, mtime)` pair.
struct FileStamp {
    hash: String,
    size: u64,
    mtime_ns: Option<u128>,
}

/// Stamp an entity file, re-hashing only when the cheap check says it may have
/// moved.
///
/// Why: reconciliation runs on every ingest AND on every status read, so the
/// steady state — nothing changed — must not cost a full re-hash of the tree.
/// `size`/`mtime` can only produce a FALSE "changed" (a touched-but-identical
/// file), which the hash comparison then absorbs; they can never produce a
/// false "unchanged" that would leave stale content searchable, because any
/// write moves at least one of the two.
/// What: returns `None` when the file is unreadable, `Some(stamp)` otherwise.
/// `prior` short-circuits the hash when both cheap fields match.
fn stamp_file(path: &Path, prior: Option<&IndexRecord>) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    if let Some(prior) = prior
        && prior.size == size
        && prior.mtime_ns.is_some()
        && prior.mtime_ns == mtime_ns
    {
        return Some(FileStamp {
            hash: prior.content_hash.clone(),
            size,
            mtime_ns,
        });
    }
    let bytes = std::fs::read(path).ok()?;
    Some(FileStamp {
        hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
        size,
        mtime_ns,
    })
}

impl KbStore {
    /// What this source still owes the bound search index.
    ///
    /// Why (#3892): this is the reconcile that makes a failed or skipped push
    /// self-healing. The ledger decides what is IN THE TREE; this diff decides
    /// what is IN THE INDEX. Because the two are independent journals, a run
    /// with the search daemon down loses nothing: the entities land, the
    /// backlog grows, and the next run drains it — including items whose source
    /// fingerprint is unchanged and which the ingest engine therefore skipped
    /// entirely.
    /// What: for every live ledger item, a push task unless the journal already
    /// records this exact file content; for every tombstoned item that WAS
    /// pushed, a withdrawal task. Pure filesystem work — no network.
    /// Test: `backlog_lists_freshly_ingested_entities`,
    /// `recorded_entity_leaves_the_backlog`,
    /// `changed_entity_reenters_the_backlog`,
    /// `tombstoned_entity_becomes_a_remove_task`,
    /// `never_indexed_tombstone_needs_no_removal`.
    pub fn okg_index_backlog(&self, spec: &SourceSpec) -> anyhow::Result<IndexBacklog> {
        let ledger = Ledger::load(&self.root, &spec.id)?;
        let journal = IndexJournal::load(&self.root, &spec.id)?;
        let mut backlog = IndexBacklog::default();

        for record in ledger.records() {
            let prior = journal.get(&record.entity);
            let path = self.entity_file_path(spec, &record.entity)?;
            match record.status {
                ItemStatus::Ingested => {
                    let Some(stamp) = stamp_file(&path, prior) else {
                        backlog.notes.push(format!(
                            "{}: entity {} is not readable at {}",
                            record.item_id,
                            record.entity,
                            path.display()
                        ));
                        continue;
                    };
                    let current = prior.is_some_and(|p| {
                        p.state == IndexState::Indexed && p.content_hash == stamp.hash
                    });
                    if current {
                        backlog.synced += 1;
                        continue;
                    }
                    backlog.tasks.push(IndexTask {
                        item_id: record.item_id.clone(),
                        entity: record.entity.clone(),
                        path,
                        content_hash: stamp.hash,
                        size: stamp.size,
                        mtime_ns: stamp.mtime_ns,
                        // A prior Indexed record means the index holds an older
                        // revision that must be withdrawn before the new one
                        // lands, or both would be searchable at once.
                        op: IndexOp::Index {
                            replace: prior.is_some_and(|p| p.state == IndexState::Indexed),
                        },
                    });
                }
                ItemStatus::Tombstoned => match prior {
                    // Never pushed, or already withdrawn — nothing owed.
                    None => {}
                    Some(p) if p.state == IndexState::Removed => backlog.synced += 1,
                    Some(p) => backlog.tasks.push(IndexTask {
                        item_id: record.item_id.clone(),
                        entity: record.entity.clone(),
                        path,
                        content_hash: p.content_hash.clone(),
                        size: p.size,
                        mtime_ns: p.mtime_ns,
                        op: IndexOp::Remove,
                    }),
                },
            }
        }
        Ok(backlog)
    }

    /// Record that one task's push into the index succeeded.
    ///
    /// Why: called by the feed layer AFTER the daemon acknowledged the write,
    /// never before — see [`IndexJournal::append`].
    pub fn okg_record_index(
        &self,
        journal: &mut IndexJournal,
        task: &IndexTask,
        now: &str,
    ) -> anyhow::Result<()> {
        journal.append(IndexRecord {
            entity: task.entity.clone(),
            content_hash: task.content_hash.clone(),
            size: task.size,
            mtime_ns: task.mtime_ns,
            state: match task.op {
                IndexOp::Index { .. } => IndexState::Indexed,
                IndexOp::Remove => IndexState::Removed,
            },
            at: now.to_string(),
        })
    }

    /// Absolute path of an entity named `<collection>/<slug>` by a journal.
    ///
    /// Mirrors `ingest::tombstone_item`'s derivation: the slug is the last
    /// segment, and the collection comes from the SPEC rather than the string,
    /// so a hand-edited entity value can never redirect the write.
    fn entity_file_path(&self, spec: &SourceSpec, entity: &str) -> anyhow::Result<PathBuf> {
        let slug = entity.rsplit('/').next().unwrap_or(entity);
        self.entity_path(&spec.collection, slug)
    }
}

#[cfg(test)]
#[path = "index_journal_tests.rs"]
mod tests;
