//! Why: The knowledge graph is migrating from SQLite to redb for embedded
//!      ACID storage without the r2d2/rusqlite dependency chain.
//! What: Table definitions, composite key encoding, and postcard value
//!       serialization for the redb-backed KG.
//! Test: Unit tests for encode/decode round-trips in this module.

use redb::TableDefinition;
use serde::{Deserialize, Serialize};

// ── Table definitions ────────────────────────────────────────────────────

/// Primary triple store.
///
/// Why: Composite key encoding allows efficient range scans by subject prefix
///      while preserving Ord semantics for redb's BTreeMap-backed tables. The
///      object is PART of the key (#4810): keying on `(subject, predicate)`
///      alone meant `room:General --contains--> drawer:X` held one row no
///      matter how many drawers the room contained, and every further member
///      silently closed its predecessor.
/// What: Key = `[subject_len: u16 BE][subject][predicate_len: u16 BE]
///       [predicate][object bytes]`. Value = postcard-encoded [`TripleValue`],
///       which still carries the object — the key copy exists to make rows
///       distinct, the value stays the authoritative read.
/// Test: See `round_trip_triple_key`, `subject_prefix_range_simulation`, and
///       `subject_predicate_prefix_excludes_a_longer_predicate`.
pub const TRIPLES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("triples");

/// Reverse index: object → (subject+predicate key) for O(degree) reverse lookup.
///
/// Why: Without this, finding "who points at X" requires a full scan of TRIPLES.
/// What: Key = `[object_len: u16 BE][object bytes][subject_len: u16 BE][subject bytes][predicate bytes]`.
///       Value = empty `&[u8]`.
/// Test: Range-scan simulation in `object_index_key_orders_by_object`.
pub const TRIPLES_BY_OBJECT: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("triples_by_object");

/// Predicate index for queries like "all triples with predicate P".
///
/// Why: Predicate-first range scans (e.g. all `created_by` edges). It carries
///      the object as of #4810 for the same reason [`TRIPLES`] does: without
///      it, two active rows sharing `(subject, predicate)` produce ONE index
///      entry, and retracting either would evict the other's. No reader
///      consumes this table today, which is exactly why the fix belongs
///      here — a known-broken index is not something to ship forward.
/// What: Key = `[predicate_len: u16 BE][predicate][subject_len: u16 BE]
///       [subject][object bytes]`. Value = empty `&[u8]`.
/// Test: Range-scan simulation in `predicate_index_key_orders_by_predicate`,
///       plus `predicate_index_key_separates_objects`.
pub const TRIPLES_BY_PREDICATE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("triples_by_predicate");

/// Active subject count — maintained for O(1) `count_active_triples`.
///
/// Why: Computing the active triple count for a subject on demand requires a
///      range scan; we maintain it incrementally for cheap reads.
/// What: Key = subject str (UTF-8 bytes — the entire key is the subject, no
///       length prefix needed since there is only one component).
///       Value = `u64` LE (count of active triples for this subject).
/// Test: `round_trip_u64`.
pub const ACTIVE_SUBJECT_COUNTS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("active_subject_counts");

/// Drawer metadata.
///
/// Why: Drawers are addressable by UUID; keep them in a separate table so
///      drawer listing does not interleave with triple range scans.
/// What: Key = uuid bytes (`[u8; 16]`).
///       Value = postcard-encoded [`DrawerRecord`].
/// Test: `round_trip_drawer_record`.
pub const DRAWERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("drawers");

/// Slot index over [`DRAWERS`]: `fact_key` → the drawer currently occupying
/// that slot (#4884, ADR-0028 D5).
///
/// Why: a Tier C fact occupies a named slot, and writing `pr:4818/state`
/// retires whatever already held it. The write path therefore has to answer
/// "does this slot have a live occupant?" on every Tier C write, and without
/// an index that answer costs a full `DRAWERS` scan. The precedent is
/// [`TRIPLES_BY_OBJECT`] / [`TRIPLES_BY_PREDICATE`]: a secondary table
/// maintained inside the same write transaction that mutates the primary row,
/// so no reader can observe the index disagreeing with `DRAWERS`. The same
/// discipline applies on delete — an entry pointing at a removed drawer would
/// make the occupancy check report a slot as taken when it is free.
/// What: Key = the `fact_key` UTF-8 bytes. The whole key is the fact_key: a
/// slot holds at most one drawer, so unlike the triple indexes there is no
/// second component and no length prefix to add. Value = the occupant's
/// drawer uuid bytes (`[u8; 16]`).
/// Test: `fact_key_index_tracks_upsert_and_delete`,
/// `fact_key_index_follows_the_slot_on_reassignment`,
/// `clearing_a_fact_key_drops_the_index_entry`,
/// `deleting_a_drawer_that_lost_its_slot_leaves_the_new_owner_indexed`.
pub const DRAWERS_BY_FACT_KEY: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("drawers_by_fact_key");

/// Room registry (ADR-0027 D1.1).
///
/// Why: rooms are stored in the SAME database as the drawers they index — not
/// a JSON sidecar — because the redb open path recreates an unreadable file
/// empty, and a surviving sidecar would then authoritatively describe rooms
/// whose drawers are gone. Rooms and drawers corrupt, snapshot, and recover as
/// one unit.
/// What: Key = room uuid bytes (`[u8; 16]`); the nil uuid is reserved for the
/// `RoomSchemaMarker` row. Value = postcard-encoded
/// [`crate::memory_core::store::rooms::RoomRecord`].
/// Test: `store::room_backfill::tests::room_insert_is_insert_only`.
pub const ROOMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("rooms");

/// Canonical-key index over [`ROOMS`] (ADR-0027 D1.3).
///
/// Why: a write names a room by label, not by id. Lowercasing the key while
/// the record keeps the first-seen spelling means `Decisions` and `decisions`
/// resolve to one room. Ids are READ from here, never recomputed.
/// What: Key = `"<wing_uuid>\x1f<normalized_label>"`. Value = room uuid bytes.
/// Test: `store::room_backfill::tests::room_key_lookup_returns_registered_id`.
pub const ROOM_KEYS: TableDefinition<&str, &[u8]> = TableDefinition::new("room_keys");

/// Wing registry (ADR-0027 D2 / ticket T9).
///
/// Why: a Wing is the scope/ownership axis over rooms — the "who" to a Room's
/// "what". It lives in the same database as the rooms it scopes for the same
/// corruption-recovery reason [`ROOMS`] does: a surviving sidecar would
/// authoritatively describe wings whose rooms are gone.
/// What: Key = wing uuid bytes (`[u8; 16]`); the nil uuid is reserved for the
/// `WingSchemaMarker` row. Value = postcard-encoded
/// [`crate::memory_core::store::wings::WingRecord`].
/// Test: `store::wings::tests::default_wing_is_seeded_once`.
pub const WINGS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("wings");

/// Canonical-key index over [`WINGS`] (ADR-0027 T9).
///
/// Why: a caller names a wing by label, not by id, and `Engineer`/`engineer`
/// must be one wing. Ids are READ from here, never recomputed — which is also
/// what pins the default wing to the `DEFAULT_WING_ID` every room row already
/// carries instead of minting a second one.
/// What: Key = the trimmed, lowercased label. Value = wing uuid bytes.
/// Test: `store::wings::tests::wing_create_is_idempotent`.
pub const WING_KEYS: TableDefinition<&str, &[u8]> = TableDefinition::new("wing_keys");

/// Payload store (for `trusty-agents`'s `TrustyBackedMemoryStore`).
///
/// Why: Payloads are namespaced by segment and addressed by id; share the
///      same redb env as the KG so payload + KG ops can ride a single
///      transaction.
/// What: Key = `[segment_len: u16 BE][segment bytes][id bytes]`.
///       Value = postcard-encoded payload string.
/// Test: `round_trip_payload_key`.
pub const PAYLOADS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("payloads");

/// Recall analytics event log (hit/miss telemetry).
///
/// Why: Issue #57 — `RecallLog` was the last rusqlite/r2d2 consumer in the
///      Memory Palace stack. Migrating it onto redb removes the heavy native
///      SQLite dependency chain from the default build and lines analytics
///      storage up with the rest of the palace (KG, payloads).
/// What: Key = monotonic u64 event id (derived from `Utc::now()` epoch ms with
///       an in-process tiebreaker so concurrent inserts in the same ms remain
///       unique and sort by insertion order).
///       Value = postcard-encoded `RecallEvent`.
/// Test: Coverage lives in `analytics::tests` (round-trip + reopen).
pub const RECALL_LOG: TableDefinition<u64, &[u8]> = TableDefinition::new("recall_log");

/// Vector store: monotonic `u64` id → postcard-encoded `Vec<f32>`.
///
/// Why: Issue #50 — `HnswStore` (the pure-Rust `hnsw_rs` backend) persists
///      its raw vectors in redb so the in-memory HNSW index can be rebuilt
///      from scratch on palace open. Keyed by a monotonic vector_id (not the
///      drawer UUID) so `hnsw_rs`'s native `usize` external id space maps
///      directly onto redb keys without re-hashing UUIDs.
/// What: Key = `u64` vector_id, value = postcard-encoded `Vec<f32>` (the
///       embedding).
/// Test: Coverage lives in `crate::memory_core::store::hnsw_store::tests`.
pub const VECTORS: TableDefinition<u64, &[u8]> = TableDefinition::new("vectors");

/// Vector key mapping: drawer UUID string → vector_id.
///
/// Why: Callers address vectors by drawer UUID (string); the HNSW index
///      addresses them by `u64`. Storing the mapping in redb eliminates the
///      JSON `key_map` sidecar used by `UsearchStore` and makes orphan
///      compaction a redb scan rather than a session-only diff.
/// What: Key = UUID string (drawer id), value = `u64` vector_id (the row
///       index into the `VECTORS` table).
/// Test: Coverage lives in `crate::memory_core::store::hnsw_store::tests`.
pub const VECTOR_KEYS: TableDefinition<&str, u64> = TableDefinition::new("vector_keys");

/// Tombstone table for soft-deleted vector_ids.
///
/// Why: `hnsw_rs` does not support removal from the in-memory HNSW graph
///      once a point is inserted. Instead of rebuilding the entire index on
///      every delete, we mark the vector_id as tombstoned in redb and filter
///      it out at search time. The tombstones are cleared on a full rebuild
///      (e.g. dream compaction).
/// What: Key = `u64` vector_id (tombstoned), value = empty `&[u8]`.
/// Test: Coverage lives in `crate::memory_core::store::hnsw_store::tests`
///       (`delete_filters_results`).
pub const DELETED_VECTORS: TableDefinition<u64, &[u8]> = TableDefinition::new("deleted_vectors");

/// Persisted vector-id allocator (issue #5005).
///
/// Why: `HnswStore` used to allocate vector ids from a process-local
///      `AtomicU64` seeded at open from `max(VECTORS, VECTOR_KEYS) + 1`. Two
///      live stores over the same database — which the in-process vector-db
///      cache deliberately supports, see
///      `crate::memory_core::store::vector::open_or_get_cached_db` — each
///      seeded their own counter from the same high-water mark and then handed
///      out the same ids, so `VECTOR_KEYS` aliased several drawers onto one
///      `vector_id` and `VECTORS` overwrote in place. Keeping the reservation
///      in redb and bumping it inside the same write transaction as the insert
///      makes allocation serialisable with every other writer on the file.
/// What: Single-row table. Key = [`NEXT_VECTOR_ID`], value = the next
///       unissued `u64` vector_id.
/// Test: `crate::memory_core::store::hnsw_store::tests::
///       two_live_stores_over_one_file_never_alias_ids` and
///       `old_palace_without_a_seq_row_is_seeded_on_open`.
pub const VECTOR_ID_SEQ: TableDefinition<&str, u64> = TableDefinition::new("vector_id_seq");

/// The only key stored in [`VECTOR_ID_SEQ`].
pub const NEXT_VECTOR_ID: &str = "next_vector_id";

/// Triple-storage schema marker (#4810).
///
/// Why: the #4810 key widening is an on-disk format change, and the migration
/// that performs it must be able to tell a migrated palace from one that has
/// never been touched. It cannot infer that from the rows: a two-component key
/// and a three-component key are both just bytes, and guessing wrong either
/// re-migrates already-correct rows or leaves broken ones alone. A marker table
/// makes the question a point read. Mirrors `RoomSchemaMarker` /
/// `WingSchemaMarker`, with one difference — those record which shape wrote the
/// rows, whereas this one GATES a rewrite, so it is written in the same
/// transaction as that rewrite.
/// What: Single-row table. Key = [`KG_SCHEMA_TRIPLE_KEY`], value =
/// postcard-encoded [`KgSchemaMarker`]. A palace with no row predates #4810.
/// Test: `store::kg_redb::tests::migration_stamps_schema_and_is_idempotent`.
pub const KG_SCHEMA: TableDefinition<&str, &[u8]> = TableDefinition::new("kg_schema");

/// The only key stored in [`KG_SCHEMA`] today.
pub const KG_SCHEMA_TRIPLE_KEY: &str = "triple_key";

/// Current triple-key schema version — 1 is the `(subject, predicate, object)`
/// key introduced by #4810. Version 0 (no marker row) is the old
/// `(subject, predicate)` key.
pub const KG_TRIPLE_KEY_SCHEMA_VERSION: u32 = 1;

/// Value stored under [`KG_SCHEMA_TRIPLE_KEY`].
///
/// Why/What: mirrors `RoomSchemaMarker` — a one-field postcard record so the
/// on-disk triple-key shape is self-describing.
/// Test: `store::kg_redb::tests::migration_stamps_schema_and_is_idempotent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KgSchemaMarker {
    pub schema_version: u32,
}

/// Predicates that hold at most ONE active object per subject (#4810).
///
/// Why: this list is INVERTED on purpose — multi-valued is the default and
/// anything absent from it keeps every object asserted against it. The two
/// failure modes are not symmetric. Forgetting to list a multi-valued
/// predicate costs silent data loss: that is precisely the #4810 defect, where
/// `room:General --contains--> drawer:X` overwrote the room's entire
/// membership on every new drawer. Forgetting to list a functional one costs
/// one extra queryable row that a reader can see and a human can retract.
/// Cheap-and-visible beats silent-and-gone, so the default falls that way.
/// What: a sorted static slice; [`is_functional_predicate`] is a linear scan
/// over it. The first four entries are trusty-memory's ADR-0028 `HOT_PREDICATES`
/// (`is_alias_for`, `has_convention`, `is_fact`, `is_shorthand_for`); the rest
/// are single-valued attributes minted by the bootstrap scanner and the
/// supersession machinery.
///
/// `has_language` is deliberately EXCLUDED even though a project usually has
/// one primary language: `scan_cargo_toml` and `scan_package_json` derive their
/// subject independently from their own manifest, so a polyglot repo whose
/// `Cargo.toml` and `package.json` agree on the package name asserts two
/// `has_language` triples under one subject. Marking it functional would make
/// whichever scanner ran second silently erase the other's language.
/// Test: `functional_predicates_are_sorted_and_unique`,
/// `is_functional_predicate_defaults_to_multi_valued`.
pub const FUNCTIONAL_PREDICATES: &[&str] = &[
    "alias_of",
    "bootstrapped_at",
    "created_at",
    "has_convention",
    "has_description",
    "has_edition",
    "has_module_path",
    "has_rust_version",
    "has_version",
    "is_alias_for",
    "is_fact",
    "is_shorthand_for",
    "requires_python",
    "source_repo",
    "superseded_by",
];

/// Whether asserting `predicate` supersedes the subject's prior object.
///
/// Why: the write path branches on this — a functional predicate closes every
/// active row at `(subject, predicate)` before inserting, a multi-valued one
/// adds a row alongside them.
/// What: linear scan over [`FUNCTIONAL_PREDICATES`]; unlisted means
/// multi-valued.
/// Test: `is_functional_predicate_defaults_to_multi_valued`.
pub fn is_functional_predicate(predicate: &str) -> bool {
    FUNCTIONAL_PREDICATES.contains(&predicate)
}

/// Chat-session store (for the trusty-memory web UI's chat panel).
///
/// Why: Each chat session is keyed by a UUID string and carries a small
///      JSON-encoded history blob. A dedicated table keeps session rows out
///      of the KG range scans and supports the same redb-on-disk format the
///      rest of the Memory Palace already uses (issue #56).
/// What: Key = session id (UTF-8 bytes, typically a UUID).
///       Value = postcard-encoded `ChatSessionRecord` (title, created_at,
///       updated_at, JSON-encoded history string).
/// Test: Round-trips via `crate::memory_core::store::chat_sessions::tests`.
pub const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("chat_sessions");

// ── Value types (postcard-serializable) ──────────────────────────────────

/// Why: The TRIPLES table value carries the object plus temporal/confidence
///      metadata; keep it serde-derived so postcard can pack it densely.
/// What: A single triple's value payload — object, valid time window,
///       confidence, optional provenance string.
/// Test: `round_trip_triple_value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TripleValue {
    pub object: String,
    /// Unix epoch milliseconds when this fact became valid.
    pub valid_from_ms: i64,
    /// Unix epoch milliseconds when this fact was invalidated. `None` = active.
    pub valid_to_ms: Option<i64>,
    pub confidence: f32,
    pub provenance: Option<String>,
}

/// Why: Drawer rows carry content + tags + importance for the Memory Palace
///      "drawer" abstraction; serde-encoded so we can add fields without a
///      schema migration.
/// What: A drawer's metadata payload.
/// Test: `round_trip_drawer_record`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawerRecord {
    pub room_id: String,
    pub content: String,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source_file: Option<String>,
    /// Unix epoch milliseconds when the drawer was created.
    pub created_at_ms: i64,
    /// Issue #61: signal-vs-noise tag. `None` for rows written before the
    /// field existed; readers fall back to `DrawerType::Unknown`. Stored as
    /// the variant name string so the on-disk schema is stable across
    /// future enum extensions (postcard would otherwise renumber variants).
    #[serde(default)]
    pub drawer_type: Option<String>,
    /// Issue #61: optional TTL expressed as epoch milliseconds. The
    /// `purge_expired` sweep at palace open drops rows where this value is
    /// in the past.
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    /// spec-001: optional `Task` completion timestamp (epoch ms). `None` for
    /// open tasks and every non-Task drawer. Because postcard is positional,
    /// rows written before this field existed fail to decode as the current
    /// shape; the reader falls back through `PreTaskDrawerRecord` (the #61-era
    /// shape) and then `LegacyDrawerRecord` to migrate them forward with this
    /// field defaulted to `None`.
    #[serde(default)]
    pub completed_at_ms: Option<i64>,
    /// #4884: ADR-0028 D5 slot name. `None` for every row written before the
    /// field existed. Because postcard is positional, adding it means
    /// spec-001-era rows no longer decode as this shape; the reader falls back
    /// through `PreFactKeyDrawerRecord` → `PreTaskDrawerRecord` →
    /// `LegacyDrawerRecord`, each lifting the row forward with the fields it
    /// predates defaulted. No existing row is rewritten to add the field.
    #[serde(default)]
    pub fact_key: Option<String>,
}

// ── Key encoding helpers ─────────────────────────────────────────────────

/// Why: redb requires Ord-preserving byte keys for range scans. Composite
///      string keys are encoded with a u16 BE length prefix per leading
///      component so prefix-based range scans (`subject..`, `(subject,
///      predicate)..`) work correctly. #4810 added the object as the trailing
///      component so two objects under one `(subject, predicate)` occupy two
///      rows instead of overwriting each other.
/// What: Encodes `(subject, predicate, object)` → `Vec<u8>` for TRIPLES table
///       lookup. The object needs no length prefix — it is the last component.
/// Test: `round_trip_triple_key`, `triple_key_separates_objects`.
pub fn encode_triple_key(subject: &str, predicate: &str, object: &str) -> Vec<u8> {
    let s = subject.as_bytes();
    let p = predicate.as_bytes();
    let o = object.as_bytes();
    let mut out = Vec::with_capacity(4 + s.len() + p.len() + o.len());
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(&(p.len() as u16).to_be_bytes());
    out.extend_from_slice(p);
    out.extend_from_slice(o);
    out
}

/// Why: Round-trip decode for diagnostic/debug paths and tests.
/// What: Splits an encoded triple key back into `(subject, predicate, object)`.
///       Returns `None` if the key is malformed (a length prefix exceeds the
///       bytes remaining, or an interior span is not valid UTF-8).
/// Test: `round_trip_triple_key`, `decode_triple_key_rejects_truncated`.
pub fn decode_triple_key(bytes: &[u8]) -> Option<(String, String, String)> {
    if bytes.len() < 2 {
        return None;
    }
    let s_len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let rest = &bytes[2..];
    if rest.len() < s_len {
        return None;
    }
    let subject = std::str::from_utf8(&rest[..s_len]).ok()?.to_string();
    let rest = &rest[s_len..];
    if rest.len() < 2 {
        return None;
    }
    let p_len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
    let rest = &rest[2..];
    if rest.len() < p_len {
        return None;
    }
    let predicate = std::str::from_utf8(&rest[..p_len]).ok()?.to_string();
    let object = std::str::from_utf8(&rest[p_len..]).ok()?.to_string();
    Some((subject, predicate, object))
}

/// Why: The #4810 migration is the one code path that must read a key written
///      before the object joined it; every other reader uses
///      [`decode_triple_key`].
/// What: Splits a pre-#4810 key (`[subject_len][subject][predicate]`) into
///       `(subject, predicate)`. Returns `None` on the same malformed-input
///       conditions as [`decode_triple_key`].
/// Test: `decode_legacy_triple_key_round_trips`.
pub fn decode_legacy_triple_key(bytes: &[u8]) -> Option<(String, String)> {
    if bytes.len() < 2 {
        return None;
    }
    let s_len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let rest = &bytes[2..];
    if rest.len() < s_len {
        return None;
    }
    let subject = std::str::from_utf8(&rest[..s_len]).ok()?.to_string();
    let predicate = std::str::from_utf8(&rest[s_len..]).ok()?.to_string();
    Some((subject, predicate))
}

/// Why: Reverse lookup by object — find all (subject, predicate) pairs that
///      point at a given object.
/// What: Encodes `(object, subject, predicate)` → composite key with two
///       length-prefixed leading components so the object prefix sorts first.
/// Test: `object_index_key_orders_by_object`.
pub fn encode_object_index_key(object: &str, subject: &str, predicate: &str) -> Vec<u8> {
    let o = object.as_bytes();
    let s = subject.as_bytes();
    let p = predicate.as_bytes();
    let mut out = Vec::with_capacity(4 + o.len() + s.len() + p.len());
    out.extend_from_slice(&(o.len() as u16).to_be_bytes());
    out.extend_from_slice(o);
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(p);
    out
}

/// Why: Predicate-first index — find all subjects connected via a given
///      predicate. #4810 added the object so two objects under one
///      `(predicate, subject)` no longer collapse into a single entry.
/// What: Encodes `(predicate, subject, object)` → composite key with two
///       length-prefixed leading components.
/// Test: `predicate_index_key_orders_by_predicate`,
///       `predicate_index_key_separates_objects`.
pub fn encode_predicate_index_key(predicate: &str, subject: &str, object: &str) -> Vec<u8> {
    let p = predicate.as_bytes();
    let s = subject.as_bytes();
    let o = object.as_bytes();
    let mut out = Vec::with_capacity(4 + p.len() + s.len() + o.len());
    out.extend_from_slice(&(p.len() as u16).to_be_bytes());
    out.extend_from_slice(p);
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(o);
    out
}

/// Why: Range scans by subject use `range(prefix..end)` where `prefix` is
///      `[subject_len][subject]`; this helper computes that prefix.
/// What: Subject prefix = `[subject_len: u16 BE][subject bytes]`.
/// Test: `subject_prefix_range_simulation`.
pub fn subject_prefix(subject: &str) -> Vec<u8> {
    let s = subject.as_bytes();
    let mut out = Vec::with_capacity(2 + s.len());
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
    out
}

/// Why (#4810): `assert` and `retract` both need "every row at this
///      `(subject, predicate)`, whatever the object" — the question the old
///      point-read answered by accident when the pair was the whole key.
/// What: `(subject, predicate)` prefix = `[subject_len: u16 BE][subject]
///       [predicate_len: u16 BE][predicate]`. Because the predicate carries its
///       own length prefix, a longer predicate sharing this one's leading bytes
///       (`has_convention` vs `has_conventions`) does NOT fall inside the
///       range.
/// Test: `subject_predicate_prefix_excludes_a_longer_predicate`.
pub fn subject_predicate_prefix(subject: &str, predicate: &str) -> Vec<u8> {
    let s = subject.as_bytes();
    let p = predicate.as_bytes();
    let mut out = Vec::with_capacity(4 + s.len() + p.len());
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(&(p.len() as u16).to_be_bytes());
    out.extend_from_slice(p);
    out
}

/// Why: every prefix range scan over [`TRIPLES`] needs an exclusive upper
///      bound, and every call site was building it the same way by hand.
/// What: returns `prefix` with `0xFF` appended. Whatever follows the prefix in
///       a real key is either UTF-8 text (`0xFF` is never a legal UTF-8 byte)
///       or the high byte of a u16 length, which reaches `0xFF` only for a
///       component of 65 280 bytes or more.
/// Test: `subject_predicate_prefix_excludes_a_longer_predicate`.
pub fn prefix_range_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    end.push(0xFF);
    end
}

/// Why: The PAYLOADS table is keyed by `(segment, id)`; this helper produces
///      the composite key for both reads and writes.
/// What: Payload key = `[segment_len: u16 BE][segment bytes][id bytes]`.
/// Test: `round_trip_payload_key`.
pub fn encode_payload_key(segment: &str, id: &[u8]) -> Vec<u8> {
    let seg = segment.as_bytes();
    let mut out = Vec::with_capacity(2 + seg.len() + id.len());
    out.extend_from_slice(&(seg.len() as u16).to_be_bytes());
    out.extend_from_slice(seg);
    out.extend_from_slice(id);
    out
}

/// Why: Range scans by segment use `range(prefix..end)` where `prefix` is
///      `[segment_len][segment]`; this helper computes that prefix so callers
///      can enumerate every payload row in a given segment.
/// What: Segment prefix = `[segment_len: u16 BE][segment bytes]`.
/// Test: `payload_keys_group_by_segment` verifies key ordering matches the
///       prefix derived from this helper.
pub fn segment_prefix(segment: &str) -> Vec<u8> {
    let seg = segment.as_bytes();
    let mut out = Vec::with_capacity(2 + seg.len());
    out.extend_from_slice(&(seg.len() as u16).to_be_bytes());
    out.extend_from_slice(seg);
    out
}

// ── Value encode/decode ──────────────────────────────────────────────────

/// Why: All value types share a single postcard codec — central helper keeps
///      call sites concise and the format consistent.
/// What: Serializes `v` to a `Vec<u8>` using postcard.
/// Test: `round_trip_triple_value`, `round_trip_drawer_record`.
pub fn encode_value<T: Serialize>(v: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(v)
}

/// Why: Mirror of [`encode_value`] for reads.
/// What: Deserializes a postcard-encoded byte slice into `T`.
/// Test: `round_trip_triple_value`, `round_trip_drawer_record`.
pub fn decode_value<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}

/// Why: redb table values are `&[u8]`, so the `active_subject_counts` u64
///      needs an explicit LE encoding rather than postcard wrapping.
/// What: Encodes a `u64` as 8 little-endian bytes.
/// Test: `round_trip_u64`.
pub fn encode_u64(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

/// Why: Mirror of [`encode_u64`].
/// What: Decodes 8 LE bytes into a `u64`. Returns 0 if `bytes.len() < 8`
///       (matches redb's "missing key returns zero" convention for counts).
/// Test: `round_trip_u64`.
pub fn decode_u64(bytes: &[u8]) -> u64 {
    if bytes.len() < 8 {
        return 0;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(buf)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_triple_key() {
        let key = encode_triple_key("user:alice", "knows", "user:bob");
        let (subject, predicate, object) = decode_triple_key(&key).expect("decode");
        assert_eq!(subject, "user:alice");
        assert_eq!(predicate, "knows");
        assert_eq!(object, "user:bob");
    }

    #[test]
    fn round_trip_triple_key_empty_components() {
        let key = encode_triple_key("subj", "", "");
        let (s, p, o) = decode_triple_key(&key).expect("decode");
        assert_eq!(s, "subj");
        assert_eq!(p, "");
        assert_eq!(o, "");
    }

    #[test]
    fn triple_key_separates_objects() {
        // #4810: the whole point — two objects under one (subject, predicate)
        // must be two distinct keys, both inside the pair's prefix range.
        let a = encode_triple_key("room:General", "contains", "drawer:a");
        let b = encode_triple_key("room:General", "contains", "drawer:b");
        assert_ne!(a, b);
        let prefix = subject_predicate_prefix("room:General", "contains");
        assert!(a.starts_with(&prefix));
        assert!(b.starts_with(&prefix));
    }

    #[test]
    fn decode_triple_key_rejects_truncated() {
        assert!(decode_triple_key(&[]).is_none());
        assert!(decode_triple_key(&[0u8]).is_none());
        // subject length prefix says 10 bytes follow but only 2 do
        assert!(decode_triple_key(&[0, 10, b'a', b'b']).is_none());
        // subject decodes, but there is no predicate length prefix
        assert!(decode_triple_key(&[0, 2, b'a', b'b']).is_none());
        // predicate length prefix says 9 bytes follow but only 1 does
        assert!(decode_triple_key(&[0, 2, b'a', b'b', 0, 9, b'p']).is_none());
    }

    #[test]
    fn decode_legacy_triple_key_round_trips() {
        // The pre-#4810 shape the migration reads: no predicate length prefix.
        let mut legacy = Vec::new();
        legacy.extend_from_slice(&(5u16).to_be_bytes());
        legacy.extend_from_slice(b"alice");
        legacy.extend_from_slice(b"knows");
        let (s, p) = decode_legacy_triple_key(&legacy).expect("decode legacy");
        assert_eq!(s, "alice");
        assert_eq!(p, "knows");
    }

    #[test]
    fn subject_prefix_range_simulation() {
        // Every triple for subject "alice" must start with `subject_prefix("alice")`,
        // and no triple for subject "alicia" should — even though "alicia" starts
        // with "alic" — because the length prefix differs.
        let prefix_alice = subject_prefix("alice");
        let alice_knows = encode_triple_key("alice", "knows", "bob");
        let alice_likes = encode_triple_key("alice", "likes", "tea");
        let alicia_knows = encode_triple_key("alicia", "knows", "bob");

        assert!(alice_knows.starts_with(&prefix_alice));
        assert!(alice_likes.starts_with(&prefix_alice));
        assert!(!alicia_knows.starts_with(&prefix_alice));
    }

    #[test]
    fn subject_predicate_prefix_excludes_a_longer_predicate() {
        // The predicate's own length prefix is what keeps `has_convention`'s
        // range from swallowing `has_conventions` rows.
        let prefix = subject_predicate_prefix("proj", "has_convention");
        let end = prefix_range_end(&prefix);
        let inside = encode_triple_key("proj", "has_convention", "tabs");
        let outside = encode_triple_key("proj", "has_conventions", "tabs");
        assert!(inside.starts_with(&prefix));
        assert!(!outside.starts_with(&prefix));
        assert!(inside.as_slice() >= prefix.as_slice() && inside.as_slice() < end.as_slice());
    }

    #[test]
    fn subject_prefix_orders_lexicographically() {
        // BTreeMap-backed redb tables sort keys lexicographically. Length-
        // prefixed keys with the same length sort by content order.
        let k1 = encode_triple_key("aaa", "p", "o");
        let k2 = encode_triple_key("aab", "p", "o");
        let k3 = encode_triple_key("bbb", "p", "o");
        assert!(k1 < k2);
        assert!(k2 < k3);
    }

    #[test]
    fn functional_predicates_are_sorted_and_unique() {
        // Sorted-and-unique is what makes the list reviewable; a duplicate
        // would be a silent no-op and an out-of-order entry hides near-misses.
        let mut sorted = FUNCTIONAL_PREDICATES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), FUNCTIONAL_PREDICATES);
    }

    #[test]
    fn is_functional_predicate_defaults_to_multi_valued() {
        assert!(is_functional_predicate("is_alias_for"));
        assert!(is_functional_predicate("has_version"));
        // #4810: the defect predicate, and every unlisted predicate, is
        // multi-valued.
        assert!(!is_functional_predicate("contains"));
        assert!(!is_functional_predicate("works_at"));
        assert!(!is_functional_predicate("knows"));
        // Deliberately excluded — two manifests can name one subject.
        assert!(!is_functional_predicate("has_language"));
    }

    #[test]
    fn object_index_key_orders_by_object() {
        // All entries with the same object must sort together and before any
        // entry with a strictly greater object.
        let k1 = encode_object_index_key("obj_a", "s1", "p1");
        let k2 = encode_object_index_key("obj_a", "s2", "p2");
        let k3 = encode_object_index_key("obj_b", "s0", "p0");
        assert!(k1 < k2);
        assert!(k2 < k3);
    }

    #[test]
    fn predicate_index_key_orders_by_predicate() {
        let k1 = encode_predicate_index_key("knows", "s1", "o");
        let k2 = encode_predicate_index_key("knows", "s2", "o");
        let k3 = encode_predicate_index_key("likes", "s0", "o");
        assert!(k1 < k2);
        assert!(k2 < k3);
    }

    #[test]
    fn predicate_index_key_separates_objects() {
        // #4810: one entry per (predicate, subject) meant retracting one
        // object evicted the index entry the other object still needed.
        let a = encode_predicate_index_key("contains", "room:General", "drawer:a");
        let b = encode_predicate_index_key("contains", "room:General", "drawer:b");
        assert_ne!(a, b);
    }

    #[test]
    fn round_trip_triple_value() {
        let v = TripleValue {
            object: "user:bob".to_string(),
            valid_from_ms: 1_700_000_000_000,
            valid_to_ms: Some(1_710_000_000_000),
            confidence: 0.85,
            provenance: Some("test/path.rs:42".to_string()),
        };
        let bytes = encode_value(&v).expect("encode");
        let decoded: TripleValue = decode_value(&bytes).expect("decode");
        assert_eq!(v, decoded);
    }

    #[test]
    fn round_trip_triple_value_active() {
        // Active triple has `valid_to_ms = None`.
        let v = TripleValue {
            object: "x".to_string(),
            valid_from_ms: 0,
            valid_to_ms: None,
            confidence: 1.0,
            provenance: None,
        };
        let bytes = encode_value(&v).expect("encode");
        let decoded: TripleValue = decode_value(&bytes).expect("decode");
        assert_eq!(v, decoded);
    }

    #[test]
    fn round_trip_drawer_record() {
        let d = DrawerRecord {
            room_id: "room-1".to_string(),
            content: "Project kickoff notes".to_string(),
            importance: 0.7,
            tags: vec!["project".to_string(), "kickoff".to_string()],
            source_file: Some("notes/2025-01-01.md".to_string()),
            created_at_ms: 1_700_000_000_000,
            drawer_type: Some("UserFact".to_string()),
            expires_at_ms: Some(1_710_000_000_000),
            completed_at_ms: Some(1_720_000_000_000),
            fact_key: Some("pr:4818/state".to_string()),
        };
        let bytes = encode_value(&d).expect("encode");
        let decoded: DrawerRecord = decode_value(&bytes).expect("decode");
        assert_eq!(d, decoded);
    }

    #[test]
    fn drawer_record_new_fields_default_to_none() {
        // Issue #61: when the writer omits drawer_type / expires_at_ms (e.g.
        // by constructing via `..Default::default()`-style flow), the
        // decoded round-trip preserves `None`. (Postcard is positional so
        // legacy on-disk bytes that omit the trailing fields must be
        // migrated by the reader — see `LegacyDrawerRecord` in `kg_redb.rs`
        // for that path.)
        let d = DrawerRecord {
            room_id: "room-1".to_string(),
            content: "legacy".to_string(),
            importance: 0.5,
            tags: vec![],
            source_file: None,
            created_at_ms: 1,
            drawer_type: None,
            expires_at_ms: None,
            completed_at_ms: None,
            fact_key: None,
        };
        let bytes = encode_value(&d).expect("encode");
        let decoded: DrawerRecord = decode_value(&bytes).expect("decode");
        assert_eq!(d, decoded);
        assert!(decoded.drawer_type.is_none());
        assert!(decoded.expires_at_ms.is_none());
        assert!(decoded.completed_at_ms.is_none());
        // #4884: a drawer that claims no slot must decode as claiming none —
        // an accidental `Some("")` would occupy a real key in the index.
        assert!(decoded.fact_key.is_none());
    }

    #[test]
    fn round_trip_u64() {
        for v in [0u64, 1, 42, u64::MAX, 1_000_000_000_000] {
            let bytes = encode_u64(v);
            assert_eq!(decode_u64(&bytes), v);
        }
    }

    #[test]
    fn decode_u64_short_returns_zero() {
        // Match the "missing key returns zero" convention used by callers.
        assert_eq!(decode_u64(&[]), 0);
        assert_eq!(decode_u64(&[1, 2, 3]), 0);
    }

    #[test]
    fn round_trip_payload_key() {
        let id = [0xAB, 0xCD, 0xEF, 0x01];
        let k = encode_payload_key("session", &id);
        // Verify segment length prefix.
        assert_eq!(&k[0..2], &(7u16).to_be_bytes());
        assert_eq!(&k[2..9], b"session");
        assert_eq!(&k[9..], &id);
    }

    #[test]
    fn payload_keys_group_by_segment() {
        // Keys with the same segment prefix sort together.
        let k1 = encode_payload_key("seg_a", &[1, 2, 3]);
        let k2 = encode_payload_key("seg_a", &[1, 2, 4]);
        let k3 = encode_payload_key("seg_b", &[0]);
        assert!(k1 < k2);
        assert!(k2 < k3);
    }

    #[test]
    fn table_definitions_have_distinct_names() {
        use redb::TableHandle;
        // Sanity check: no two tables share the same name (would alias in redb).
        let names = [
            TRIPLES.name(),
            TRIPLES_BY_OBJECT.name(),
            TRIPLES_BY_PREDICATE.name(),
            ACTIVE_SUBJECT_COUNTS.name(),
            DRAWERS.name(),
            ROOMS.name(),
            ROOM_KEYS.name(),
            WINGS.name(),
            WING_KEYS.name(),
            PAYLOADS.name(),
            SESSIONS.name(),
            RECALL_LOG.name(),
            VECTORS.name(),
            VECTOR_KEYS.name(),
            DELETED_VECTORS.name(),
            VECTOR_ID_SEQ.name(),
            DRAWERS_BY_FACT_KEY.name(),
            KG_SCHEMA.name(),
        ];
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                assert_ne!(names[i], names[j]);
            }
        }
    }
}
