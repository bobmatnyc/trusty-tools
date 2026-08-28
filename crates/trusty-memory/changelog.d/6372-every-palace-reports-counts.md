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
