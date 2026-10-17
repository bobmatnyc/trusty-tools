Fixed

- `rehydrate_dedupes_concurrent_callers_onto_one_scan` counts rehydrate scans through a `cfg(test)` per-indexer counter instead of asserting on elapsed time (test-only; no runtime change)
