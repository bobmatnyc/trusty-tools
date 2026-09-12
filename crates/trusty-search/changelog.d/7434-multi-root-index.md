Added
- An index can now span several directory trees. `IndexHandle` and the
  persisted `indexes.toml` record carry `additional_roots` beside `root_path`,
  the reindex walk covers every root, files under an additional root are stored
  relative to their own root, the search containment post-filter and the
  root-collision guard both ask any-of-N, and a root that is absent at walk time
  is named in `GET /indexes/:id/status` instead of silently contributing
  nothing. `root_path` stays the primary root: index-id derivation, colocated
  storage and the root-hijack gate are unchanged. Existing `indexes.toml` files
  load with an empty list and need no migration (#7434).
