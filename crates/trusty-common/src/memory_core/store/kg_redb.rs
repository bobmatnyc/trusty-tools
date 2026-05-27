//! redb-backed storage engine for the temporal knowledge graph.
//!
//! Why: The KG previously rode on rusqlite + r2d2, which carries a heavy native
//! dependency chain and a 30s default connect timeout that stalls daemon
//! startup when a palace's `kg.db` is corrupt. redb is a pure-Rust embedded
//! transactional k/v store with O(log n) range scans and no native deps.
//! Issue #44 swaps the internals; #47 will retire the sqlite code path.
//! What: `KgStoreRedb` wraps `redb::Database` and implements every method that
//! `KnowledgeGraph` exposes — assert/retract/query/list for triples, and
//! upsert/load/delete for drawers. Composite key encodings, table definitions,
//! and value codecs live in `kg_store.rs`.
//! Test: See `tests` module — round-trips, retract semantics, persistence
//! across reopen, drawer CRUD, count_active.

use crate::memory_core::palace::Drawer;
use crate::memory_core::store::concurrent_open::{OpenMode, SnapshotGuard, try_open_or_snapshot};
use crate::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, DrawerRecord, TRIPLES, TRIPLES_BY_OBJECT, TRIPLES_BY_PREDICATE,
    TripleValue, decode_triple_key, decode_u64, decode_value, encode_object_index_key,
    encode_predicate_index_key, encode_triple_key, encode_u64, encode_value, subject_prefix,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use uuid::Uuid;

use super::kg::Triple;

/// Pre-#61 on-disk shape of a drawer row (without `drawer_type` /
/// `expires_at_ms`).
///
/// Why: postcard is positional — it refuses to decode legacy rows as the
/// new `DrawerRecord` because the trailing optional fields don't exist in
/// the bytes. We try the current shape first and fall back to this struct
/// to migrate the data forward on read.
/// What: Mirrors the historical struct field-for-field; `From` lifts it
/// into the modern `DrawerRecord` with the new fields defaulted.
/// Test: `drawer_type_round_trips_through_redb` plus
/// `drawer_record_legacy_decode_without_new_fields` in `kg_store`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LegacyDrawerRecord {
    room_id: String,
    content: String,
    importance: f32,
    tags: Vec<String>,
    source_file: Option<String>,
    created_at_ms: i64,
}

impl From<LegacyDrawerRecord> for DrawerRecord {
    fn from(l: LegacyDrawerRecord) -> Self {
        DrawerRecord {
            room_id: l.room_id,
            content: l.content,
            importance: l.importance,
            tags: l.tags,
            source_file: l.source_file,
            created_at_ms: l.created_at_ms,
            drawer_type: None,
            expires_at_ms: None,
        }
    }
}

/// Build a `DrawerRecord` from a live `Drawer`.
///
/// Why: Three call sites (single upsert, bulk import, batch upsert) all
/// build the same record; centralising the construction keeps the
/// drawer_type / expires_at_ms fields (issue #61) in sync across them.
/// What: Copies the persisted fields and converts the optional
/// `DrawerType` and `expires_at` into their on-disk representations.
/// Test: Indirect via `upsert_drawer_then_load_drawers_round_trips` and
/// the new `drawer_type_round_trips_through_redb`.
fn drawer_to_record(drawer: &Drawer) -> DrawerRecord {
    DrawerRecord {
        room_id: drawer.room_id.to_string(),
        content: drawer.content.clone(),
        importance: drawer.importance,
        tags: drawer.tags.clone(),
        source_file: drawer
            .source_file
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        created_at_ms: drawer.created_at.timestamp_millis(),
        drawer_type: Some(drawer.drawer_type.as_str().to_string()),
        expires_at_ms: drawer.expires_at.map(|d| d.timestamp_millis()),
    }
}

/// Parse a `DrawerType` tag back from its on-disk string representation.
///
/// Why: We persist the variant name as a string so the schema stays stable
/// when new variants are added; readers tolerate unknown / absent tags by
/// returning `DrawerType::Unknown` (the migration default).
/// What: Match against the known variant names; anything else falls back
/// to `DrawerType::Unknown`.
/// Test: Indirect via `drawer_type_round_trips_through_redb`.
fn parse_drawer_type(tag: Option<&str>) -> crate::memory_core::palace::DrawerType {
    use crate::memory_core::palace::DrawerType;
    match tag {
        Some("UserFact") => DrawerType::UserFact,
        Some("SessionEvent") => DrawerType::SessionEvent,
        Some("AgentNote") => DrawerType::AgentNote,
        Some("Commit") => DrawerType::Commit,
        _ => DrawerType::Unknown,
    }
}

/// Sentinel returned by every write method when the store is in snapshot
/// (read-only) mode.
///
/// Why: Issue #59 — a stdio MCP client that falls back to a snapshot must
/// reject writes with a clear message so the caller sees "writes go
/// through the HTTP daemon" instead of a silent divergence where the
/// write succeeds locally but never reaches the live file.
/// What: A `&'static str` so call sites can wrap it in `anyhow::anyhow!`
/// without allocating.
/// Test: `write_on_snapshot_returns_read_only_error`.
pub(crate) const READ_ONLY_ERROR_MSG: &str = "palace is read-only: HTTP daemon holds the write lock — \
     route writes through the daemon's HTTP API or stop the daemon \
     before retrying via stdio";

/// Shared per-path state: the open `Database` plus its open mode and
/// optional snapshot guard. Bundled into one `Arc` so every cache hit
/// inherits the same snapshot lifetime (the guard's `Drop` removes the
/// snapshot file on disk).
///
/// Why: Issue #59 — when the live redb file is locked by another process
/// (typically the HTTP daemon), `try_open_or_snapshot` copies it to a
/// process-local snapshot. The snapshot's `SnapshotGuard` must live as
/// long as any handle to the resulting `Database` to keep the temp file
/// alive for reads. Bundling them in one `Arc` ties the two lifetimes
/// together.
/// What: Carries the open `Database`, the `OpenMode`, and the snapshot
/// guard. `SnapshotGuard::noop()` is used for the read/write path so the
/// shape is uniform.
/// Test: Indirect via every `KgStoreRedb::open` call.
#[derive(Debug)]
struct KgDbState {
    db: Arc<Database>,
    mode: OpenMode,
    _snapshot_guard: SnapshotGuard,
}

/// Why: redb forbids more than one in-process `Database` handle to the same
/// file ("Database already open. Cannot acquire lock."). The trusty stack
/// regularly opens the same palace from multiple registries within a single
/// process (e.g. test setup + `AppState`, or background dreamer + foreground
/// handle); SQLite previously allowed this so we must preserve it. The fix
/// is a process-global cache of `Weak<KgDbState>` keyed by canonical path —
/// when any handle is alive we hand it back; once all handles drop the entry
/// expires and the next `open` creates a fresh `Database`.
/// What: Lazily-initialised global mutex over a `HashMap<canonical_path,
/// Weak<KgDbState>>`.
/// Test: `multiple_handles_to_same_path_share_database`.
fn db_cache() -> &'static Mutex<HashMap<PathBuf, Weak<KgDbState>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<KgDbState>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Why: The cache key must be path-canonical so `/var/tmp/x` and
/// `/private/var/tmp/x` (the same file via symlink) collapse to one entry.
/// What: Tries `canonicalize`; on failure falls back to the original path so
/// brand-new files (not yet on disk) still work.
/// Test: Indirect — exercised by every `open` call.
fn canonical_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Why: All KG callers go through a single `KnowledgeGraph` handle that is
/// cheap to clone and Send + Sync. Holding `Arc<Database>` lets background
/// tasks (Dreamer, compaction) share the same db without re-opening.
/// What: Owns the redb `Database` plus the on-disk path for diagnostics.
/// Test: Implicit — every test below constructs one.
#[derive(Clone)]
pub struct KgStoreRedb {
    state: Arc<KgDbState>,
    #[allow(dead_code)]
    path: PathBuf,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn ms_to_dt(ms: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms).context("invalid millisecond timestamp")
}

fn triple_from_parts(subject: String, predicate: String, v: TripleValue) -> Result<Triple> {
    let valid_from = ms_to_dt(v.valid_from_ms)?;
    let valid_to = match v.valid_to_ms {
        Some(ms) => Some(ms_to_dt(ms)?),
        None => None,
    };
    Ok(Triple {
        subject,
        predicate,
        object: v.object,
        valid_from,
        valid_to,
        confidence: v.confidence,
        provenance: v.provenance,
    })
}

impl KgStoreRedb {
    /// Open or create the redb database at `path`.
    ///
    /// Why: Creating the file plus initializing every table must be idempotent
    /// so daemon restarts succeed without manual setup. redb's
    /// `Database::create` opens an existing file or creates a fresh one.
    /// What: Opens the file, then in a single write transaction touches every
    /// table so the file always carries a stable schema even when no data has
    /// been written.
    /// Test: `open_then_reopen_persists_state`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create kg db parent dir {}", parent.display()))?;
        }

        // Reuse an existing `Arc<KgDbState>` if any handle to this path is
        // still alive — see `db_cache` for the rationale.
        {
            let mut cache = db_cache().lock().expect("db_cache poisoned");
            let key = canonical_key(path);
            if let Some(weak) = cache.get(&key)
                && let Some(state) = weak.upgrade()
            {
                return Ok(Self {
                    state,
                    path: path.to_path_buf(),
                });
            }
            // Either no entry or a dead Weak — fall through to create.
            cache.remove(&key);
        }

        // Try a normal exclusive open; on `DatabaseAlreadyOpen` fall back
        // to a process-local snapshot copy so a stdio MCP client can read
        // a palace while the HTTP daemon owns the live file (issue #59).
        let (db, snapshot_guard, mode) = try_open_or_snapshot(path)
            .with_context(|| format!("open kg redb at {}", path.display()))?;

        // Touch every table in a single write txn so they exist on disk
        // even before the first write. Skip this step in snapshot mode
        // because (a) the live file already initialised every table — we
        // copied a fully-formed redb image — and (b) any write we make
        // here would only land in the throw-away snapshot, masking the
        // read-only intent of every later write rejection.
        if matches!(mode, OpenMode::ReadWrite) {
            let wtx = db.begin_write().context("begin init txn")?;
            {
                let _ = wtx.open_table(TRIPLES).context("init triples table")?;
                let _ = wtx
                    .open_table(TRIPLES_BY_OBJECT)
                    .context("init triples_by_object table")?;
                let _ = wtx
                    .open_table(TRIPLES_BY_PREDICATE)
                    .context("init triples_by_predicate table")?;
                let _ = wtx
                    .open_table(ACTIVE_SUBJECT_COUNTS)
                    .context("init active_subject_counts table")?;
                let _ = wtx.open_table(DRAWERS).context("init drawers table")?;
            }
            wtx.commit().context("commit init txn")?;
        }

        let state = Arc::new(KgDbState {
            db,
            mode,
            _snapshot_guard: snapshot_guard,
        });
        {
            let mut cache = db_cache().lock().expect("db_cache poisoned");
            // Use the post-create canonical path so symlinks resolve.
            let key = canonical_key(path);
            cache.insert(key, Arc::downgrade(&state));
        }

        Ok(Self {
            state,
            path: path.to_path_buf(),
        })
    }

    /// Whether this store is operating against a read-only snapshot.
    ///
    /// Why: Issue #59 — `KnowledgeGraph` exposes this through to
    /// `PalaceHandle::is_read_only` so write paths can short-circuit
    /// before touching the store. Cheap field read, no I/O.
    /// What: Returns `true` when the underlying database was opened via
    /// the snapshot fallback rather than directly.
    /// Test: `write_on_snapshot_returns_read_only_error`.
    pub fn is_read_only(&self) -> bool {
        self.state.mode.is_read_only()
    }

    /// Internal accessor used by every method that previously read
    /// `self.db`. Centralising it lets the cache and snapshot guard live
    /// inside `KgDbState` without rewriting every call site.
    fn db(&self) -> &Database {
        &self.state.db
    }

    /// Reject the operation when the store is in snapshot mode.
    ///
    /// Why: Every write path (`assert`, `retract`, drawer upsert/delete)
    /// must surface the same actionable error so users see the same
    /// guidance regardless of which mutation they attempted.
    /// What: Returns `Err(READ_ONLY_ERROR_MSG)` when `is_read_only()`,
    /// otherwise `Ok(())`.
    /// Test: `write_on_snapshot_returns_read_only_error`.
    fn check_writable(&self) -> Result<()> {
        if self.is_read_only() {
            Err(anyhow::anyhow!(READ_ONLY_ERROR_MSG))
        } else {
            Ok(())
        }
    }

    /// Assert a triple. If an active row exists for `(subject, predicate)` it
    /// is closed (valid_to = now) and removed from secondary indexes; then the
    /// new triple is inserted and indexed.
    ///
    /// Why: Temporal model — facts have intervals. New assertion supersedes
    /// the prior active row instead of overwriting it, preserving history.
    /// What: Single write transaction over TRIPLES + secondary indexes +
    /// ACTIVE_SUBJECT_COUNTS so the invariant "at most one active row per
    /// (subject, predicate)" can never be observed broken.
    /// Test: `assert_then_query_returns_triple`, `assert_supersedes_prior`.
    pub fn assert(&self, triple: &Triple) -> Result<()> {
        self.check_writable()?;
        let close_ms = triple.valid_from.timestamp_millis();
        let new_value = TripleValue {
            object: triple.object.clone(),
            valid_from_ms: triple.valid_from.timestamp_millis(),
            valid_to_ms: triple.valid_to.map(|dt| dt.timestamp_millis()),
            confidence: triple.confidence,
            provenance: triple.provenance.clone(),
        };

        let wtx = self.db().begin_write().context("begin assert txn")?;
        {
            let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;

            let key = encode_triple_key(&triple.subject, &triple.predicate);

            // Look up existing active row at this (subject, predicate). Because
            // we only ever store one row per (subject, predicate) key (the most
            // recent), checking by direct key is sufficient.
            let mut closed_any = false;
            let prior_opt: Option<TripleValue> = {
                let existing = triples
                    .get(key.as_slice())
                    .context("read existing triple")?;
                match existing {
                    Some(g) => Some(decode_value(g.value()).context("decode prior triple")?),
                    None => None,
                }
            };
            if let Some(prior) = prior_opt {
                if prior.valid_to_ms.is_none() {
                    // Active — close it by setting valid_to and writing back.
                    // But since we're about to overwrite with the new row, we
                    // only need to drop the secondary index entries and
                    // decrement the active counter.
                    let obj_key =
                        encode_object_index_key(&prior.object, &triple.subject, &triple.predicate);
                    by_object
                        .remove(obj_key.as_slice())
                        .context("remove prior object index")?;
                    let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
                    by_predicate
                        .remove(pred_key.as_slice())
                        .context("remove prior predicate index")?;
                    closed_any = true;
                }
                // History preservation: write the closed prior row into a
                // history key. We use a synthetic key suffix so it does not
                // collide with the active row. Format: `[hist:][orig key]
                // [valid_from_ms BE]`. This keeps dump_all_triples honest.
                if prior.valid_to_ms.is_none() {
                    let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                    hist_key.extend_from_slice(b"hist:");
                    hist_key.extend_from_slice(&key);
                    hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
                    let closed = TripleValue {
                        valid_to_ms: Some(close_ms),
                        ..prior
                    };
                    let closed_bytes = encode_value(&closed).context("encode closed prior")?;
                    triples
                        .insert(hist_key.as_slice(), closed_bytes.as_slice())
                        .context("insert closed history row")?;
                }
            }

            // Insert / overwrite active row.
            let new_bytes = encode_value(&new_value).context("encode new triple")?;
            triples
                .insert(key.as_slice(), new_bytes.as_slice())
                .context("insert new triple")?;

            // Insert secondary indexes for the new active row (only when it
            // is itself active — `assert` with `valid_to = Some(_)` would be
            // a closed-on-arrival row that should not appear in indexes).
            if new_value.valid_to_ms.is_none() {
                let obj_key =
                    encode_object_index_key(&new_value.object, &triple.subject, &triple.predicate);
                by_object
                    .insert(obj_key.as_slice(), [].as_slice())
                    .context("insert new object index")?;
                let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
                by_predicate
                    .insert(pred_key.as_slice(), [].as_slice())
                    .context("insert new predicate index")?;

                // Maintain count: net change is 0 if we just closed one and
                // opened one; +1 if there was no prior active row.
                if !closed_any {
                    let subj_key = triple.subject.as_bytes();
                    let prev = counts
                        .get(subj_key)
                        .context("read prior count")?
                        .map(|v| decode_u64(v.value()))
                        .unwrap_or(0);
                    let next = prev.saturating_add(1);
                    counts
                        .insert(subj_key, encode_u64(next).as_slice())
                        .context("update active count")?;
                }
            } else if closed_any {
                // Closed-on-arrival row replacing an active one — decrement.
                let subj_key = triple.subject.as_bytes();
                let prev = counts
                    .get(subj_key)
                    .context("read prior count")?
                    .map(|v| decode_u64(v.value()))
                    .unwrap_or(0);
                let next = prev.saturating_sub(1);
                if next == 0 {
                    counts.remove(subj_key).context("remove zero count")?;
                } else {
                    counts
                        .insert(subj_key, encode_u64(next).as_slice())
                        .context("update active count")?;
                }
            }
        }
        wtx.commit().context("commit assert txn")?;
        Ok(())
    }

    /// Close the active triple for `(subject, predicate)` without inserting a
    /// replacement. Returns the number of rows closed (0 or 1).
    ///
    /// Why: `assert` always closes-and-replaces; retract is the way to say
    /// "this fact is no longer true and has no successor" — used by
    /// `remove_prompt_fact`.
    /// What: Reads the row at `(subject, predicate)`. If active, writes a
    /// history copy with `valid_to = now`, drops the active row from the
    /// primary table, removes secondary indexes, and decrements the count.
    /// Test: `retract_closes_active_interval`.
    pub fn retract(&self, subject: &str, predicate: &str) -> Result<usize> {
        self.check_writable()?;
        let key = encode_triple_key(subject, predicate);
        let close_ms = now_ms();
        let wtx = self.db().begin_write().context("begin retract txn")?;
        let closed;
        {
            let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;

            let prior_opt: Option<TripleValue> = {
                let existing = triples
                    .get(key.as_slice())
                    .context("lookup active triple for retract")?;
                match existing {
                    Some(g) => Some(decode_value(g.value()).context("decode prior for retract")?),
                    None => None,
                }
            };
            match prior_opt {
                Some(prior) => {
                    if prior.valid_to_ms.is_none() {
                        // Move to history.
                        let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                        hist_key.extend_from_slice(b"hist:");
                        hist_key.extend_from_slice(&key);
                        hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
                        let closed_v = TripleValue {
                            valid_to_ms: Some(close_ms),
                            ..prior.clone()
                        };
                        let bytes = encode_value(&closed_v).context("encode retract history")?;
                        triples
                            .insert(hist_key.as_slice(), bytes.as_slice())
                            .context("insert retract history row")?;
                        // Remove active row + indexes.
                        triples
                            .remove(key.as_slice())
                            .context("remove active row for retract")?;
                        let obj_key = encode_object_index_key(&prior.object, subject, predicate);
                        by_object
                            .remove(obj_key.as_slice())
                            .context("remove object index for retract")?;
                        let pred_key = encode_predicate_index_key(predicate, subject);
                        by_predicate
                            .remove(pred_key.as_slice())
                            .context("remove predicate index for retract")?;
                        // Decrement count.
                        let subj_key = subject.as_bytes();
                        let prev = counts
                            .get(subj_key)
                            .context("read prior count for retract")?
                            .map(|v| decode_u64(v.value()))
                            .unwrap_or(0);
                        let next = prev.saturating_sub(1);
                        if next == 0 {
                            counts.remove(subj_key).context("remove zero count")?;
                        } else {
                            counts
                                .insert(subj_key, encode_u64(next).as_slice())
                                .context("update count after retract")?;
                        }
                        closed = 1;
                    } else {
                        // Row exists but is already closed — nothing to do.
                        closed = 0;
                    }
                }
                None => {
                    closed = 0;
                }
            }
        }
        wtx.commit().context("commit retract txn")?;
        Ok(closed)
    }

    /// Return all currently active triples for `subject`.
    ///
    /// Why: Most queries want "what is true *now*". The primary TRIPLES table
    /// holds at most one active row per (subject, predicate), so a prefix scan
    /// on `subject_prefix(subject)` returns at most one row per predicate.
    /// What: Range scan over `[subject_prefix..end_of_prefix]`, filter rows
    /// whose `valid_to_ms.is_none()`, and map to `Triple`.
    /// Test: `assert_then_query_returns_triple`.
    pub fn query_active(&self, subject: &str) -> Result<Vec<Triple>> {
        let prefix = subject_prefix(subject);
        let rtx = self.db().begin_read().context("begin query_active txn")?;
        let triples = rtx
            .open_table(TRIPLES)
            .context("open triples table for query_active")?;
        let mut out = Vec::new();
        let mut end = prefix.clone();
        // Build exclusive end key by appending 0xFF — every valid key with this
        // subject prefix sorts before it.
        end.push(0xFF);
        let range = triples
            .range::<&[u8]>(prefix.as_slice()..end.as_slice())
            .context("range scan for query_active")?;
        for entry in range {
            let (k, v) = entry.context("read row in query_active")?;
            // Skip history rows (which we never put under the active prefix
            // anyway, but defensive against future encoders).
            if k.value().starts_with(b"hist:") {
                continue;
            }
            let value: TripleValue =
                decode_value(v.value()).context("decode TripleValue in query_active")?;
            if value.valid_to_ms.is_some() {
                continue;
            }
            let (s, p) = match decode_triple_key(k.value()) {
                Some(parts) => parts,
                None => continue,
            };
            if s != subject {
                continue;
            }
            out.push(triple_from_parts(s, p, value)?);
        }
        Ok(out)
    }

    /// List up to `limit` distinct subjects that have at least one active
    /// triple, ordered alphabetically.
    ///
    /// Why: KG Explorer UI browses subjects without knowing one upfront.
    /// What: Iterate ACTIVE_SUBJECT_COUNTS (keyed by subject bytes, sorted
    /// alphabetically), collect subjects whose count is > 0, take `limit`.
    /// Test: `list_subjects_returns_distinct_active_subjects`.
    pub fn list_subjects(&self, limit: usize) -> Result<Vec<String>> {
        let rtx = self.db().begin_read().context("begin list_subjects txn")?;
        let counts = rtx
            .open_table(ACTIVE_SUBJECT_COUNTS)
            .context("open active_subject_counts")?;
        let mut out = Vec::new();
        for entry in counts.iter().context("iter counts")? {
            if out.len() >= limit {
                break;
            }
            let (k, v) = entry.context("read counts row")?;
            if decode_u64(v.value()) == 0 {
                continue;
            }
            let s = std::str::from_utf8(k.value())
                .context("invalid utf8 in subject counts key")?
                .to_string();
            out.push(s);
        }
        Ok(out)
    }

    /// List up to `limit` `(subject, count)` rows for subjects with at least
    /// one active triple, ordered alphabetically by subject.
    ///
    /// Why: KG Explorer UI shows a count badge next to each subject; computing
    /// the count server-side in one pass avoids one query per subject.
    /// What: Iterate ACTIVE_SUBJECT_COUNTS in key order, take rows with
    /// non-zero counts up to `limit`.
    /// Test: `list_subjects_with_counts_returns_grouped_counts`.
    pub fn list_subjects_with_counts(&self, limit: usize) -> Result<Vec<(String, u64)>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin list_subjects_with_counts txn")?;
        let counts = rtx
            .open_table(ACTIVE_SUBJECT_COUNTS)
            .context("open active_subject_counts")?;
        let mut out = Vec::new();
        for entry in counts.iter().context("iter counts")? {
            if out.len() >= limit {
                break;
            }
            let (k, v) = entry.context("read counts row")?;
            let c = decode_u64(v.value());
            if c == 0 {
                continue;
            }
            let s = std::str::from_utf8(k.value())
                .context("invalid utf8 in subject counts key")?
                .to_string();
            out.push((s, c));
        }
        Ok(out)
    }

    /// List up to `limit` active triples ordered by `valid_from` descending,
    /// skipping the first `offset` rows.
    ///
    /// Why: KG Explorer's "All" mode pages through every active triple.
    /// What: Full scan of TRIPLES, filter active rows, sort by valid_from desc,
    /// take the requested window. We do a full scan because redb has no
    /// secondary index on valid_from — acceptable since the active set is
    /// bounded by application sizing.
    /// Test: `list_active_returns_ordered_window`.
    pub fn list_active(&self, limit: usize, offset: usize) -> Result<Vec<Triple>> {
        let rtx = self.db().begin_read().context("begin list_active txn")?;
        let triples = rtx
            .open_table(TRIPLES)
            .context("open triples table for list_active")?;
        let mut rows = Vec::new();
        for entry in triples.iter().context("iter triples")? {
            let (k, v) = entry.context("read triples row")?;
            if k.value().starts_with(b"hist:") {
                continue;
            }
            let value: TripleValue =
                decode_value(v.value()).context("decode TripleValue in list_active")?;
            if value.valid_to_ms.is_some() {
                continue;
            }
            let (s, p) = match decode_triple_key(k.value()) {
                Some(parts) => parts,
                None => continue,
            };
            rows.push((value.valid_from_ms, s, p, value));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        let mut out = Vec::new();
        for (_, s, p, value) in rows.into_iter().skip(offset).take(limit) {
            out.push(triple_from_parts(s, p, value)?);
        }
        Ok(out)
    }

    /// Count currently active triples (sum of ACTIVE_SUBJECT_COUNTS).
    ///
    /// Why: Dashboard tally of live facts. Maintained incrementally so it is
    /// O(distinct subjects) rather than O(history).
    /// What: Iterate ACTIVE_SUBJECT_COUNTS, sum values.
    /// Test: `count_active_triples_returns_live_only`.
    pub fn count_active_triples(&self) -> u64 {
        let rtx = match self.db().begin_read() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("count_active_triples: begin_read failed: {e:#}");
                return 0;
            }
        };
        let counts = match rtx.open_table(ACTIVE_SUBJECT_COUNTS) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("count_active_triples: open table failed: {e:#}");
                return 0;
            }
        };
        let mut total: u64 = 0;
        let iter = match counts.iter() {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("count_active_triples: iter failed: {e:#}");
                return 0;
            }
        };
        for entry in iter {
            match entry {
                Ok((_, v)) => total = total.saturating_add(decode_u64(v.value())),
                Err(e) => {
                    tracing::warn!("count_active_triples: row read failed: {e:#}");
                    continue;
                }
            }
        }
        total
    }

    /// No-op checkpoint hook.
    ///
    /// Why: SQLite needed `PRAGMA wal_checkpoint(PASSIVE)` to bound the WAL.
    /// redb manages its own write-ahead log internally and does not require a
    /// manual checkpoint; the call is kept for API compatibility.
    /// What: Returns immediately.
    /// Test: Implicit via `checkpoint_is_noop`.
    pub fn checkpoint(&self) -> Result<()> {
        // redb manages its commit log internally — nothing to do.
        Ok(())
    }

    /// Persist a drawer's metadata.
    ///
    /// Why: HNSW only stores vectors keyed by UUID; without drawer metadata
    /// persisted alongside, vector hits map to nothing after a cold restart.
    /// What: Serialize the drawer to `DrawerRecord` and write under its UUID
    /// bytes in DRAWERS.
    /// Test: `upsert_drawer_then_load_drawers_round_trips`.
    pub fn upsert_drawer(&self, drawer: &Drawer) -> Result<()> {
        self.check_writable()?;
        let record = drawer_to_record(drawer);
        let bytes = encode_value(&record).context("encode drawer record")?;
        let id_bytes = *drawer.id.as_bytes();
        let wtx = self.db().begin_write().context("begin upsert_drawer txn")?;
        {
            let mut drawers = wtx.open_table(DRAWERS).context("open drawers table")?;
            drawers
                .insert(id_bytes.as_slice(), bytes.as_slice())
                .context("insert drawer record")?;
        }
        wtx.commit().context("commit upsert_drawer txn")?;
        Ok(())
    }

    /// Remove a drawer by UUID.
    ///
    /// Why: Forgetting must clear both the vector index and the persistent
    /// metadata row — otherwise restart resurrects the drawer.
    /// What: Remove the row keyed by UUID bytes from DRAWERS. No-op on unknown id.
    /// Test: `delete_drawer_removes_row`.
    pub fn delete_drawer(&self, id: Uuid) -> Result<()> {
        self.check_writable()?;
        let id_bytes = *id.as_bytes();
        let wtx = self.db().begin_write().context("begin delete_drawer txn")?;
        {
            let mut drawers = wtx.open_table(DRAWERS).context("open drawers table")?;
            drawers
                .remove(id_bytes.as_slice())
                .context("remove drawer record")?;
        }
        wtx.commit().context("commit delete_drawer txn")?;
        Ok(())
    }

    /// Delete all active triples whose subject matches `subject`.
    ///
    /// Why: Cascade-delete on drawer removal (issue #278) — when a drawer is
    /// forgotten, every triple extracted from it (identified by the
    /// `drawer:<uuid>` subject prefix) must be removed so the KG does not
    /// accumulate orphaned edges.
    /// What: Performs a prefix scan over TRIPLES using `subject_prefix(subject)`,
    /// collects every active (non-history, non-closed) `(subject, predicate)`
    /// pair, and retracts each via the existing `retract` path so secondary
    /// indexes and the active count table are kept consistent. Returns the
    /// number of active rows closed.
    /// Test: `cascade_delete_removes_triples_for_subject` in this module's
    /// test section.
    pub fn delete_by_subject(&self, subject: &str) -> Result<usize> {
        self.check_writable()?;
        let prefix = subject_prefix(subject);
        let mut to_retract: Vec<(String, String)> = Vec::new();
        {
            let rtx = self.db().begin_read().context("begin delete_by_subject read")?;
            let triples = rtx
                .open_table(TRIPLES)
                .context("open triples for delete_by_subject scan")?;
            let mut end = prefix.clone();
            end.push(0xFF);
            let range = triples
                .range::<&[u8]>(prefix.as_slice()..end.as_slice())
                .context("range scan for delete_by_subject")?;
            for entry in range {
                let (k, v) = entry.context("read row in delete_by_subject")?;
                if k.value().starts_with(b"hist:") {
                    continue;
                }
                let value: TripleValue =
                    decode_value(v.value()).context("decode value in delete_by_subject")?;
                if value.valid_to_ms.is_some() {
                    // Already closed — skip.
                    continue;
                }
                if let Some((s, p)) = decode_triple_key(k.value()) {
                    to_retract.push((s, p));
                }
            }
        }
        let mut closed = 0usize;
        for (s, p) in &to_retract {
            match self.retract(s, p) {
                Ok(n) => closed += n,
                Err(e) => {
                    tracing::warn!(subject = %s, predicate = %p, "delete_by_subject: retract failed: {e:#}");
                }
            }
        }
        Ok(closed)
    }

    /// Load all drawers from the table.
    ///
    /// Why: Cold-start retrieval needs the full drawer table to map every HNSW
    /// vector hit back to metadata.
    /// What: Iterate DRAWERS, decode each `DrawerRecord` back into a `Drawer`.
    /// Rows with malformed UUID/timestamp are skipped with a warning.
    /// Test: `upsert_drawer_then_load_drawers_round_trips`.
    pub fn load_drawers(&self) -> Result<Vec<Drawer>> {
        let rtx = self.db().begin_read().context("begin load_drawers txn")?;
        let drawers = rtx.open_table(DRAWERS).context("open drawers table")?;
        let mut out = Vec::new();
        for entry in drawers.iter().context("iter drawers")? {
            let (k, v) = entry.context("read drawer row")?;
            let id_bytes = k.value();
            if id_bytes.len() != 16 {
                tracing::warn!(len = id_bytes.len(), "skip drawer with non-16-byte id key");
                continue;
            }
            let mut id_arr = [0u8; 16];
            id_arr.copy_from_slice(id_bytes);
            let id = Uuid::from_bytes(id_arr);
            let record: DrawerRecord = match decode_value::<DrawerRecord>(v.value()) {
                Ok(r) => r,
                Err(_) => {
                    // Issue #61: rows written before drawer_type /
                    // expires_at_ms existed lack those trailing fields and
                    // postcard refuses to decode them as the new struct.
                    // Fall back to the legacy shape and lift it forward.
                    match decode_value::<LegacyDrawerRecord>(v.value()) {
                        Ok(legacy) => legacy.into(),
                        Err(e) => {
                            tracing::warn!(id = %id, "skip drawer with malformed value: {e}");
                            continue;
                        }
                    }
                }
            };
            let room_id = match Uuid::parse_str(&record.room_id) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(id = %id, "skip drawer with invalid room_id: {e}");
                    continue;
                }
            };
            let created_at = match DateTime::from_timestamp_millis(record.created_at_ms) {
                Some(dt) => dt,
                None => {
                    tracing::warn!(id = %id, "skip drawer with invalid created_at_ms");
                    continue;
                }
            };
            let drawer_type = parse_drawer_type(record.drawer_type.as_deref());
            let expires_at = record
                .expires_at_ms
                .and_then(DateTime::from_timestamp_millis);
            out.push(Drawer {
                id,
                room_id,
                content: record.content,
                importance: record.importance,
                source_file: record.source_file.map(PathBuf::from),
                created_at,
                tags: record.tags,
                last_accessed_at: None,
                access_count: 0,
                drawer_type,
                expires_at,
            });
        }
        Ok(out)
    }

    /// Load just the set of drawer IDs.
    ///
    /// Why: Compaction only needs "is this UUID a live drawer?"; this avoids
    /// the cost of materializing `Drawer` rows.
    /// What: Iterate DRAWERS keys, parse each 16-byte slice into a `Uuid`,
    /// collect into a `HashSet`.
    /// Test: `load_drawer_ids_matches_load_drawers`.
    pub fn load_drawer_ids(&self) -> Result<HashSet<Uuid>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin load_drawer_ids txn")?;
        let drawers = rtx.open_table(DRAWERS).context("open drawers table")?;
        let mut out = HashSet::new();
        for entry in drawers.iter().context("iter drawers")? {
            let (k, _) = entry.context("read drawer row")?;
            let id_bytes = k.value();
            if id_bytes.len() != 16 {
                continue;
            }
            let mut id_arr = [0u8; 16];
            id_arr.copy_from_slice(id_bytes);
            out.insert(Uuid::from_bytes(id_arr));
        }
        Ok(out)
    }

    /// Import a batch of historical triples and drawers without disturbing
    /// existing rows.
    ///
    /// Why: Issue #45's one-shot SQLite → redb migration needs to write every
    /// legacy row verbatim — both active and closed intervals — without the
    /// "close prior active" semantics that `assert` enforces. The rows
    /// originated from a temporal store and already carry their final
    /// `valid_from` / `valid_to`; we must preserve that history.
    /// What: In a single write transaction, write each triple at either its
    /// primary `(subject, predicate)` key (when active) or a `hist:`-prefixed
    /// key (when closed). Secondary indexes and `ACTIVE_SUBJECT_COUNTS` are
    /// updated only for active rows. Drawers are upserted as-is. If multiple
    /// triples share `(subject, predicate)`, the last active one wins at the
    /// primary key — earlier active rows of the same pair are stored under
    /// `hist:` keys instead so no data is silently dropped.
    /// Test: Covered by the kg_migration integration test in
    /// `crates/trusty-common/tests/kg_migration_tests.rs`.
    pub fn import_all(&self, triples: Vec<Triple>, drawers: Vec<Drawer>) -> Result<()> {
        self.check_writable()?;
        let wtx = self.db().begin_write().context("begin import txn")?;
        {
            let mut triples_t = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;

            for triple in &triples {
                let value = TripleValue {
                    object: triple.object.clone(),
                    valid_from_ms: triple.valid_from.timestamp_millis(),
                    valid_to_ms: triple.valid_to.map(|dt| dt.timestamp_millis()),
                    confidence: triple.confidence,
                    provenance: triple.provenance.clone(),
                };
                let key = encode_triple_key(&triple.subject, &triple.predicate);
                let bytes = encode_value(&value).context("encode triple value")?;

                if value.valid_to_ms.is_some() {
                    // Closed interval: store under history key, indexed by its
                    // own valid_from_ms so multiple closed rows for the same
                    // (subject, predicate) can coexist.
                    let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                    hist_key.extend_from_slice(b"hist:");
                    hist_key.extend_from_slice(&key);
                    hist_key.extend_from_slice(&value.valid_from_ms.to_be_bytes());
                    triples_t
                        .insert(hist_key.as_slice(), bytes.as_slice())
                        .context("insert closed history row")?;
                } else {
                    // Active interval: if the primary slot is already taken by
                    // a previously-imported active row, demote that row to
                    // history first so we never silently overwrite an active
                    // fact during migration.
                    let prior_opt: Option<TripleValue> = {
                        let existing = triples_t
                            .get(key.as_slice())
                            .context("read existing triple during import")?;
                        match existing {
                            Some(g) => Some(
                                decode_value(g.value()).context("decode prior during import")?,
                            ),
                            None => None,
                        }
                    };
                    #[allow(clippy::collapsible_if)]
                    if let Some(prior) = prior_opt {
                        if prior.valid_to_ms.is_none() {
                            // Demote existing active row to history (closed at
                            // the new row's valid_from), drop its indexes /
                            // counter so the new row owns them.
                            let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                            hist_key.extend_from_slice(b"hist:");
                            hist_key.extend_from_slice(&key);
                            hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
                            let closed = TripleValue {
                                valid_to_ms: Some(value.valid_from_ms),
                                ..prior.clone()
                            };
                            let closed_bytes = encode_value(&closed)
                                .context("encode demoted active row during import")?;
                            triples_t
                                .insert(hist_key.as_slice(), closed_bytes.as_slice())
                                .context("insert demoted history row")?;

                            let obj_key = encode_object_index_key(
                                &prior.object,
                                &triple.subject,
                                &triple.predicate,
                            );
                            by_object
                                .remove(obj_key.as_slice())
                                .context("remove demoted object index")?;
                            let pred_key =
                                encode_predicate_index_key(&triple.predicate, &triple.subject);
                            by_predicate
                                .remove(pred_key.as_slice())
                                .context("remove demoted predicate index")?;

                            let subj_key = triple.subject.as_bytes();
                            let prev = counts
                                .get(subj_key)
                                .context("read prior count during import")?
                                .map(|v| decode_u64(v.value()))
                                .unwrap_or(0);
                            let next = prev.saturating_sub(1);
                            if next == 0 {
                                counts.remove(subj_key).context("remove zero count")?;
                            } else {
                                counts
                                    .insert(subj_key, encode_u64(next).as_slice())
                                    .context("update count after demote")?;
                            }
                        }
                    }

                    triples_t
                        .insert(key.as_slice(), bytes.as_slice())
                        .context("insert imported active triple")?;

                    let obj_key =
                        encode_object_index_key(&value.object, &triple.subject, &triple.predicate);
                    by_object
                        .insert(obj_key.as_slice(), [].as_slice())
                        .context("insert object index for imported row")?;
                    let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
                    by_predicate
                        .insert(pred_key.as_slice(), [].as_slice())
                        .context("insert predicate index for imported row")?;

                    let subj_key = triple.subject.as_bytes();
                    let prev = counts
                        .get(subj_key)
                        .context("read count for imported subject")?
                        .map(|v| decode_u64(v.value()))
                        .unwrap_or(0);
                    let next = prev.saturating_add(1);
                    counts
                        .insert(subj_key, encode_u64(next).as_slice())
                        .context("update count for imported subject")?;
                }
            }

            // Drawers — straight upsert. UUID keys collide cleanly on duplicates.
            let mut drawers_t = wtx.open_table(DRAWERS).context("open drawers table")?;
            for drawer in &drawers {
                let record = drawer_to_record(drawer);
                let bytes = encode_value(&record).context("encode drawer for import")?;
                let id_bytes = *drawer.id.as_bytes();
                drawers_t
                    .insert(id_bytes.as_slice(), bytes.as_slice())
                    .context("insert imported drawer")?;
            }
        }
        wtx.commit().context("commit import txn")?;
        Ok(())
    }

    /// Apply a batch of write ops inside a single redb write transaction.
    ///
    /// Why: Issue #59 follow-up — bulk `assert` / `retract` / drawer
    /// upsert workloads otherwise pay one `begin_write` + one fsync per op.
    /// Coalescing N ops into a single transaction collapses N fsyncs into
    /// one, which is the dominant cost on durable writes. The batch
    /// preserves per-op semantics: each `Assert` still closes any prior
    /// active interval, each `Retract` still moves rows to history, each
    /// drawer op still mutates the DRAWERS table. The only behavioural
    /// difference from calling each op individually is atomicity — if one
    /// op fails, the whole batch is rolled back (caller decides via the
    /// returned error whether to retry individually).
    /// What: Opens one write transaction, applies each `BatchWriteOp` by
    /// delegating to a free-function helper that takes already-opened
    /// tables, and commits once. On any per-op error the transaction is
    /// aborted and the error is returned together with the index of the
    /// failing op so callers can log it.
    /// Test: `apply_batch_groups_asserts_into_single_commit` and
    /// `apply_batch_rolls_back_on_error` in this module.
    pub fn apply_batch(&self, ops: &[BatchWriteOp]) -> Result<Vec<BatchOpResult>> {
        self.check_writable()?;
        if ops.is_empty() {
            return Ok(Vec::new());
        }

        let wtx = self.db().begin_write().context("begin batch txn")?;
        let mut results: Vec<BatchOpResult> = Vec::with_capacity(ops.len());
        {
            let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;
            let mut drawers_t = wtx.open_table(DRAWERS).context("open drawers table")?;

            for (idx, op) in ops.iter().enumerate() {
                let res: Result<BatchOpResult> = match op {
                    BatchWriteOp::Assert(triple) => batch_assert(
                        &mut triples,
                        &mut by_object,
                        &mut by_predicate,
                        &mut counts,
                        triple,
                    )
                    .map(|_| BatchOpResult::Asserted),
                    BatchWriteOp::Retract { subject, predicate } => batch_retract(
                        &mut triples,
                        &mut by_object,
                        &mut by_predicate,
                        &mut counts,
                        subject,
                        predicate,
                    )
                    .map(BatchOpResult::Retracted),
                    BatchWriteOp::UpsertDrawer(drawer) => {
                        batch_upsert_drawer(&mut drawers_t, drawer)
                            .map(|_| BatchOpResult::DrawerUpserted)
                    }
                    BatchWriteOp::DeleteDrawer(id) => batch_delete_drawer(&mut drawers_t, *id)
                        .map(|_| BatchOpResult::DrawerDeleted),
                };
                match res {
                    Ok(r) => results.push(r),
                    Err(e) => {
                        return Err(
                            e.context(format!("batch op #{idx} failed; transaction rolled back"))
                        );
                    }
                }
            }
        }
        wtx.commit().context("commit batch txn")?;
        Ok(results)
    }

    /// Dump every triple, including closed history rows.
    ///
    /// Why: The #45 migration path needs to walk the entire table to export
    /// data. Also useful for diagnostics.
    /// What: Scan the TRIPLES table end-to-end, returning both active rows and
    /// `hist:` rows decoded as `Triple` (so `valid_to.is_some()` for history).
    /// Test: `assert_supersedes_prior` checks history is preserved.
    pub fn dump_all_triples(&self) -> Result<Vec<Triple>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin dump_all_triples txn")?;
        let triples = rtx
            .open_table(TRIPLES)
            .context("open triples table for dump_all_triples")?;
        let mut out = Vec::new();
        for entry in triples.iter().context("iter triples for dump")? {
            let (k, v) = entry.context("read triples row for dump")?;
            let key_bytes = k.value();
            let value: TripleValue = match decode_value(v.value()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("skip undecodable triple value in dump: {e}");
                    continue;
                }
            };
            let (s, p) = if let Some(stripped) = key_bytes.strip_prefix(b"hist:") {
                // History key = `hist:` + original encoded key + 8 byte suffix.
                if stripped.len() < 8 {
                    continue;
                }
                let core = &stripped[..stripped.len() - 8];
                match decode_triple_key(core) {
                    Some(parts) => parts,
                    None => continue,
                }
            } else {
                match decode_triple_key(key_bytes) {
                    Some(parts) => parts,
                    None => continue,
                }
            };
            out.push(triple_from_parts(s, p, value)?);
        }
        Ok(out)
    }
}

/// A single write op that can be queued through `apply_batch`.
///
/// Why: The write coalescer in `kg_writer.rs` accepts ops from concurrent
/// callers, then replays them inside a single redb transaction. Modelling
/// the op shape explicitly keeps the writer task backend-agnostic and
/// makes `apply_batch` directly unit-testable.
/// What: Mirrors the four mutating entry points on `KgStoreRedb` —
/// `assert`, `retract`, `upsert_drawer`, `delete_drawer`. All variants
/// own their data so an op can cross an `mpsc` channel.
/// Test: `apply_batch_groups_asserts_into_single_commit` exercises the
/// `Assert` variant; the writer tests cover the others.
#[derive(Debug, Clone)]
pub enum BatchWriteOp {
    /// Assert a triple; closes any prior active interval.
    Assert(Triple),
    /// Close the active triple for `(subject, predicate)` without
    /// inserting a replacement.
    Retract { subject: String, predicate: String },
    /// Persist a drawer row.
    UpsertDrawer(Drawer),
    /// Remove a drawer row by UUID.
    DeleteDrawer(Uuid),
}

/// Per-op outcome returned from `apply_batch`.
///
/// Why: Callers awaiting a queued op need typed results — in particular
/// `Retract` returns 0/1 for "rows closed" which the writer task forwards
/// back through a `oneshot::Sender<Result<usize>>`.
/// What: Enum carrying the same return shape each single-op method
/// already exposes (`assert` → unit, `retract` → usize, drawer ops →
/// unit).
/// Test: Indirect via `apply_batch_*` tests and the writer tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOpResult {
    Asserted,
    Retracted(usize),
    DrawerUpserted,
    DrawerDeleted,
}

// ----- in-transaction helpers shared by the single-op and batch paths -----
//
// Why: The single-op `assert` / `retract` / drawer methods already
// implement the correct semantics inside their own `begin_write` block.
// To share that logic with `apply_batch` without duplicating it, we lift
// the per-op body into a free function that takes already-opened tables.
// This keeps the txn boundary explicit (one `begin_write` per batch) and
// avoids logic drift between the two paths. The single-op methods could
// be migrated to call these helpers in a follow-up; for now we accept
// the duplication to keep the diff minimal.

type Tbl<'txn> = redb::Table<'txn, &'static [u8], &'static [u8]>;

/// In-transaction assert helper; mirrors `KgStoreRedb::assert`.
///
/// Why: Lets `apply_batch` perform N asserts inside one write txn.
/// What: Same close-prior + insert-new + index-maintenance logic that
/// the single-op `assert` runs, but takes already-opened tables.
/// Test: `apply_batch_groups_asserts_into_single_commit`.
fn batch_assert(
    triples: &mut Tbl<'_>,
    by_object: &mut Tbl<'_>,
    by_predicate: &mut Tbl<'_>,
    counts: &mut Tbl<'_>,
    triple: &Triple,
) -> Result<()> {
    let close_ms = triple.valid_from.timestamp_millis();
    let new_value = TripleValue {
        object: triple.object.clone(),
        valid_from_ms: triple.valid_from.timestamp_millis(),
        valid_to_ms: triple.valid_to.map(|dt| dt.timestamp_millis()),
        confidence: triple.confidence,
        provenance: triple.provenance.clone(),
    };
    let key = encode_triple_key(&triple.subject, &triple.predicate);

    let mut closed_any = false;
    let prior_opt: Option<TripleValue> = {
        let existing = triples
            .get(key.as_slice())
            .context("read existing triple (batch)")?;
        match existing {
            Some(g) => Some(decode_value(g.value()).context("decode prior triple (batch)")?),
            None => None,
        }
    };
    if let Some(prior) = prior_opt
        && prior.valid_to_ms.is_none()
    {
        let obj_key = encode_object_index_key(&prior.object, &triple.subject, &triple.predicate);
        by_object
            .remove(obj_key.as_slice())
            .context("remove prior object index (batch)")?;
        let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
        by_predicate
            .remove(pred_key.as_slice())
            .context("remove prior predicate index (batch)")?;
        closed_any = true;

        let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
        hist_key.extend_from_slice(b"hist:");
        hist_key.extend_from_slice(&key);
        hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
        let closed = TripleValue {
            valid_to_ms: Some(close_ms),
            ..prior
        };
        let closed_bytes = encode_value(&closed).context("encode closed prior (batch)")?;
        triples
            .insert(hist_key.as_slice(), closed_bytes.as_slice())
            .context("insert closed history row (batch)")?;
    }

    let new_bytes = encode_value(&new_value).context("encode new triple (batch)")?;
    triples
        .insert(key.as_slice(), new_bytes.as_slice())
        .context("insert new triple (batch)")?;

    if new_value.valid_to_ms.is_none() {
        let obj_key =
            encode_object_index_key(&new_value.object, &triple.subject, &triple.predicate);
        by_object
            .insert(obj_key.as_slice(), [].as_slice())
            .context("insert new object index (batch)")?;
        let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
        by_predicate
            .insert(pred_key.as_slice(), [].as_slice())
            .context("insert new predicate index (batch)")?;
        if !closed_any {
            let subj_key = triple.subject.as_bytes();
            let prev = counts
                .get(subj_key)
                .context("read prior count (batch)")?
                .map(|v| decode_u64(v.value()))
                .unwrap_or(0);
            let next = prev.saturating_add(1);
            counts
                .insert(subj_key, encode_u64(next).as_slice())
                .context("update active count (batch)")?;
        }
    } else if closed_any {
        let subj_key = triple.subject.as_bytes();
        let prev = counts
            .get(subj_key)
            .context("read prior count for closed-on-arrival (batch)")?
            .map(|v| decode_u64(v.value()))
            .unwrap_or(0);
        let next = prev.saturating_sub(1);
        if next == 0 {
            counts
                .remove(subj_key)
                .context("remove zero count (batch)")?;
        } else {
            counts
                .insert(subj_key, encode_u64(next).as_slice())
                .context("update active count (batch)")?;
        }
    }
    Ok(())
}

/// In-transaction retract helper; mirrors `KgStoreRedb::retract`.
fn batch_retract(
    triples: &mut Tbl<'_>,
    by_object: &mut Tbl<'_>,
    by_predicate: &mut Tbl<'_>,
    counts: &mut Tbl<'_>,
    subject: &str,
    predicate: &str,
) -> Result<usize> {
    let key = encode_triple_key(subject, predicate);
    let close_ms = now_ms();
    let prior_opt: Option<TripleValue> = {
        let existing = triples
            .get(key.as_slice())
            .context("lookup active triple for retract (batch)")?;
        match existing {
            Some(g) => Some(decode_value(g.value()).context("decode prior for retract (batch)")?),
            None => None,
        }
    };
    let Some(prior) = prior_opt else { return Ok(0) };
    if prior.valid_to_ms.is_some() {
        return Ok(0);
    }

    let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
    hist_key.extend_from_slice(b"hist:");
    hist_key.extend_from_slice(&key);
    hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
    let closed_v = TripleValue {
        valid_to_ms: Some(close_ms),
        ..prior.clone()
    };
    let bytes = encode_value(&closed_v).context("encode retract history (batch)")?;
    triples
        .insert(hist_key.as_slice(), bytes.as_slice())
        .context("insert retract history row (batch)")?;
    triples
        .remove(key.as_slice())
        .context("remove active row for retract (batch)")?;
    let obj_key = encode_object_index_key(&prior.object, subject, predicate);
    by_object
        .remove(obj_key.as_slice())
        .context("remove object index for retract (batch)")?;
    let pred_key = encode_predicate_index_key(predicate, subject);
    by_predicate
        .remove(pred_key.as_slice())
        .context("remove predicate index for retract (batch)")?;
    let subj_key = subject.as_bytes();
    let prev = counts
        .get(subj_key)
        .context("read prior count for retract (batch)")?
        .map(|v| decode_u64(v.value()))
        .unwrap_or(0);
    let next = prev.saturating_sub(1);
    if next == 0 {
        counts
            .remove(subj_key)
            .context("remove zero count (batch)")?;
    } else {
        counts
            .insert(subj_key, encode_u64(next).as_slice())
            .context("update count after retract (batch)")?;
    }
    Ok(1)
}

/// In-transaction drawer upsert helper.
fn batch_upsert_drawer(drawers: &mut Tbl<'_>, drawer: &Drawer) -> Result<()> {
    let record = drawer_to_record(drawer);
    let bytes = encode_value(&record).context("encode drawer record (batch)")?;
    let id_bytes = *drawer.id.as_bytes();
    drawers
        .insert(id_bytes.as_slice(), bytes.as_slice())
        .context("insert drawer record (batch)")?;
    Ok(())
}

/// In-transaction drawer delete helper.
fn batch_delete_drawer(drawers: &mut Tbl<'_>, id: Uuid) -> Result<()> {
    let id_bytes = *id.as_bytes();
    drawers
        .remove(id_bytes.as_slice())
        .context("remove drawer record (batch)")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_kg() -> (tempfile::TempDir, KgStoreRedb) {
        let dir = tempdir().unwrap();
        let kg = KgStoreRedb::open(&dir.path().join("kg.redb")).unwrap();
        (dir, kg)
    }

    fn t(subject: &str, predicate: &str, object: &str) -> Triple {
        Triple {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        }
    }

    #[test]
    fn open_then_reopen_persists_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        {
            let kg = KgStoreRedb::open(&path).unwrap();
            kg.assert(&t("alice", "knows", "bob")).unwrap();
        }
        let kg = KgStoreRedb::open(&path).unwrap();
        let active = kg.query_active("alice").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].object, "bob");
    }

    #[test]
    fn assert_then_query_returns_triple() {
        let (_d, kg) = open_kg();
        kg.assert(&t("alice", "works_at", "Acme Corp")).unwrap();
        let active = kg.query_active("alice").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].object, "Acme Corp");
    }

    #[test]
    fn assert_supersedes_prior() {
        let (_d, kg) = open_kg();
        kg.assert(&t("alice", "works_at", "Acme")).unwrap();
        kg.assert(&t("alice", "works_at", "Beta")).unwrap();
        let active = kg.query_active("alice").unwrap();
        assert_eq!(active.len(), 1, "exactly one active row");
        assert_eq!(active[0].object, "Beta");

        // dump_all should include both — history + current.
        let all = kg.dump_all_triples().unwrap();
        assert_eq!(all.len(), 2);
        let objects: Vec<_> = all.iter().map(|x| x.object.as_str()).collect();
        assert!(objects.contains(&"Acme"));
        assert!(objects.contains(&"Beta"));
    }

    #[test]
    fn retract_closes_active_interval() {
        let (_d, kg) = open_kg();
        kg.assert(&t("tga", "is_alias_for", "trusty-git-analytics"))
            .unwrap();
        assert_eq!(kg.query_active("tga").unwrap().len(), 1);

        let closed = kg.retract("tga", "is_alias_for").unwrap();
        assert_eq!(closed, 1);
        assert!(kg.query_active("tga").unwrap().is_empty());

        // Second retract no-op.
        let again = kg.retract("tga", "is_alias_for").unwrap();
        assert_eq!(again, 0);

        // History row preserved.
        let all = kg.dump_all_triples().unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].valid_to.is_some());
    }

    #[test]
    fn list_subjects_returns_distinct_active_subjects() {
        let (_d, kg) = open_kg();
        assert!(kg.list_subjects(50).unwrap().is_empty());

        kg.assert(&t("bob", "knows", "alice")).unwrap();
        kg.assert(&t("alice", "knows", "bob")).unwrap();
        kg.assert(&t("alice", "knows", "carol")).unwrap(); // supersedes prior

        let subjects = kg.list_subjects(50).unwrap();
        assert_eq!(subjects, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn list_subjects_with_counts_returns_grouped_counts() {
        let (_d, kg) = open_kg();
        assert!(kg.list_subjects_with_counts(50).unwrap().is_empty());

        for (subj, pred) in [
            ("alice", "knows"),
            ("alice", "likes"),
            ("alice", "owns"),
            ("bob", "knows"),
        ] {
            kg.assert(&t(subj, pred, "thing")).unwrap();
        }

        let rows = kg.list_subjects_with_counts(50).unwrap();
        assert_eq!(rows, vec![("alice".to_string(), 3), ("bob".to_string(), 1)]);
    }

    #[test]
    fn count_active_triples_returns_live_only() {
        let (_d, kg) = open_kg();
        assert_eq!(kg.count_active_triples(), 0);

        kg.assert(&t("alice", "works_at", "Acme")).unwrap();
        assert_eq!(kg.count_active_triples(), 1);

        kg.assert(&t("alice", "works_at", "Beta")).unwrap();
        assert_eq!(kg.count_active_triples(), 1);

        kg.assert(&t("bob", "works_at", "Gamma")).unwrap();
        assert_eq!(kg.count_active_triples(), 2);

        kg.retract("alice", "works_at").unwrap();
        assert_eq!(kg.count_active_triples(), 1);
    }

    #[test]
    fn list_active_returns_ordered_window() {
        let (_d, kg) = open_kg();
        for i in 0..3 {
            kg.assert(&Triple {
                subject: format!("subj-{i}"),
                predicate: "rel".into(),
                object: format!("obj-{i}"),
                valid_from: Utc::now() + chrono::Duration::milliseconds(i * 10),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .unwrap();
        }

        let all = kg.list_active(10, 0).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].subject, "subj-2");
        assert_eq!(all[2].subject, "subj-0");

        let window = kg.list_active(2, 1).unwrap();
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].subject, "subj-1");
        assert_eq!(window[1].subject, "subj-0");
    }

    #[test]
    fn upsert_drawer_then_load_drawers_round_trips() {
        let (_d, kg) = open_kg();
        let room_id = Uuid::new_v4();
        let mut d = Drawer::new(room_id, "the cold-start drawer");
        d.importance = 0.83;
        d.tags = vec!["alpha".into(), "beta".into()];
        d.source_file = Some(PathBuf::from("/tmp/source.md"));
        kg.upsert_drawer(&d).unwrap();

        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, d.id);
        assert_eq!(loaded[0].room_id, room_id);
        assert_eq!(loaded[0].content, "the cold-start drawer");
        assert!((loaded[0].importance - 0.83).abs() < 1e-5);
        assert_eq!(loaded[0].tags, vec!["alpha".to_string(), "beta".into()]);
        assert_eq!(loaded[0].source_file, Some(PathBuf::from("/tmp/source.md")));
    }

    #[test]
    fn drawer_type_round_trips_through_redb() {
        // Issue #61: drawer_type + expires_at must survive a write/read.
        use crate::memory_core::palace::DrawerType;
        let (_d, kg) = open_kg();
        let room_id = Uuid::new_v4();
        let drawer =
            Drawer::new(room_id, "session event content").with_type(DrawerType::SessionEvent);
        kg.upsert_drawer(&drawer).unwrap();
        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].drawer_type, DrawerType::SessionEvent);
        assert!(
            loaded[0].expires_at.is_some(),
            "session events must carry a TTL"
        );
    }

    #[test]
    fn load_drawer_ids_matches_load_drawers() {
        let (_d, kg) = open_kg();
        let room = Uuid::new_v4();
        let d1 = Drawer::new(room, "one");
        let d2 = Drawer::new(room, "two");
        kg.upsert_drawer(&d1).unwrap();
        kg.upsert_drawer(&d2).unwrap();
        let ids = kg.load_drawer_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&d1.id));
        assert!(ids.contains(&d2.id));
    }

    #[test]
    fn delete_drawer_removes_row() {
        let (_d, kg) = open_kg();
        let d = Drawer::new(Uuid::new_v4(), "to be deleted");
        kg.upsert_drawer(&d).unwrap();
        kg.delete_drawer(d.id).unwrap();
        assert!(kg.load_drawers().unwrap().is_empty());
    }

    #[test]
    fn upsert_drawer_replaces_existing_row() {
        let (_d, kg) = open_kg();
        let mut d = Drawer::new(Uuid::new_v4(), "original");
        kg.upsert_drawer(&d).unwrap();
        d.content = "updated".into();
        d.importance = 0.95;
        kg.upsert_drawer(&d).unwrap();
        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "updated");
        assert!((loaded[0].importance - 0.95).abs() < 1e-5);
    }

    /// Why: Production opens the same palace from multiple registries (test
    /// setup + `AppState`, foreground + dreamer). redb forbids two `Database`
    /// handles to one file; the cache must hand back the live handle so
    /// concurrent opens of the same path succeed.
    #[test]
    fn multiple_handles_to_same_path_share_database() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let a = KgStoreRedb::open(&path).unwrap();
        let b = KgStoreRedb::open(&path).unwrap();
        // Writes through one are visible through the other.
        a.assert(&t("alice", "knows", "bob")).unwrap();
        let active = b.query_active("alice").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].object, "bob");
    }

    #[test]
    fn checkpoint_is_noop() {
        let (_d, kg) = open_kg();
        kg.checkpoint().unwrap();
        kg.checkpoint().unwrap();
    }

    /// Why: `apply_batch` is the heart of the write-coalescing path —
    /// asserting multiple triples in one transaction must produce the
    /// same end state as calling `assert` N times.
    /// What: Submits a 5-op batch (4 asserts + 1 retract) and verifies
    /// the active set matches the expected result.
    /// Test ID: apply_batch_groups_asserts_into_single_commit.
    #[test]
    fn apply_batch_groups_asserts_into_single_commit() {
        let (_d, kg) = open_kg();
        let ops = vec![
            BatchWriteOp::Assert(t("a", "p1", "v1")),
            BatchWriteOp::Assert(t("a", "p2", "v2")),
            BatchWriteOp::Assert(t("b", "p1", "v3")),
            BatchWriteOp::Assert(t("a", "p1", "v1b")), // supersedes a/p1
            BatchWriteOp::Retract {
                subject: "a".to_string(),
                predicate: "p2".to_string(),
            },
        ];
        let results = kg.apply_batch(&ops).unwrap();
        assert_eq!(results.len(), 5);
        assert!(matches!(results[0], BatchOpResult::Asserted));
        assert!(matches!(results[3], BatchOpResult::Asserted));
        assert_eq!(results[4], BatchOpResult::Retracted(1));

        // Active state: a/p1 = v1b (latest), a/p2 retracted, b/p1 = v3.
        let a_active = kg.query_active("a").unwrap();
        assert_eq!(a_active.len(), 1);
        assert_eq!(a_active[0].predicate, "p1");
        assert_eq!(a_active[0].object, "v1b");

        let b_active = kg.query_active("b").unwrap();
        assert_eq!(b_active.len(), 1);
        assert_eq!(b_active[0].object, "v3");
    }

    /// Why: Empty batches must be safe — the writer may flush a coalesce
    /// window with zero queued ops if the caller dropped its sender
    /// between recv and drain.
    /// What: `apply_batch(&[])` returns `Ok(vec![])` and does not open a
    /// transaction (so write-locks are not contended for nothing).
    /// Test ID: apply_batch_empty_is_noop.
    #[test]
    fn apply_batch_empty_is_noop() {
        let (_d, kg) = open_kg();
        let results = kg.apply_batch(&[]).unwrap();
        assert!(results.is_empty());
    }

    /// Why: Drawer upserts must coexist with triple ops in the same
    /// transaction so a `remember` + `kg_assert` burst can be coalesced.
    /// What: Mixed batch with a drawer and a triple; both visible after.
    /// Test ID: apply_batch_mixes_drawer_and_triple_ops.
    #[test]
    fn apply_batch_mixes_drawer_and_triple_ops() {
        use crate::memory_core::palace::Drawer;
        let (_d, kg) = open_kg();
        let drawer = Drawer::new(Uuid::new_v4(), "hello world".to_string());
        let drawer_id = drawer.id;
        let ops = vec![
            BatchWriteOp::UpsertDrawer(drawer),
            BatchWriteOp::Assert(t("alice", "wrote", "drawer-1")),
        ];
        let results = kg.apply_batch(&ops).unwrap();
        assert_eq!(results.len(), 2);
        assert!(matches!(results[0], BatchOpResult::DrawerUpserted));
        assert!(matches!(results[1], BatchOpResult::Asserted));

        let drawer_ids = kg.load_drawer_ids().unwrap();
        assert!(drawer_ids.contains(&drawer_id));
        assert_eq!(kg.query_active("alice").unwrap().len(), 1);
    }

    // -- Issue #59: read-only snapshot fallback ----------------------------

    /// Hold the live redb file with a direct `Database::create` (bypassing
    /// the in-process `db_cache`) so the next `KgStoreRedb::open` triggers
    /// the snapshot-mode fallback. The returned `Database` must be kept
    /// alive for the duration of the test so the file lock is held.
    ///
    /// Why: Centralises the lock-from-another-handle pattern used by every
    /// read-only test in this module.
    /// What: Creates a redb file at `path` via the raw `redb` API; the
    /// returned handle owns the exclusive flock.
    /// Test: Indirect — every snapshot test below.
    fn lock_redb_file(path: &Path) -> Database {
        Database::create(path).expect("first lock-holder open")
    }

    /// Why: Confirms the central invariant of issue #59 — when the redb
    /// file is locked by another handle, a fresh `KgStoreRedb::open` falls
    /// back to a snapshot and `is_read_only` reports true.
    /// What: Seeds a palace file, drops the seeding store so the cache
    /// entry expires, locks the file via raw `Database::create`, then
    /// opens `KgStoreRedb` and asserts the snapshot mode.
    /// Test: this test.
    #[test]
    fn open_on_locked_file_returns_read_only_handle() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        // Touch the file so it has the redb header.
        drop(KgStoreRedb::open(&path).unwrap());
        let _live = lock_redb_file(&path);

        let snap = KgStoreRedb::open(&path).expect("snapshot fallback");
        assert!(snap.is_read_only(), "snapshot must report read-only");
    }

    /// Why: Every write surface (`assert`, `retract`, drawer
    /// upsert/delete, `import_all`) must reject the operation when the
    /// store is in snapshot mode so the MCP / HTTP layer can surface a
    /// single, actionable error string.
    /// What: Seeds the file, drops the seeding store, locks the file,
    /// opens a snapshot store, then exercises every write entrypoint and
    /// asserts each returns an error whose message references the
    /// daemon-guidance.
    /// Test: this test.
    #[test]
    fn write_on_snapshot_returns_read_only_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        // Seed the live file with one row so retract has something to act on
        // when the snapshot is taken.
        {
            let primary = KgStoreRedb::open(&path).unwrap();
            primary.assert(&t("alice", "knows", "bob")).unwrap();
        }
        // Hold the live lock with a raw handle (bypasses the cache).
        let _live = lock_redb_file(&path);

        let snap = KgStoreRedb::open(&path).expect("snapshot fallback");
        assert!(snap.is_read_only());

        assert!(
            snap.assert(&t("carol", "knows", "dave")).is_err(),
            "assert must fail in snapshot mode"
        );
        assert!(
            snap.retract("alice", "knows").is_err(),
            "retract must fail in snapshot mode"
        );
        let drawer = Drawer::new(Uuid::new_v4(), "x");
        assert!(
            snap.upsert_drawer(&drawer).is_err(),
            "upsert_drawer must fail in snapshot mode"
        );
        assert!(
            snap.delete_drawer(drawer.id).is_err(),
            "delete_drawer must fail in snapshot mode"
        );
        assert!(
            snap.import_all(Vec::new(), Vec::new()).is_err(),
            "import_all must fail in snapshot mode"
        );

        // Sentinel substring check — keeps the test resilient to wording
        // tweaks while still pinning the operator-facing guidance.
        let msg = format!("{:#}", snap.assert(&t("e", "f", "g")).unwrap_err());
        assert!(
            msg.contains("read-only"),
            "expected read-only sentinel in error, got: {msg}"
        );
        assert!(
            msg.contains("daemon"),
            "expected daemon-guidance in error, got: {msg}"
        );
    }

    /// Why: Reads must continue to work against the snapshot copy so the
    /// stdio MCP client can serve `query_active`, `list_subjects`,
    /// `load_drawers`, and `count_active_triples` while the daemon owns
    /// the live file.
    /// What: Seeds the live file with one triple and one drawer, drops the
    /// seeding store, locks the file, then opens a snapshot and asserts
    /// every read surface returns the seeded data.
    /// Test: this test.
    #[test]
    fn reads_on_snapshot_succeed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let drawer_id = {
            let primary = KgStoreRedb::open(&path).unwrap();
            primary.assert(&t("alice", "works_at", "Acme")).unwrap();
            let mut d = Drawer::new(Uuid::new_v4(), "snapshot drawer");
            d.importance = 0.7;
            primary.upsert_drawer(&d).unwrap();
            d.id
        };
        let _live = lock_redb_file(&path);

        let snap = KgStoreRedb::open(&path).expect("snapshot fallback");
        let triples = snap.query_active("alice").unwrap();
        assert_eq!(triples.len(), 1, "snapshot must surface seeded triple");
        assert_eq!(triples[0].object, "Acme");

        let subjects = snap.list_subjects(10).unwrap();
        assert!(subjects.contains(&"alice".to_string()));

        let drawers = snap.load_drawers().unwrap();
        assert_eq!(drawers.len(), 1);
        assert_eq!(drawers[0].id, drawer_id);
        assert_eq!(drawers[0].content, "snapshot drawer");

        assert_eq!(snap.count_active_triples(), 1);
    }

    /// Why: Cached in-process handles to the same canonical path must be
    /// usable concurrently — multiple tasks holding cloned `KgStoreRedb`
    /// handles must each be able to issue reads simultaneously without
    /// blocking each other. Validates the cache + `Arc<KgDbState>`
    /// sharing.
    /// What: Opens the same path three times in the same process (all
    /// served from the cache), then issues `query_active` concurrently
    /// on three threads. All three must succeed and observe the same row.
    /// Test: this test.
    #[test]
    fn concurrent_readers_share_cached_state() {
        use std::thread;

        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let primary = KgStoreRedb::open(&path).unwrap();
        primary.assert(&t("alice", "knows", "bob")).unwrap();

        let a = KgStoreRedb::open(&path).unwrap();
        let b = KgStoreRedb::open(&path).unwrap();
        let c = KgStoreRedb::open(&path).unwrap();

        let handles: Vec<_> = [a, b, c]
            .into_iter()
            .map(|store| {
                thread::spawn(move || {
                    let active = store.query_active("alice").unwrap();
                    assert_eq!(active.len(), 1);
                    assert_eq!(active[0].object, "bob");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("reader thread panicked");
        }
    }
}
