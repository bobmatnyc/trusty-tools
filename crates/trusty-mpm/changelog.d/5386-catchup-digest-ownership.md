Fixed

- `session_context_catchup` returned every paused session's `source_file` and
  `tmux_window` in `sessions[]` to any caller, regardless of `session_id`. A
  caller could read another session's window out of the response, hand it back
  as its own, and resolve that session's snapshot — reconstructing by hand the
  cross-session resume #5272 removed. A caller that does not own a session now
  gets `format`, `paused_at`, `summary` and `owned: false`; `source_file`,
  `tmux_window`, `in_progress`, `next_steps` and `git_context` are withheld.
  The `tm session catchup` CLI digest is unchanged.
