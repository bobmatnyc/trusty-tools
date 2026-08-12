Fixed

- `standalone::trust_seed::preseed_managed_trust` now holds
  `core::claude_json_guard::lock()` across its whole read → mutate → write
  cycle. It was the third `.claude.json` seeder with the unguarded cycle #4072
  fixed for the other two, and the daemon calls it once per session it
  provisions: two overlapping cycles silently dropped one session's whole
  `projects.<workspace>` entry, so the operator met the trust dialog the seed
  exists to dismiss while both writes reported success. A concurrent seed of
  320 distinct workspaces lost 278 of them before the fix and none after
  (#4076). `mcp_config::seed_builtin_servers` already held the guard against
  the same `<claude_config_dir>/.claude.json`, so leaving this seeder out also
  left those two racing each other.
- `core::claude_json_guard`'s module doc no longer claims that a cross-process
  race "can only ever lose an update, never leave a torn file behind". That
  was false until `trusty-common`'s `write_json_atomic` stopped sharing one
  staging filename (#4077); it is true now, and the doc says which change
  makes it so.
