//! Canonical facts store, redb-backed. The analyzer owns this now (it lived
//! in trusty-search before issue #40 split).
//!
//! Why: distilled `(subject, predicate, object)` knowledge complements raw
//! chunk retrieval. Identity is the triple — re-asserting the same fact
//! merges provenance and overwrites confidence rather than duplicating.
//!
//! What: a redb table from `fact_id (u64)` → JSON-encoded `FactRecord`.
//! `fact_id` is a stable hash of `(subject, predicate, object)`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::types::facts::FactRecord;
use anyhow::{Context, Result};
use redb::{Database, DatabaseError, ReadableDatabase, ReadableTable, TableDefinition};
use xxhash_rust::xxh3::Xxh3;

const FACTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("facts");

/// Stable u64 hash of the canonical `(subject, predicate, object)` triple.
///
/// Why: `FactRecord.id` keys redb persistence; the hash must be stable across
/// Rust toolchain versions so that re-asserting the same triple after a
/// compiler upgrade still hits the same row. The previous implementation
/// used `std::collections::hash_map::DefaultHasher`, which is explicitly
/// *not* stable across releases (see its rustdoc), silently breaking the
/// store on toolchain bumps. xxh3 is a fast, explicitly-versioned algorithm
/// with no implicit per-process seed.
///
/// What: Feeds each field length-prefixed into an `Xxh3` hasher and returns
/// `digest()`. Length-prefixing prevents `("ab","c","d")` and
/// `("a","bc","d")` from colliding.
///
/// Test: `fact_hash("a","b","c")` is deterministic and not equal to
/// `fact_hash("ab","","c")` — see the `fact_hash_is_stable_and_unambiguous`
/// unit test below.
///
/// NOTE: This change invalidates any redb entries written before the switch
/// from `DefaultHasher`. No migration is provided — facts are derivable from
/// source and will be re-asserted on the next analyzer run. Tracked in
/// issue bobmatnyc/trusty-search#64.
pub fn fact_hash(subject: &str, predicate: &str, object: &str) -> u64 {
    let mut h = Xxh3::new();
    for part in [subject, predicate, object] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.digest()
}

/// Build a fresh `FactRecord` with a derived `id` and current `created_at`.
pub fn new_fact(
    subject: impl Into<String>,
    predicate: impl Into<String>,
    object: impl Into<String>,
    index_id: impl Into<String>,
) -> FactRecord {
    let subject = subject.into();
    let predicate = predicate.into();
    let object = object.into();
    FactRecord {
        id: fact_hash(&subject, &predicate, &object),
        subject,
        predicate,
        object,
        confidence: 1.0,
        provenance: Vec::new(),
        index_id: index_id.into(),
        created_at: now_secs(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Classify a `redb::DatabaseError` as an incompatible / unreadable facts file.
///
/// Why: the open path must recover (rebuild empty) from a redb-2.x or otherwise
/// unparseable `facts.redb`, but must NOT do so on transient I/O or lock errors.
/// What: returns `true` for `UpgradeRequired` / `RepairAborted` /
/// `Storage(Corrupted)` / `Storage(Io(InvalidData))`; `false` otherwise.
/// Test: `incompatible_facts_db_is_recreated` exercises the `InvalidData` path.
fn facts_db_is_incompatible(err: &DatabaseError) -> bool {
    use redb::StorageError;
    match err {
        DatabaseError::UpgradeRequired(_) | DatabaseError::RepairAborted => true,
        DatabaseError::Storage(StorageError::Corrupted(_)) => true,
        DatabaseError::Storage(StorageError::Io(io)) => {
            io.kind() == std::io::ErrorKind::InvalidData
        }
        _ => false,
    }
}

/// Open the facts redb at `path`, recreating it empty if the existing file is
/// in an incompatible / old redb format (issue #702).
///
/// Why: redb 4.x cannot open a `facts.redb` written by redb 2.x — the open
/// returns `DatabaseError::UpgradeRequired(_)`. The facts store is a
/// re-derivable knowledge cache, so on that error we move the stale file aside
/// (`facts.redb.v2-incompatible`) and create a fresh empty store rather than
/// crashing the daemon. A loud `ERROR` makes the reset visible.
/// What: tries `Database::create`. On a format-incompatibility error
/// (`UpgradeRequired` / `RepairAborted`) it renames the file aside, logs, and
/// retries the create. Other errors are surfaced verbatim.
/// Test: `incompatible_facts_db_is_recreated`.
fn open_facts_db_or_recreate(path: &Path) -> Result<Database> {
    match Database::create(path) {
        Ok(db) => Ok(db),
        Err(e) if facts_db_is_incompatible(&e) => {
            let mut backup = path.as_os_str().to_os_string();
            backup.push(".v2-incompatible");
            let backup = std::path::PathBuf::from(backup);
            std::fs::rename(path, &backup).with_context(|| {
                format!(
                    "back up incompatible-format facts redb {} before recreating",
                    path.display()
                )
            })?;
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                "facts redb is in an incompatible/old format (redb 2.x); moved it aside and \
                 creating a fresh empty facts store — facts must be re-derived"
            );
            Database::create(path).context("create fresh facts redb after moving incompatible file")
        }
        Err(e) => Err(anyhow::Error::new(e)).context("open facts redb"),
    }
}

/// redb-backed store for `FactRecord`s. Cheap to clone — `Arc<Database>`.
#[derive(Clone)]
pub struct FactStore {
    db: Arc<Database>,
}

impl FactStore {
    /// Open (creating if absent) the facts redb at `path`.
    ///
    /// Why: the facts store is a `(subject, predicate, object)` knowledge cache
    /// that recovers by being re-derived from analysis. Issue #702: redb 4.x
    /// cannot open a `facts.redb` written by redb 2.x — without a guard the
    /// daemon would crash on the first warm boot after the binary upgrade.
    /// What: opens via [`open_facts_db_or_recreate`], which on an
    /// incompatible-format error moves the stale file aside
    /// (`*.v2-incompatible`) and creates a fresh empty store, then materialises
    /// the facts table so reads on a brand-new file succeed.
    /// Test: `incompatible_facts_db_is_recreated`.
    pub fn open(path: &Path) -> Result<Self> {
        let db = open_facts_db_or_recreate(path)?;
        let txn = db.begin_write().context("begin facts init txn")?;
        {
            let _t = txn
                .open_table(FACTS_TABLE)
                .context("open facts table for init")?;
        }
        txn.commit().context("commit facts init txn")?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Upsert a fact. On id collision (same triple), provenance is set-merged
    /// and `confidence` is overwritten. `created_at` is preserved.
    pub fn upsert(&self, mut fact: FactRecord) -> Result<()> {
        // Re-derive id so callers building the struct literal can't desync it.
        fact.id = fact_hash(&fact.subject, &fact.predicate, &fact.object);
        // Clamp confidence to [0,1] to match the trusty-search builder semantics.
        fact.confidence = fact.confidence.clamp(0.0, 1.0);

        let txn = self.db.begin_write().context("begin upsert txn")?;
        {
            let mut table = txn
                .open_table(FACTS_TABLE)
                .context("open facts table for upsert")?;

            let merged =
                if let Some(existing_bytes) = table.get(fact.id).context("read existing fact")? {
                    let existing: FactRecord = serde_json::from_slice(existing_bytes.value())
                        .context("decode existing fact for merge")?;
                    let mut prov_set: HashSet<String> = existing.provenance.into_iter().collect();
                    for p in &fact.provenance {
                        prov_set.insert(p.clone());
                    }
                    let mut provenance: Vec<String> = prov_set.into_iter().collect();
                    provenance.sort();
                    FactRecord {
                        id: fact.id,
                        subject: fact.subject,
                        predicate: fact.predicate,
                        object: fact.object,
                        confidence: fact.confidence,
                        provenance,
                        index_id: fact.index_id,
                        created_at: existing.created_at,
                    }
                } else {
                    fact
                };

            let bytes = serde_json::to_vec(&merged).context("encode fact")?;
            table
                .insert(merged.id, bytes.as_slice())
                .context("insert fact")?;
        }
        txn.commit().context("commit upsert txn")?;
        Ok(())
    }

    /// Filter facts. Any `None` field matches anything.
    pub fn query(
        &self,
        subject: Option<&str>,
        predicate: Option<&str>,
        object: Option<&str>,
    ) -> Result<Vec<FactRecord>> {
        let txn = self.db.begin_read().context("begin query txn")?;
        let table = txn
            .open_table(FACTS_TABLE)
            .context("open facts table for query")?;

        let mut out = Vec::new();
        for row in table.iter().context("iter facts")? {
            let (_, v) = row.context("read fact row")?;
            let fact: FactRecord =
                serde_json::from_slice(v.value()).context("decode fact during query")?;
            if let Some(s) = subject {
                if fact.subject != s {
                    continue;
                }
            }
            if let Some(p) = predicate {
                if fact.predicate != p {
                    continue;
                }
            }
            if let Some(o) = object {
                if fact.object != o {
                    continue;
                }
            }
            out.push(fact);
        }
        Ok(out)
    }

    pub fn all(&self) -> Result<Vec<FactRecord>> {
        self.query(None, None, None)
    }

    pub fn delete(&self, id: u64) -> Result<bool> {
        let txn = self.db.begin_write().context("begin delete txn")?;
        let removed = {
            let mut table = txn
                .open_table(FACTS_TABLE)
                .context("open facts table for delete")?;
            let was_present = table.remove(id).context("delete fact")?.is_some();
            was_present
        };
        txn.commit().context("commit delete txn")?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (FactStore, TempDir) {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("facts.redb");
        let store = FactStore::open(&path).expect("open facts store");
        (store, tmp)
    }

    /// Why: #702 graceful-handling — a `facts.redb` redb 4.x cannot open (a
    /// stale redb-2.x file, simulated with garbage bytes) must NOT crash the
    /// daemon; it is moved aside and replaced with a fresh empty store.
    /// What: writes garbage to `facts.redb`, opens via `FactStore::open`,
    /// asserts the open succeeds, the backup exists, and the store is empty.
    /// Test: this test.
    #[test]
    fn incompatible_facts_db_is_recreated() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("facts.redb");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
            .unwrap();

        let store = FactStore::open(&path).expect("incompatible facts db must recover, not error");
        assert!(
            path.with_file_name("facts.redb.v2-incompatible").exists(),
            "incompatible facts file must be backed up"
        );
        assert!(
            store.query(None, None, None).unwrap().is_empty(),
            "recreated facts store must start empty"
        );
    }

    #[test]
    fn upsert_and_query_by_subject() {
        let (store, _tmp) = make_store();
        let mut f = new_fact("fn search", "implements", "trait Searcher", "test");
        f.confidence = 0.9;
        f.provenance = vec!["src/indexer.rs:1:10".into()];
        store.upsert(f).unwrap();

        let hits = store.query(Some("fn search"), None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].object, "trait Searcher");
    }

    #[test]
    fn upsert_dedupes_and_merges_provenance() {
        let (store, _tmp) = make_store();
        let mut a = new_fact("X", "implements", "Y", "i1");
        a.provenance = vec!["c1".into()];
        store.upsert(a).unwrap();
        let mut b = new_fact("X", "implements", "Y", "i1");
        b.provenance = vec!["c2".into()];
        store.upsert(b).unwrap();

        let all = store.all().unwrap();
        assert_eq!(all.len(), 1);
        let mut prov = all[0].provenance.clone();
        prov.sort();
        assert_eq!(prov, vec!["c1".to_string(), "c2".to_string()]);
    }

    #[test]
    fn confidence_clamps_on_upsert() {
        let (store, _tmp) = make_store();
        let mut f = new_fact("a", "b", "c", "i");
        f.confidence = 2.5;
        store.upsert(f).unwrap();
        assert_eq!(store.all().unwrap()[0].confidence, 1.0);
    }

    #[test]
    fn fact_hash_is_stable_and_unambiguous() {
        // Stability: same inputs → same hash within a process run. The
        // underlying xxh3 algorithm is also versioned and stable across
        // Rust toolchain upgrades, which is the whole point of the switch
        // away from DefaultHasher (issue bobmatnyc/trusty-search#64).
        let h1 = fact_hash("a", "b", "c");
        let h2 = fact_hash("a", "b", "c");
        assert_eq!(h1, h2);

        // Length-prefixing must prevent ambiguous concatenation collisions:
        // ("ab","","c") and ("a","b","c") would collide under naive concat.
        assert_ne!(fact_hash("ab", "", "c"), fact_hash("a", "b", "c"));

        // Field-order sensitivity: swapping subject and object must change
        // the hash, since the triple is directional.
        assert_ne!(fact_hash("a", "b", "c"), fact_hash("c", "b", "a"));
    }

    #[test]
    fn delete_returns_true_then_false() {
        let (store, _tmp) = make_store();
        let f = new_fact("X", "y", "Z", "i");
        let id = f.id;
        store.upsert(f).unwrap();
        assert!(store.delete(id).unwrap());
        assert!(!store.delete(id).unwrap());
    }
}
