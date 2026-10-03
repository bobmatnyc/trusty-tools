Added
- `session_context_catchup` responses carry an optional `resolved_note` when
  `tmux_window` was passed without `tmux_session_created` and nothing resolved.
  It says the tmux-session route was not tried, so a skipped route no longer
  reads as "nothing paused". Every other response omits the key.
