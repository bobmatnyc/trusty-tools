Fixed

- `search_index`'s two mutating entry points (`ensure_project_indexed`, `index_files_best_effort`) no longer reach a live trusty-search daemon from a `cargo test` process, so a `tempfile` fixture root can no longer be registered in the operator's `indexes.toml` (closes [#4255](https://github.com/bobmatnyc/trusty-tools/issues/4255))
  - new `test_harness::running_under_test_harness()` detects a cargo test binary at runtime, which — unlike `cfg(test)` — also covers `tests/` and `[[bin]]` targets and the cross-process case where the write lands in a different process
  - `TRUSTY_ALLOW_PRODUCTION_STATE=1` is the explicit opt-in for a test that deliberately drives a real daemon; `TRUSTY_TEST_HARNESS=1` forces detection on for a child process a test spawns
  - reads are unaffected — only the writes are gated
