Added

- The search dashboard's index rows expand to a per-collection indexing
  pipeline: a badge per lane (lexical, semantic, graph) with its counters, an
  embedding pause/resume toggle, and a live feed of the last 200 file changes the
  watcher saw. Pausing stops embedding only — lexical search, the knowledge graph
  and the watcher keep running — and the pause is in-memory, so it clears when
  the daemon restarts; the panel says so. An expanded row polls its status every
  15s and holds one SSE feed; collapsing it stops both (#6524).
- Three `/api/search/…` rows onto the daemon's socket methods:
  `POST /indexes/{id}/embedding/pause` and `.../resume` onto
  `search.index.pause_embedding` / `search.index.resume_embedding`, and
  `GET /indexes/{id}/file-events/stream` onto the `search.index.file_events`
  stream (#6524).
