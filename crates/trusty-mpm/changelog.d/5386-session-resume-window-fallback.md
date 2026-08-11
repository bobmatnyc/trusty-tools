Added

- `session_context_catchup` takes an optional `tmux_window` and resolves a
  snapshot by the caller's tmux window id when `session_id` owns none. A Claude
  Code relaunch mints a new harness session id inside the same window, which
  left `resolved_snapshot` null and pushed `/tm-session-resume` back to a human
  guessing which snapshot was theirs. The response also carries `resolved_via`
  (`session_id` / `tmux_window` / null) so a window match is never read as an
  exact one.
