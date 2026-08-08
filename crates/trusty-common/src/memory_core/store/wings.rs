//! Wing registry records, default-wing seeding, and the wing entry points.
//!
//! Why (ADR-0027 D2): a Wing is the scope/ownership axis — "who" — as opposed
//! to a Room's topic axis — "what". It is what lets `engineer/Planning` and
//! `pm/Planning` be two rooms without name mangling, and it is where #3064's
//! "two agent types cannot accidentally read/write the same room unless
//! configured to do so" will eventually hang its configuration. This module
//! owns the on-disk row shape and every policy decision about wings.
//!
//! **Migration posture — naming, never reclassification.** Placing existing
//! rooms in the default wing requires writing nothing to `ROOMS` at all:
//! `RoomRecord::wing_id` has carried [`DEFAULT_WING_ID`] since ADR-0027 T1, so
//! the entire migration is inserting ONE row describing a wing that every room
//! already points at. Zero drawer rows change and zero room ids are rewritten,
//! and that is proven byte-for-byte rather than asserted (see
//! `seeding_the_default_wing_changes_no_room_or_drawer_rows`).
//!
//! **Wing is never required of a caller** (ADR-0027 D2). Every palace gets a
//! default wing, every room defaults into it, and no pre-existing call site
//! gains an argument. A caller who never mentions a wing behaves exactly as it
//! did before this module existed.
//!
//! Storage note: the tables live in the palace's `kg.db` beside `ROOMS` and
//! `DRAWERS`, NOT in a JSON sidecar, for the corruption-recovery reason spelled
//! out in ADR-0027 D1.1 — wings, rooms, and drawers corrupt and recover as one
//! unit.
//!
//! Test: `wings_tests.rs`.

use crate::memory_core::room_identity::DEFAULT_WING_ID;
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::kg_redb::KgStoreRedb;
use crate::memory_core::wing_identity::{
    DEFAULT_WING_LABEL, canonical_wing_key, default_wing_key, mint_wing_id,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

/// Schema version stamped into the `WINGS` marker row.
///
/// Bump only when the *meaning* of existing rows changes; appending a trailing
/// optional field does not qualify (the decode chain handles that).
pub const WING_SCHEMA_VERSION: u32 = 1;

/// On-disk wing row.
///
/// Why: field order is load-bearing — postcard is positional, so a new field is
/// APPENDED and old bytes are recovered through a fallback chain exactly as
/// `DrawerRecord` and `RoomRecord` already do. Never insert a field in the
/// middle.
///
/// Deliberately minimal: ADR-0027 D2 names a Wing as the eventual anchor for
/// #3064's per-wing access configuration, but no such field is reserved here.
/// The trailing-optional evolution pattern IS the mechanism that keeps it
/// satisfiable later — adding `access: Option<…>` then costs no migration —
/// and a field nothing reads is the exact defect this ADR exists to correct.
///
/// What: the first-seen display spelling, the creation stamp, and an optional
/// human description.
/// Test: `wing_record_round_trip`, `wing_record_decodes_under_a_future_field`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WingRecord {
    /// First-seen display spelling, e.g. `"default"`, `"engineer"`, `"pm"`.
    pub label: String,
    pub created_at_ms: i64,
    pub description: Option<String>,
}

/// Schema-version marker stored under the nil-UUID key in `WINGS`.
///
/// Why/What: mirrors `RoomSchemaMarker` — it lets a future migration recognise
/// which shape wrote these rows. Seeding idempotency does NOT depend on it; it
/// comes from the by-id existence probe, which is the stronger guarantee
/// because it also preserves a rename.
/// Test: `default_wing_is_seeded_once`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WingSchemaMarker {
    pub schema_version: u32,
}

/// A decoded wing row plus its id and room population — the read-side view.
#[derive(Debug, Clone, PartialEq)]
pub struct WingSummary {
    pub id: Uuid,
    pub label: String,
    pub created_at_ms: i64,
    pub description: Option<String>,
    /// How many `ROOMS` rows name this wing.
    pub room_count: usize,
    /// Whether this is the palace's default wing — the one every room falls
    /// into when a caller names none.
    pub is_default: bool,
}

impl WingRecord {
    /// Build a record for `label` with the current timestamp.
    pub fn new(label: impl Into<String>, created_at_ms: i64, description: Option<String>) -> Self {
        Self {
            label: label.into(),
            created_at_ms,
            description,
        }
    }

    /// Pair this row with its id and room population for the read surface.
    pub fn summarize(&self, id: Uuid, room_count: usize) -> WingSummary {
        WingSummary {
            id,
            label: self.label.clone(),
            created_at_ms: self.created_at_ms,
            description: self.description.clone(),
            room_count,
            is_default: id == DEFAULT_WING_ID,
        }
    }
}

/// Reject a wing label that cannot address a wing.
///
/// Why: an empty or whitespace-only label normalises to an empty canonical
/// key, which would alias every other empty-labelled create into one wing and
/// produce a wing nobody can name. Failing loud at the boundary is cheaper
/// than a silently-merged scope.
/// What: trims; errors when the result is empty.
/// Test: `wing_create_rejects_a_blank_label`.
pub fn normalize_wing_label(label: &str) -> Result<String> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        bail!("wing label must not be empty");
    }
    Ok(trimmed.to_string())
}

/// Seed the palace's default wing if it does not already exist.
///
/// Why (ADR-0027 D2): every palace gets a default wing so that "wing" is never
/// a required concept for a caller. This is the whole wing migration — because
/// `RoomRecord::wing_id` has been [`DEFAULT_WING_ID`] since T1, no room row and
/// no drawer row needs to change for existing rooms to be *in* this wing. They
/// are named, not reclassified.
/// What: probes `WINGS` **by id**, and inserts the row plus its canonical key
/// only when absent. Probing by id rather than by key is what lets a renamed
/// default wing keep its new name — a key probe would resurrect `"default"` as
/// an alias on the next open. Returns `true` when a row was written.
///
/// **An already-seeded palace is left entirely alone**, schema marker included:
/// this function returns before reaching the stamp, so opening a palace costs
/// one read transaction and no write. That is deliberate rather than an
/// oversight — see `set_wing_schema_version`'s call contract. Seeding is not a
/// migration, so it must never restamp a marker over rows it did not convert;
/// a future [`WING_SCHEMA_VERSION`] bump needs a real migration pass that
/// converts the rows and stamps the new version itself.
/// Test: `default_wing_is_seeded_once`, `wing_rename_survives_reseed`,
/// `seeding_the_default_wing_changes_no_room_or_drawer_rows`,
/// `reseeding_does_not_restamp_the_schema_version`.
pub fn ensure_default_wing(kg: &KnowledgeGraph) -> Result<bool> {
    let store = kg.store();
    if store
        .get_wing(DEFAULT_WING_ID)
        .context("probe default wing row")?
        .is_some()
    {
        return Ok(false);
    }
    let record = WingRecord::new(
        DEFAULT_WING_LABEL,
        chrono::Utc::now().timestamp_millis(),
        Some("Default scope — every room a caller does not place lands here.".to_string()),
    );
    // The id is written VERBATIM, never minted: every room row already carries
    // DEFAULT_WING_ID, so this is the same "ids are read from the table"
    // discipline that keeps legacy room ids intact (ADR-0027 D1.3).
    let inserted = store
        .insert_wing_if_absent(DEFAULT_WING_ID, &default_wing_key(), &record)
        .context("seed default wing")?;
    if inserted {
        store
            .set_wing_schema_version()
            .context("stamp wing schema version")?;
    }
    Ok(inserted)
}

/// Seed the default wing, swallowing every failure.
///
/// Why (ADR-0027 D1.4, mirroring `backfill_rooms_fail_open`): a palace whose
/// wing registry cannot be written must still open. The cost of failing open is
/// that `wing_list` is empty until the next successful open; the cost of
/// failing closed is an unopenable palace. A read-only (snapshot) palace takes
/// this path every time and that is correct — it has nothing to write to.
/// What: calls [`ensure_default_wing`] and logs any error at `warn!`.
/// Test: `fail_open_seeding_creates_the_default_wing`.
pub fn ensure_default_wing_fail_open(palace_id: &str, kg: &KnowledgeGraph) {
    if kg.is_read_only() {
        return;
    }
    match ensure_default_wing(kg) {
        Ok(true) => tracing::info!(
            palace = palace_id,
            "seeded default wing (no room or drawer rows touched)"
        ),
        Ok(false) => {}
        Err(e) => tracing::warn!(
            palace = palace_id,
            "default wing seeding failed, wing listing degraded: {e:#}"
        ),
    }
}

/// Resolve a caller-supplied wing selector to a wing id.
///
/// Why: an MCP caller has a string, not a `Uuid`, and may reasonably hold
/// either a wing id echoed back from `wing_list` or the label a human typed.
/// Accepting both means neither surface has to teach the other's format.
/// What: tries `selector` as a UUID that has a `WINGS` row first, then as a
/// canonical label. `Ok(None)` means "no such wing" — a caller-facing
/// condition, not an error.
/// Test: `wing_selector_accepts_id_or_label`.
pub fn resolve_wing_selector(kg: &KnowledgeGraph, selector: &str) -> Result<Option<Uuid>> {
    let store = kg.store();
    if let Ok(id) = Uuid::parse_str(selector.trim())
        && store.get_wing(id).context("probe wing by id")?.is_some()
    {
        return Ok(Some(id));
    }
    store
        .lookup_wing_id(&canonical_wing_key(selector))
        .context("look up wing by label")
}

/// Every registered wing with its room population, id-ordered.
///
/// Why: the discovery primitive behind `wing_list`. Without it the `WINGS`
/// table would be exactly the dark level ADR-0027 exists to stop shipping.
/// What: joins `list_wings` against `list_rooms`, counting rooms per wing in
/// one pass rather than re-scanning per wing.
/// Test: `wing_list_reports_seeded_and_created_wings`,
/// `wing_list_counts_rooms_per_wing`.
pub fn list_wings(kg: &KnowledgeGraph) -> Result<Vec<WingSummary>> {
    let store = kg.store();
    let rooms = store.list_rooms().context("list rooms for wing counts")?;
    store
        .list_wings()
        .context("list wings")?
        .into_iter()
        .map(|(id, record)| {
            let count = rooms
                .iter()
                .filter(|(_, r)| Uuid::from_bytes(r.wing_id) == id)
                .count();
            Ok(record.summarize(id, count))
        })
        .collect()
}

/// The set of room ids belonging to `wing_id`.
///
/// Why: wing-scoped recall is "every room this wing owns", and the drawer table
/// stores only `room_id`. This is the join that turns a scope into a filter.
/// What: scans `ROOMS` and collects the ids whose row names `wing_id`.
///
/// A drawer whose room has no `ROOMS` row yet is NOT in any wing and is
/// therefore excluded from every wing-scoped read. That case is transient: the
/// room backfill (ADR-0027 T2) registers every observed `room_id` at palace
/// open, before any query can run against the handle.
/// Test: `rooms_in_wing_separates_same_named_rooms`.
pub fn rooms_in_wing(kg: &KnowledgeGraph, wing_id: Uuid) -> Result<HashSet<Uuid>> {
    Ok(kg
        .store()
        .list_rooms()
        .context("list rooms for wing scope")?
        .into_iter()
        .filter(|(_, r)| Uuid::from_bytes(r.wing_id) == wing_id)
        .map(|(id, _)| id)
        .collect())
}

/// Resolve `label` to a wing id, creating the wing when absent.
///
/// Why: this is `wing_create`'s core, and it is idempotent by construction —
/// creating a wing that exists returns the existing id rather than a second
/// wing, so a caller can call it unconditionally.
/// What: looks the canonical key up; on a hit returns the stored id verbatim;
/// on a miss mints a UUIDv5 and inserts the row and key in one transaction.
/// Returns `(id, created)`.
///
/// Unlike the room write path this does NOT fail open. A room resolution
/// failure must never fail a memory write, so it degrades to the legacy fold;
/// a wing create has no such caller — it is an explicit operation whose only
/// sensible failure mode is telling the caller it failed.
/// Test: `wing_create_is_idempotent`, `wing_create_returns_the_default_wing`.
pub fn resolve_or_create_wing_sync(store: &Arc<KgStoreRedb>, label: &str) -> Result<(Uuid, bool)> {
    let label = normalize_wing_label(label)?;
    let key = canonical_wing_key(&label);
    if let Some(id) = store.lookup_wing_id(&key).context("look up wing key")? {
        return Ok((id, false));
    }
    let id = mint_wing_id(&key);
    let record = WingRecord::new(label.clone(), chrono::Utc::now().timestamp_millis(), None);
    let inserted = store
        .insert_wing_if_absent(id, &key, &record)
        .with_context(|| format!("register wing {label:?}"))?;
    // `insert_wing_if_absent` probes and inserts inside ONE write transaction,
    // and redb allows a single writer at a time, so the key is guaranteed
    // present once it returns: either this call inserted it or an earlier one
    // did. The re-read therefore reports which writer won rather than guarding
    // a partial write, and `unwrap_or(id)` is unreachable in practice.
    //
    // A racer cannot diverge here anyway: the id is `mint_wing_id(&key)`, a
    // UUIDv5 of the same key, so two concurrent creates of the same label
    // compute the SAME id. The race is benign by construction — which is why
    // this returns a `created` flag rather than an error.
    let winner = store
        .lookup_wing_id(&key)
        .context("re-read wing key")?
        .unwrap_or(id);
    Ok((winner, inserted && winner == id))
}

/// Async wrapper over [`resolve_or_create_wing_sync`].
///
/// Why: the redb work is blocking and every MCP handler is async, so the write
/// runs on the blocking pool — the same posture `resolve_or_create_room` takes.
/// Test: `wing_create_is_idempotent` covers the sync core.
pub async fn resolve_or_create_wing(kg: &KnowledgeGraph, label: &str) -> Result<(Uuid, bool)> {
    let store = kg.store();
    let label = label.to_string();
    tokio::task::spawn_blocking(move || resolve_or_create_wing_sync(&store, &label))
        .await
        .context("join wing create")?
}

/// Rename a wing, retiring its old label.
///
/// Why: this is the repair path — and the reason the seeding pass probes by id.
/// A wing's *id* is what every `RoomRecord` references, so renaming the label
/// cannot move a room and provably cannot touch a drawer.
/// What: validates the new label, refuses a label another wing already holds,
/// then applies the row rewrite, the new key, and the old key's retirement in
/// ONE redb write transaction so the previous name stops resolving (a rename,
/// not an alias). Never touches `ROOMS` or `DRAWERS`.
///
/// Atomicity is load-bearing, not incidental: if the old-key removal could
/// commit separately, a crash between the two commits would leave the retired
/// label resolving to the renamed wing forever. In a scope mechanism that is a
/// leak, not cosmetic drift — which is why it is a single transaction.
/// Test: `wing_rename_changes_no_room_or_drawer_rows`,
/// `wing_rename_retires_the_old_label`, `wing_rename_rejects_a_taken_label`,
/// `wing_rename_survives_reseed`, `wing_rename_applies_every_effect_together`.
pub fn rename_wing_sync(
    store: &Arc<KgStoreRedb>,
    id: Uuid,
    new_label: &str,
) -> Result<WingSummary> {
    let new_label = normalize_wing_label(new_label)?;
    let new_key = canonical_wing_key(&new_label);
    // Existence, uniqueness, and every write happen inside ONE transaction in
    // `rename_wing_in_place`. Probing here first would reintroduce the race it
    // exists to close: a concurrent `resolve_or_create_wing_sync` claiming
    // `new_key` between the check and the write.
    let renamed = store
        .rename_wing_in_place(id, &new_key, &new_label)
        .context("write renamed wing")?;
    let room_count = rooms_in_wing_from(store, id)?;
    Ok(renamed.summarize(id, room_count))
}

/// Async wrapper over [`rename_wing_sync`].
pub async fn rename_wing(kg: &KnowledgeGraph, id: Uuid, new_label: &str) -> Result<WingSummary> {
    let store = kg.store();
    let new_label = new_label.to_string();
    tokio::task::spawn_blocking(move || rename_wing_sync(&store, id, &new_label))
        .await
        .context("join wing rename")?
}

/// Room population of `wing_id`, counted straight off the store.
///
/// Why: `rename_wing_sync` already holds an `Arc<KgStoreRedb>` and has no
/// `KnowledgeGraph` to hand, so it cannot call [`rooms_in_wing`].
fn rooms_in_wing_from(store: &Arc<KgStoreRedb>, wing_id: Uuid) -> Result<usize> {
    Ok(store
        .list_rooms()
        .context("list rooms for wing count")?
        .iter()
        .filter(|(_, r)| Uuid::from_bytes(r.wing_id) == wing_id)
        .count())
}

#[cfg(test)]
#[path = "wings_tests.rs"]
mod tests;
