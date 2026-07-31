Fixed

- two `tools::tests` no longer depend on an environment variable another test
  happened to leak (closes [#4413](https://github.com/bobmatnyc/trusty-tools/issues/4413); refs [#4407](https://github.com/bobmatnyc/trusty-tools/issues/4407), [#3451](https://github.com/bobmatnyc/trusty-tools/issues/3451)).
  `add_alias_round_trip_through_prompt_cache` and
  `dispatch_discover_aliases_inserts_new_and_dedupes` build `AppState` inline
  (they need `with_default_palace`) and so never ran `test_state()`'s
  `TRUSTY_SKIP_PALACE_ENFORCEMENT` write — they passed under `cargo test` only
  because a sibling test in the same process had set it first. In isolation they
  verified nothing, failing at `palace_create` before reaching any assertion.
  Both now call a named `skip_palace_enforcement()` helper, which
  `test_state`/`test_state_warming` share. `cargo nextest run -p trusty-memory`
  (per-test process isolation) goes from 2 failures to 527/527 passing.
