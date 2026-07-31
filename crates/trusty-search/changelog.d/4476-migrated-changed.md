Changed

- **BREAKING (HTTP API): `DELETE /indexes/:id` no longer destroys on-disk data
  by default** ([#4123](https://github.com/bobmatnyc/trusty-tools/issues/4123)).
  The handler hardcoded `delete_data=true`, so every `DELETE` destroyed the
  index's data directory and the HTTP surface offered NO way to merely
  deregister. Registry hygiene was therefore impossible through the API: an
  operator clearing 49 stale entries had to stop the daemon and hand-edit
  `indexes.toml`, because one mis-typed id would have destroyed a real corpus.
  A bare `DELETE` now deregisters only (the same safe path the orphan-reaper
  has always used); destroying data requires an explicit
  `?delete_data=true`. The response gained a `data_deleted` field so callers
  can confirm which semantics ran. An unparseable `delete_data` value is
  rejected with `400` rather than guessed at.

  This also makes three long-standing pieces of documentation true rather than
  false — all of them already promised the preserving behaviour that did not
  exist: the API reference (`crates/trusty-search/CLAUDE.md`, "On-disk redb data
  is preserved"), `trusty-search index remove`'s `--help` ("The on-disk redb /
  HNSW snapshot is preserved — re-registering with the same path reuses it"),
  and the UI's delete confirmation ("On-disk data is preserved.").

  **Action required for callers that relied on `DELETE` reclaiming disk** —
  they will silently stop reclaiming it and leave orphaned data behind. The
  in-tree ones are updated in this change to pass `?delete_data=true`: the
  `delete_index` MCP tool (its descriptor promises "and all its data"),
  `trusty-search cleanup`, trusty-mpm's decommission + orphan-sweep index GC,
  and the benchmark harnesses that require a clean slate. `trusty-search index
  remove` and the UI are deliberately left on the new preserving default,
  because that is what they already told users they did.
