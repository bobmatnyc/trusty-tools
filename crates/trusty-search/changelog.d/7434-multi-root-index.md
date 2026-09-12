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
- `POST /indexes/{id}/roots` adds a directory tree to an existing index, and
  `POST /indexes` accepts the same list as `roots` at creation. Both run one
  gate: each path is canonicalised, the index's own roots are an idempotent
  no-op, duplicates within a request collapse, and a tree already covered by
  another index — as its primary root or one of its additional ones, live or
  cold-parked — is refused `409`. The add holds the per-index reindex lock for
  the whole read-append-persist-swap, so concurrent adds both land, and it
  queues a background reindex so the new tree is walked (#7434).
