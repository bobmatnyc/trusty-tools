Added

- `memory_note` accepts an explicit `room` (ADR-0027 T5). It was hard-pinned to
  `General`, so a curated fact could not be filed anywhere else. The argument is
  optional, goes through the same single `RoomType::parse` as `memory_remember`,
  and still defaults to `General` — a caller that passes nothing is unaffected.
