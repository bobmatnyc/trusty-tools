Added
- `disk_survey` attributes every worktree to a session and can roll usage up by
  it: each row gains `owning_session` (the live claim, else the session the
  `.trusty-mpm-worktree` sentinel names, so an ENDED session's leftovers are no
  longer unattributed) and `build_dir_bytes` (its `target*` share), and
  `group_by: "session"` adds a `by_session` list sorted by bytes with a null
  bucket for the unattributed. `budget_seconds` is now capped at 55 to stay
  inside the `tm serve --stdio` bridge's 60-second forwarding timeout; a clamped
  request answers with `budget_clamped: true`. (#7313)
