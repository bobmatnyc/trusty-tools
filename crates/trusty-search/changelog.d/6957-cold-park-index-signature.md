Changed

- **`lazy_loader::cold_park_index` takes a fifth argument.** `reindex_in_flight: impl FnOnce() -> bool` is the late reindex re-check the park now runs immediately before it detaches the handle (#6957). The caller supplies it as a closure so the residency module keeps depending on neither `SearchAppState` nor `ReindexStatus` — the same shape `root_gate::evaluate_root_move` uses for its registry read. A caller that tracks no reindex state passes `|| false` ([#6957](https://github.com/bobmatnyc/trusty-tools/issues/6957))
