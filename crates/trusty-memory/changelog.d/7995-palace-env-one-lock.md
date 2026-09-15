Fixed

- The `messaging` slug tests now take the crate-wide `commands::env_test_lock`
  alongside `#[serial]`, so `TRUSTY_MEMORY_PALACE` is guarded by one lock
  instead of two disjoint ones and `cwd_palace_slug_at_env_override_wins` can
  no longer lose the override to a concurrent `remove_var` (#7995).
