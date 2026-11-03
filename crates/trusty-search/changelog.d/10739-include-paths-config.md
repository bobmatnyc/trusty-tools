Added
- `search.index.config.get` (and `GET /indexes/:id/config`) now report `include_paths` (relative to the index root) and `path_filter`, so a caller can prove from the reply alone which paths an index admits.
- `search.index.config.set` (and `PATCH /indexes/:id/config`) accept `include_paths`; `[]` clears the allowlist. Files the new list refuses are removed in place (`include_paths_purge` in the reply); files it newly admits are indexed by the next reindex. A path outside the root is `400 invalid_include_path`.
- New socket method `search.index.path.admits {index_id, path}` answers `{admitted, reason}` from the daemon's own admission policy.
