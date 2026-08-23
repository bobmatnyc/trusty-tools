Fixed

- `PalaceHandle` now records when its drawer table loaded incompletely (#6201).
  When `open_with_intent` fails open on drawer-load trouble the palace still
  opens, but the returned handle was previously indistinguishable from a
  genuinely-empty or fully-loaded palace, so a caller had no signal it was
  missing corpus. The new `PalaceHandle::drawer_load_degraded` field is `true`
  whenever any drawer row failed to load — both the whole-table Err path (empty
  fallback) and the more common per-row-skip path, where `load_drawers` drops an
  undecodable row (bad key, malformed value, invalid room_id/timestamp) and
  returns the rest. `KnowledgeGraph::load_drawers_with_skipped` threads the
  skipped-row count out for the open to see; `load_drawers` keeps its bare
  `Vec<Drawer>` return, and the existing per-row and whole-table `warn!` logs are
  kept.
