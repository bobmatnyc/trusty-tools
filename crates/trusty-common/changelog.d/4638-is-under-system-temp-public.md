Changed

- **`bin_resolve::is_under_system_temp` is now public (issue #4638).** Promoted
  from a private helper of `is_ephemeral_build_path` so trusty-code's turn
  recorder can ask the one question it needs — "is this project root under a
  system temp root?" — without duplicating the #4485 temp-root prefix,
  `TMPDIR`, and macOS symlink-alias logic. Behavior is unchanged; this is a
  visibility change only.
  - Callers deciding whether a PROJECT root is durable must use this rather
    than `is_ephemeral_build_path`, whose `EPHEMERAL_PATH_SEGMENTS` half also
    flags `.claude/worktrees/` — an ephemeral BINARY location but a durable
    project checkout that carries the repo's git remote. A new test
    (`is_under_system_temp_is_true_for_temp_and_false_for_worktrees`) pins that
    distinction so the two predicates cannot quietly converge.
