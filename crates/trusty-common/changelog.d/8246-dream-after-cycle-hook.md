Added
- `Dreamer::with_after_cycle` installs a callback that runs after every dream cycle that claimed its palace: with `Some(DreamStats)` when the cycle completed, and with `None` when it failed. A failed cycle can still have persisted dedup merges. trusty-memory uses the callback to queue BM25 repair after a cycle that merged or consolidated drawers, or failed (#8246).
