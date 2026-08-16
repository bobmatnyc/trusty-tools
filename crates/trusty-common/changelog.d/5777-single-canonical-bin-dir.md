Changed
- `candidate_bin_dirs` (the install-dir list behind
  `verify_installed_binary`) now returns the single canonical cargo bin dir
  (`$CARGO_HOME/bin`, falling back to `~/.cargo/bin`). `~/.local/bin` is no
  longer consulted as an install destination — with every write path flipped
  onto the canonical dir (#4964 Phase 3), a stale legacy copy there must not
  satisfy the post-upgrade health gate. Legacy copies on `PATH` are still
  found via the `which` fallback (#5777).
