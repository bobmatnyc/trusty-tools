Changed

- `KgStoreRedb::count_active_triples` and `KnowledgeGraph::count_active_triples` return `Result` instead of failing open to `0` ([#5384](https://github.com/bobmatnyc/trusty-tools/issues/5384))
  - `begin_read`, `open_table`, `iter`, and a per-row read each logged a warning and returned `0`, so a caller could not tell an empty graph from a storage read that never completed. `open_table` cannot mean "not written yet" here — `open_with_intent` creates every table on open — so the failure is always real.
  - A dropped-row read no longer `continue`s past the error and undercounts.
  - Breaking for callers that consumed the bare `u64` / `usize`; `trusty-memory`'s call sites are updated in the same change.
