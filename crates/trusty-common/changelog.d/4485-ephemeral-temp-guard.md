Fixed

- `bin_resolve::is_ephemeral_build_path` now also rejects any path under a system temp root (`/tmp`, `/private/tmp`, `/var/tmp`, macOS `/var/folders`, and the live `std::env::temp_dir()`), matched as component-wise path prefixes rather than substrings. Previously the guard enumerated only `target/debug`, `target/release` and the two worktree layouts, so a scratch binary under an agent harness's temp scratchpad read as an ordinary installed path and was accepted as stable (#4485).
