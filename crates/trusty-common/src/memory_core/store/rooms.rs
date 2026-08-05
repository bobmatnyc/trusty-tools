//! Room registry records and the resolve-or-create entry point.
//!
//! Why (ADR-0027 D1): rooms were live in the data but had no registry — they
//! could not be listed, had no durable name, and their id was a lossy fold of a
//! `Debug` string. This module owns the on-disk row shape and the ONE function
//! every writer routes through to turn a `RoomType` into a `room_id`.
//! What: `RoomRecord` (postcard, trailing-optional evolution following the
//! `DrawerRecord` precedent), `RoomSummary` for reads, the schema marker, and
//! `resolve_or_create_room` / `resolve_room_filter_id`.
//! Storage note: the tables live in the palace's `kg.db` beside `DRAWERS`, NOT
//! in a JSON sidecar — the redb open path recreates an unreadable database
//! empty, and a surviving sidecar would then authoritatively describe rooms
//! whose drawers are gone (ADR-0027 D1.1). Rooms and drawers corrupt and
//! recover as one unit.
//! Test: `room_record_round_trip`, `room_record_decodes_under_a_future_field`,
//! `resolve_or_create_is_idempotent`, `filter_id_falls_back_to_legacy_fold`,
//! plus the T6 surface in `surface_tests` (`create_room_is_idempotent`,
//! `rename_changes_no_drawer_rows`).

use crate::memory_core::palace::RoomType;
use crate::memory_core::room_identity::{
    DEFAULT_WING_ID, canonical_room_key, default_wing_key, mint_room_id, room_label, room_to_uuid,
    room_type_from_parts, room_type_tag,
};
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::kg_redb::KgStoreRedb;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Schema version stamped into the `ROOMS` marker row.
///
/// Bump only when the *meaning* of existing rows changes; adding a trailing
/// optional field does not qualify (the decode chain handles that).
pub const ROOM_SCHEMA_VERSION: u32 = 1;

/// On-disk room row.
///
/// Why: field order is load-bearing — postcard is positional, so a new field
/// is APPENDED and old bytes are recovered through a fallback chain exactly as
/// `DrawerRecord` already does twice (`store/kg_redb/types.rs`). Never insert a
/// field in the middle.
/// What: the first-seen display spelling, the kind tag, the owning wing, the
/// creation stamp, whether the label was recovered or synthesised, an optional
/// description, and the forward-compatibility slot for room merging.
/// Test: `room_record_round_trip`, `room_record_decodes_under_a_future_field`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomRecord {
    /// First-seen display spelling, e.g. `"Backend"`, `"decisions"`, `"status"`.
    pub label: String,
    /// Canonical kind tag: one of the nine built-in variant names, or
    /// `"Custom"`. The label carries the custom body; this carries the kind.
    pub room_type: String,
    /// Owning wing. [`DEFAULT_WING_ID`] for every room created before the Wing
    /// entity ships (ADR-0027 D2 / ticket T9).
    pub wing_id: [u8; 16],
    pub created_at_ms: i64,
    /// `false` when the backfill could not recover a label and synthesised
    /// one. Surfaced to callers so a human knows a rename is wanted; never
    /// blocks a read.
    pub resolved: bool,
    pub description: Option<String>,
    /// Forward-compatibility slot for room aliasing (ADR-0027 D5). Empty
    /// today. When non-empty, a room filter matches `id` OR any member.
    pub merged_from: Vec<[u8; 16]>,
}

/// Schema-version marker stored under the nil-UUID key in `ROOMS`.
///
/// Why: a future migration needs to know which shape wrote these rows without
/// probing every row. Note that backfill idempotency does NOT depend on this —
/// it comes from the insert-only write path, which is the stronger guarantee
/// because it also preserves a human rename (ADR-0027 D1.4).
/// What: a one-field postcard record; readers skip the nil key when listing.
/// Test: `backfill_stamps_schema_version_and_skips_marker_row`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomSchemaMarker {
    pub schema_version: u32,
}

/// A decoded room row plus its id — the read-side view.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomSummary {
    pub id: Uuid,
    pub label: String,
    pub room_type: RoomType,
    pub wing_id: Uuid,
    pub created_at_ms: i64,
    pub resolved: bool,
    pub description: Option<String>,
}

impl RoomRecord {
    /// Build a record for `room` in the default wing.
    ///
    /// `resolved` is `true` for a room a caller named explicitly and `false`
    /// only for a backfill fallback label.
    pub fn new(room: &RoomType, created_at_ms: i64, resolved: bool) -> Self {
        Self {
            label: room_label(room),
            room_type: room_type_tag(room).to_string(),
            wing_id: *DEFAULT_WING_ID.as_bytes(),
            created_at_ms,
            resolved,
            description: None,
            merged_from: Vec::new(),
        }
    }

    /// Project this row back into the enum callers use.
    pub fn room_type(&self) -> RoomType {
        room_type_from_parts(&self.room_type, &self.label)
    }

    /// Pair this row with its id for the read surface.
    pub fn summarize(&self, id: Uuid) -> RoomSummary {
        RoomSummary {
            id,
            label: self.label.clone(),
            room_type: self.room_type(),
            wing_id: Uuid::from_bytes(self.wing_id),
            created_at_ms: self.created_at_ms,
            resolved: self.resolved,
            description: self.description.clone(),
        }
    }
}

/// Resolve `room` to the id a filter should compare `Drawer::room_id` against.
///
/// Why: after ADR-0027 a room's id is whatever the `ROOMS` table says — for a
/// legacy room that is the verbatim fold value already stamped on its drawers,
/// and for a new room it is a UUIDv5 no fold could reproduce. A read filter
/// that recomputed the fold would therefore silently return nothing for every
/// room created from now on: an invisible failure, exactly the class this ADR
/// exists to remove.
/// What: point-looks-up the canonical key in `ROOM_KEYS`. On a miss (palace
/// never backfilled, or a filter naming a room that does not exist) or on any
/// read error it falls back to [`room_to_uuid`], which keeps filtering
/// byte-identical to pre-ADR behaviour.
/// Test: `filter_id_falls_back_to_legacy_fold`,
/// `filter_id_prefers_registered_room`.
pub fn resolve_room_filter_id(kg: &KnowledgeGraph, room: &RoomType) -> Uuid {
    match kg.store().lookup_room_id(&default_wing_key(room)) {
        Ok(Some(id)) => id,
        Ok(None) => room_to_uuid(room),
        Err(e) => {
            tracing::warn!("room filter lookup failed, using legacy fold: {e:#}");
            room_to_uuid(room)
        }
    }
}

/// Resolve `room` to a `room_id`, creating its registry row when absent.
///
/// Why (ADR-0027 D4.2): the write path used to stamp
/// `room_id = room_to_uuid(&room)` — a lossy hash with structured collisions
/// and no way to enumerate what it produced. Routing every write through the
/// table is what makes a room nameable, listable, and renameable.
/// What: looks the canonical key up; on a hit returns the stored id verbatim
/// (so a write into a backfilled legacy room lands on exactly the id its
/// existing drawers carry); on a miss mints a UUIDv5 and inserts the row and
/// key in one transaction. Runs the redb work on the blocking pool because
/// callers are async.
/// Fail-open: any registry error is logged at `warn!` and the legacy fold is
/// returned, so a room-registry problem can never fail a memory write — and
/// the resulting drawer stays recoverable by the next backfill (a UUIDv5 would
/// not be).
/// Test: `resolve_or_create_is_idempotent`,
/// `resolve_or_create_reuses_backfilled_legacy_id`.
pub async fn resolve_or_create_room(kg: &KnowledgeGraph, room: &RoomType) -> Uuid {
    let store = kg.store();
    let room = room.clone();
    let fallback = room_to_uuid(&room);
    let joined =
        tokio::task::spawn_blocking(move || resolve_or_create_room_sync(&store, &room)).await;
    match joined {
        Ok(Ok(id)) => id,
        Ok(Err(e)) => {
            tracing::warn!("room resolve failed, using legacy fold id: {e:#}");
            fallback
        }
        Err(e) => {
            tracing::warn!("room resolve join failed, using legacy fold id: {e:#}");
            fallback
        }
    }
}

/// Blocking half of [`resolve_or_create_room`].
///
/// Why: exposed to the crate so the backfill and synchronous callers (CLI
/// migrate paths) share one implementation rather than re-deriving the
/// look-up-then-mint sequence.
/// What: `ROOM_KEYS` lookup, else mint + insert-if-absent. The insert is
/// insert-only for BOTH tables, so a concurrent writer that won the race
/// cannot be clobbered; we re-read the key afterwards to return the winner's
/// id rather than our own.
/// Test: `resolve_or_create_is_idempotent`.
pub fn resolve_or_create_room_sync(store: &Arc<KgStoreRedb>, room: &RoomType) -> Result<Uuid> {
    let key = default_wing_key(room);
    if let Some(id) = store.lookup_room_id(&key)? {
        return Ok(id);
    }
    let id = mint_room_id(&key);
    let record = RoomRecord::new(room, chrono::Utc::now().timestamp_millis(), true);
    store
        .insert_room_if_absent(id, &key, &record)
        .with_context(|| format!("register room {:?}", record.label))?;
    // A racing writer may have claimed the key first; the key is authoritative.
    Ok(store.lookup_room_id(&key)?.unwrap_or(id))
}

/// Every registered room, id-ordered, as the read-side view.
///
/// Why: `room_list` (ADR-0027 D6) is the discovery primitive the product has
/// never had; giving it a typed projection here keeps the MCP handler free of
/// `RoomRecord` decoding details.
/// What: [`KgStoreRedb::list_rooms`] mapped through [`RoomRecord::summarize`].
/// Test: `create_room_is_idempotent`, `rename_updates_label_and_key`.
pub fn list_room_summaries(store: &Arc<KgStoreRedb>) -> Result<Vec<RoomSummary>> {
    Ok(store
        .list_rooms()?
        .into_iter()
        .map(|(id, record)| record.summarize(id))
        .collect())
}

/// Create `room`, or return the room the canonical key already resolves to.
///
/// Why (ADR-0027 D6): `room_create` is documented idempotent — a caller that
/// re-runs it, or two callers racing, must converge on one room rather than
/// minting a second id for the same name. Idempotency comes from the
/// insert-only write path, not from a check-then-write: the loser of a race
/// reads the winner's id back out of `ROOM_KEYS`.
/// What: looks up the canonical key; on a hit returns `(summary, false)`
/// leaving the stored row (and its description) untouched; on a miss mints a
/// UUIDv5, inserts, then re-reads the key so the winner's id is what is
/// returned. `created` reflects whether THIS call wrote the row.
/// Test: `create_room_is_idempotent`,
/// `create_room_returns_the_winner_under_a_race`.
pub fn create_room(
    store: &Arc<KgStoreRedb>,
    room: &RoomType,
    description: Option<String>,
) -> Result<(RoomSummary, bool)> {
    let key = default_wing_key(room);
    if let Some(id) = store.lookup_room_id(&key)?
        && let Some(record) = store.get_room(id)?
    {
        return Ok((record.summarize(id), false));
    }
    let id = mint_room_id(&key);
    let mut record = RoomRecord::new(room, chrono::Utc::now().timestamp_millis(), true);
    record.description = description;
    let inserted = store
        .insert_room_if_absent(id, &key, &record)
        .with_context(|| format!("create room {:?}", record.label))?;
    // A racing writer may have claimed the key first; the key is authoritative.
    let winner = store.lookup_room_id(&key)?.unwrap_or(id);
    match store.get_room(winner)? {
        Some(stored) => Ok((stored.summarize(winner), inserted && winner == id)),
        None => Ok((record.summarize(winner), inserted)),
    }
}

/// Resolve a caller-supplied room selector — a UUID or a label — to a room id.
///
/// Why: `room_rename` takes `room_id | label` because the repair path starts
/// from a `room_list` row a human is reading, and the label is what they can
/// type. Accepting both in one place keeps the MCP handler from growing a
/// second, subtly different parser (ADR-0027 D4.1's lesson).
/// What: a well-formed UUID that has a `ROOMS` row wins; otherwise the string
/// is normalised into a canonical key and looked up in `ROOM_KEYS`. Errors
/// when neither resolves, so a typo can never silently create a room.
/// Test: `selector_resolves_by_id_and_by_label`.
pub fn resolve_room_selector(store: &Arc<KgStoreRedb>, selector: &str) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(selector.trim())
        && store.get_room(id)?.is_some()
    {
        return Ok(id);
    }
    store
        .lookup_room_id(&canonical_room_key(DEFAULT_WING_ID, selector))?
        .ok_or_else(|| anyhow::anyhow!("no room matches {selector:?} in this palace"))
}

/// Rename room `id` to `new_label`, touching `ROOMS` / `ROOM_KEYS` only.
///
/// Why (ADR-0027 D6): this is the repair path for the `unresolved-<first8>`
/// labels the backfill synthesises when it cannot invert a legacy id. It is
/// also the reason a wrong label is cheap: a rename costs a name, whereas
/// re-filing drawers would cost data — so this deliberately never writes to
/// `DRAWERS`, and every drawer keeps the `room_id` it already carries.
/// What: rewrites the row's `label`, re-derives `room_type` from `new_label`
/// through the one parser (`RoomType::parse`, ADR-0027 D4.1) so renaming a
/// room to `Backend` produces the built-in kind rather than a `Custom` body,
/// marks it `resolved`, and moves the canonical key. Fails when the new name
/// is already owned by a different room — merging is D5 and is deferred.
/// Test: `rename_changes_no_drawer_rows`, `rename_updates_label_and_key`,
/// `rename_rejects_a_key_owned_by_another_room`.
pub fn rename_room(store: &Arc<KgStoreRedb>, id: Uuid, new_label: &str) -> Result<RoomSummary> {
    let label = new_label.trim();
    if label.is_empty() {
        anyhow::bail!("room_rename: new_label must be non-empty");
    }
    let existing = store
        .get_room(id)?
        .ok_or_else(|| anyhow::anyhow!("no room row for {id}"))?;
    let old_key = canonical_room_key(DEFAULT_WING_ID, &existing.label);
    let new_key = canonical_room_key(DEFAULT_WING_ID, label);
    let record = RoomRecord {
        label: label.to_string(),
        room_type: room_type_tag(&RoomType::parse(label)).to_string(),
        resolved: true,
        ..existing
    };
    store
        .rename_room(id, &old_key, &new_key, &record)
        .with_context(|| format!("rename room {id} to {label:?}"))?;
    Ok(record.summarize(id))
}

#[cfg(test)]
#[path = "rooms_surface_tests.rs"]
mod surface_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::store::kg_store::{decode_value, encode_value};

    /// Simulates the NEXT schema revision: same fields, one appended
    /// trailing optional. Proves the `DrawerRecord` evolution pattern applies.
    #[derive(Debug, Serialize, Deserialize)]
    struct FutureRoomRecord {
        label: String,
        room_type: String,
        wing_id: [u8; 16],
        created_at_ms: i64,
        resolved: bool,
        description: Option<String>,
        merged_from: Vec<[u8; 16]>,
        /// The hypothetical new field.
        owner: Option<String>,
    }

    fn sample() -> RoomRecord {
        RoomRecord::new(
            &RoomType::Custom("status".to_string()),
            1_700_000_000_000,
            true,
        )
    }

    #[test]
    fn room_record_round_trip() {
        let r = sample();
        let bytes = encode_value(&r).expect("encode");
        let back: RoomRecord = decode_value(&bytes).expect("decode");
        assert_eq!(r, back);
        assert_eq!(back.label, "status");
        assert_eq!(back.room_type, "Custom");
        assert_eq!(back.room_type(), RoomType::Custom("status".to_string()));
        assert_eq!(Uuid::from_bytes(back.wing_id), DEFAULT_WING_ID);
    }

    #[test]
    fn room_record_decodes_under_a_future_field() {
        // A row written by THIS version must survive a future revision that
        // appends a trailing optional — via the same fallback chain
        // `DrawerRecord` uses, not via postcard magic (postcard is positional,
        // so the naive decode must and does fail).
        let bytes = encode_value(&sample()).expect("encode");
        assert!(
            decode_value::<FutureRoomRecord>(&bytes).is_err(),
            "postcard is positional: the naive future decode must fail"
        );
        let migrated: RoomRecord = decode_value(&bytes).expect("fallback to current shape");
        let lifted = FutureRoomRecord {
            label: migrated.label,
            room_type: migrated.room_type,
            wing_id: migrated.wing_id,
            created_at_ms: migrated.created_at_ms,
            resolved: migrated.resolved,
            description: migrated.description,
            merged_from: migrated.merged_from,
            owner: None,
        };
        assert_eq!(lifted.label, "status");
        assert!(lifted.owner.is_none());
    }

    #[test]
    fn summary_projects_id_and_type() {
        let id = Uuid::from_u128(7);
        let s = sample().summarize(id);
        assert_eq!(s.id, id);
        assert_eq!(s.room_type, RoomType::Custom("status".to_string()));
        assert_eq!(s.wing_id, DEFAULT_WING_ID);
        assert!(s.resolved);
    }
}
