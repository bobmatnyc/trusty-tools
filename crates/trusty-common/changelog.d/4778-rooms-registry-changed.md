Changed

- The memory write path resolves a drawer's `room_id` through the new room
  registry instead of hashing a `RoomType`'s `Debug` string, and the room
  filters on `list_drawers` / `retrieve_l2` resolve the same way. Both fall
  back to the legacy fold when a palace has no registry row, so filtering on an
  un-backfilled palace is byte-identical to before. Room selection remains
  caller-supplied-or-`General`: content and tag inference are deliberately not
  implemented, because a mis-inferred room fails invisibly (ADR-0027 D4.4).
- The documented palace model is `Palace -> Wing -> Room -> Drawer`. A *closet*
  is not a hierarchy level — it is the many-to-many keyword -> drawer-ids
  inverted index on `PalaceHandle`, and the field keeps its name (ADR-0027 D3).
- Room-filter resolution is performed before the drawer (and closet) read guards
  are acquired on the `list_drawers` and `retrieve_l2` paths, so the redb read
  transaction no longer runs while a palace lock is held.
