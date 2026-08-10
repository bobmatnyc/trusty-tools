Fixed

- `POST /indexes/:id/reindex` with a `root_path` override no longer makes every
  search return nothing. The override rebuilt the index handle around the same
  indexer, leaving the indexer's own `root_path` on the old value — so the
  absolute `file` on each result was built against the old root while the search
  post-filter (#64/#541) checked it against the new one, dropping 100% of
  matches. Callers saw `results: []` with `stale_index_root: true` on an index
  whose `/status` read `ready` with a full chunk count.
