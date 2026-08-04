Added

- Wings are a real, persisted, enumerable scope over rooms (ADR-0027 D2, ticket T9, closes [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - a Wing is the "who" axis (scope/ownership); a Room stays the "what" axis (topic).
    `engineer`/`Planning` and `pm`/`Planning` are now two distinct rooms, with no
    `Custom("engineer-planning")` name mangling
  - new `WINGS` / `WING_KEYS` redb tables in the palace's `kg.db`, initialised in the
    same transaction as `DRAWERS` and `ROOMS` so the schema is never half-present
  - every palace gets a `default` wing, seeded at open. **Wing is never required of a
    caller**: `wing_id` defaults to the default wing on every write and read, so a
    caller that never mentions a wing behaves exactly as it did before
  - existing rooms land in the default wing by NAMING, never reclassification — every
    `RoomRecord` has carried `DEFAULT_WING_ID` since T1, so the migration writes one
    wing row and changes zero drawer rows and zero room rows (proven byte-for-byte by
    `seeding_the_default_wing_changes_no_room_or_drawer_rows`)
  - seeding is insert-only and probes by id, so it is idempotent and a wing renamed
    between palace opens keeps its new name
  - `RecallScope` (`All` / `Room` / `Wing`) plus `retrieve_l2_scoped`, `recall_scoped`,
    and `list_drawers_in_wing` give wing-scoped recall and listing. A wing scope
    resolves **fail-closed** — an unresolvable scope admits nothing rather than
    everything, because a scope boundary that fails open is a leak (the #3064
    "two agent types cannot accidentally read/write the same room" criterion)
  - `resolve_or_create_room_in_wing` lets a write place a room in a named wing;
    without it a non-default wing could never receive a drawer
  - a wing rename reads the row, checks the new label is free, rewrites the row,
    points the new key at it, and retires the old key — all in ONE redb write
    transaction. No crash can leave the retired label resolving as a stale alias,
    and a concurrent `wing_create` cannot claim the new label between the check
    and the write. A rejected rename writes nothing at all
  - `retrieve_l3` / `recall_deep` gained the same `RecallScope` generalisation as
    L2, so `memory_recall_deep` honours a wing instead of ignoring it
