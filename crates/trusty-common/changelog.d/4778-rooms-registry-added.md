Added

- Rooms are a real, persisted, enumerable entity (ADR-0027 T1/T2/T4). Two new
  redb tables in each palace's `kg.db` — `rooms` and `room_keys` — give every
  room a durable id, a first-seen label, and a canonical `(wing, label)` key.
  Ids are now READ from that table rather than recomputed: legacy rooms keep
  the exact `room_to_uuid` value already stamped on their drawers, and new
  rooms mint a UUIDv5, which removes the structured-collision class the old
  16-byte fold admitted for any custom room name of 9+ characters.
  - An additive backfill runs at palace open and registers a row for every
    room the palace's drawers already use. It is insert-only, so it is
    idempotent and a later rename survives every reopen; it is fail-open, so a
    registry problem degrades room listing rather than blocking the palace; and
    it is per-palace on demand, with no sweep across the whole data root.
    **It writes zero `DRAWERS` rows** — existing drawers are named, never
    reclassified or moved.
  - Labels are recovered in four steps: a rainbow table over the nine built-in
    variants, direct inversion of the fold for short labels, a dictionary
    match against the palace's own KG `room:` subjects, and a non-lossy
    `unresolved-<first8>` fallback flagged `resolved: false` for a human to
    rename.
  - `wing_id` is reserved on every room row and defaults to a per-palace
    `DEFAULT_WING_ID`. The Wing entity itself is not implemented (ADR-0027 T9,
    gated on its #3064 consumer).
