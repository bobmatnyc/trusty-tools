Added

- Reindex now reports a per-stage wall-clock breakdown — hash-cache load, corpus
  carryover copy, batch pipeline, prune, HNSW commit, corpus commit, KG, and an
  explicit unattributed remainder — in the `reindex phase timings` log line and
  in the `complete` SSE event's `timings` object. This replaces the derived
  `model_load_approx_ms` residual, which folded five distinct stages into one
  number computed by subtraction, leaving the cost of a corpus carryover copy
  unmeasurable. Adds `tests/reindex_stage_profile.rs`, an `#[ignore]`d harness
  that prints cold and warm breakdowns against a throwaway temp corpus (#5024).
