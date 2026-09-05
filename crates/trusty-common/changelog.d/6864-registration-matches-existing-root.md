Fixed
- `ensure_project_indexed_reporting` no longer fails a session when the derived
  index id already identifies another tree. On a `409` — or a
  `200 {created:false}` naming a different root — it reads
  `GET /indexes?details=true` and pins the index whose `root_path` IS the
  requested project, and registers under `derive_checkout_index_id` when nothing
  serves it. Two checkouts of one repository used to leave the second unpinned,
  so every MCP `search` in that session answered `missing required string field:
  index_id` (#6864).
