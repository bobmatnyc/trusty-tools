Fixed
- The stale-aside sweep's delete branch now logs "deleted stale pre-cargo
  aside litter" instead of reusing the restore branch's "recovered" message,
  so an operator reading the logs can tell a restore from a deletion
  (#5777, trusty-review round on PR #5778).
- `pid_is_alive` converts the aside's pid with `try_from` instead of a bare
  `as` cast: a pid above `i32::MAX` no longer wraps negative — where
  `kill(-n, 0)` would probe a process GROUP — and the (today unreachable)
  overflow maps to `kill(-1, 0)`, which reads as alive and keeps the sweep
  fail-closed (#5778).
