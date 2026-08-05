Added

- Room registry surface backing the new MCP room tools (ADR-0027 T6):
  `create_room` (idempotent — a racing second caller reads the winner's id back
  out of `ROOM_KEYS` rather than minting a second room), `rename_room`,
  `list_room_summaries`, `resolve_room_selector` (a room id or a
  case-insensitive label), and `parse_room_preserving_case`, which classifies a
  caller-typed name through the one `RoomType::parse` while keeping the
  capitalisation a human chose for the stored label.
  - `rename_room` is the ONLY code in the room registry that rewrites a row.
    It touches `ROOMS` / `ROOM_KEYS` only and provably changes zero `DRAWERS`
    bytes; it refuses to take a name already owned by another room, because
    merging two rooms is ADR-0027 D5 and is deliberately deferred.
