Added

- MCP room surface: `room_list`, `room_create`, and `room_rename` (ADR-0027 T6).
  `room_list` is the discovery primitive the product never had — before it, a
  caller could not learn that a palace had a `decisions` room without already
  knowing the word. `room_create` is idempotent and matches names
  case-insensitively. `room_rename` is the repair path for rooms the migration
  could only name `unresolved-<id>`; it changes a room's NAME and nothing else —
  no drawer is moved, reassigned, or rewritten — and refuses a name already
  owned by another room.
  - `palace_info` gains `room_count`.
  - The `wing` argument is declared on `room_list` / `room_create` so its name is
    stable, and is validated strictly: anything other than the palace's default
    wing is an explicit error rather than a silently ignored argument, because
    wings are not implemented until ADR-0027 T9.
