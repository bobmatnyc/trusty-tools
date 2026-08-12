Fixed

- `POST /indexes/{id}/index-file` and `POST /indexes/{id}/remove-file` now load a
  cold-parked index and apply the write, instead of answering
  `503 index_not_resident` and pointing the caller at
  `POST /indexes/{id}/search` to warm the index as a side effect (#5349). Both
  handlers route through the same resolve-or-lazy-load function `search` uses, so
  a write and a read against identical daemon state can no longer disagree about
  whether an index is reachable — which mattered most for network-mounted roots
  (#3408), where these two endpoints are the only thing keeping the index
  current. A load that genuinely fails still refuses the write: the caller gets
  the residency verdict (`index_restore_failed` / `index_loading` / `404`), never
  a `200` for a write no index received. `index_not_resident` is now unreachable
  from these two endpoints, so a caller branching on it should branch on
  `index_restore_failed` instead; `status`, `chunks`, and `grep` still report it
  and still name `search` as the way to clear it.
