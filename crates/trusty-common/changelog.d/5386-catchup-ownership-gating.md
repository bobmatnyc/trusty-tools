Added

- `catchup::resolve` gained `resolve_snapshot_for_caller`, which resolves a
  paused snapshot by the caller's tmux window id when its `session_id` owns
  none, and `redact_sessions_not_owned_by`, which withholds from a catch-up
  digest every field of a session the caller does not own. `PausedSessionJson`
  carries `owned` to say which is which, and
  `session_log::snapshots_attributed_to` returns every snapshot belonging to one
  session id rather than just the newest.
