Fixed

- `console_metrics` now reports real drawer, vector, room and KG-triple counts
  for every palace on disk, not only the ones resident in the open-handle LRU
  cache. On a host with 94 palaces and 2 resident the dashboard showed 92 rows
  of zeros, which reads as "those palaces are empty". A closed palace is counted
  by reading four redb B-tree lengths under a shared lock — no palace is opened,
  so #1924's cache-thrashing fix stands. A palace whose files cannot be read
  (another process holds them) reports null counts with a reason rather than a
  zero (#6372)
- `console_metrics` now carries per-palace `room_count` and a `total_rooms`
  aggregate. Rooms were absent from the payload entirely, so the dashboard had
  nothing to render (#6372)

Changed

- The `console_metrics` payload adds `counted_palace_count`, `total_rooms`, and
  per-palace `room_count`, `stats_source` (`cache` / `disk` / `unavailable`) and
  `stats_error`; `metrics_schema_version` is now 3. `cached_palace_count` and
  the per-palace `cached` flag keep their meaning — how many palaces were
  resident — so an older console still reads the report (#6372)
