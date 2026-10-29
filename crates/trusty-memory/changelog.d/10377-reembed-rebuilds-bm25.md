Fixed
- `palace_reembed` (non-dry-run) now also rebuilds the palace's BM25 lexical index when the lane is enabled, and reports the result under `bm25` (#10377). A restored palace has no `bm25/` because snapshots leave it out, so recall scores drifted (0.86 before backup, 0.71 after restore) until the next startup or repair sweep.
