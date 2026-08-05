Fixed

- No test process can resolve the operator's live data directory any more, so the test suite can no longer register fixture roots in `indexes.toml` (closes [#4255](https://github.com/bobmatnyc/trusty-tools/issues/4255))
  - issue #4094 guarded this with `#[cfg(test)]`, which is set per compilation unit: the `tests/` integration targets and the `[[bin]]` unit tests link the library built without it and kept resolving the real location. `default_data_dir` now branches on the runtime check `trusty_common::running_under_test_harness()` instead
  - `tests/registry_isolation.rs` proves it from the non-`cfg(test)` linkage the old guard missed: it performs a real `upsert_index_registry_entry` and asserts the operator's `indexes.toml` is byte-identical afterwards
  - `TRUSTY_DATA_DIR` still wins over both branches, so tests that set it are unchanged
