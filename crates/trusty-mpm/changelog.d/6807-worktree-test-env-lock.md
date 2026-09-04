Fixed

- `session_worktree_path_uses_dot_prefix` now holds `env_test_lock()`. It reads
  `TRUSTY_MPM_WORKTREES_DIRNAME` without taking the lock a sibling test holds to
  set it, so under `cargo test`'s shared process it intermittently observed
  `.sessions` and failed. See #4162.
