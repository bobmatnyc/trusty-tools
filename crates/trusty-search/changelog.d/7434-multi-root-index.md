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
- The file watcher covers every root of a multi-root index, not just
  `root_path`. Each root gets its own watch and its own state, so a save under
  an additional root updates that root's own chunks, one root that cannot be
  watched leaves the others running and is recorded rather than only logged,
  and a dropped-event rescan reconciles every root instead of sweeping the
  other roots' files out of the corpus. `POST /indexes/{id}/roots` starts the
  new root's watch in the same request, and a relocate restarts the primary's
  watch while keeping the additional ones. `GET /indexes/{id}/status` gains
  `watcher.roots`, one row per root reporting `watching` / `degraded` /
  `failed` with its reason; the existing `watcher` fields are unchanged
  (#7434).
- MCP tool `add_root` (`index_id`, `roots`) adds directory trees to an existing
  index, and `create_index` accepts the same `roots` list at registration — the
  multi-root surface is reachable from an MCP client rather than only over
  HTTP. Both run the daemon's existing gate, so a tree another index owns is
  still refused. `add_root` requires `roots`: unlike `create_index`'s optional
  filters, a malformed array is an error rather than a dropped field, because
  posting an empty list would report the unchanged root table as a success. The
  index-lifecycle descriptors moved to `mcp/tools/descriptors_lifecycle.rs`,
  which changes no schema (#7434).
