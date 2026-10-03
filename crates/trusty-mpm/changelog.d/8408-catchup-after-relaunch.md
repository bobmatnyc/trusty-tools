Fixed
- `session_context_catchup` called with only `tmux_window` resolves the
  caller's prior snapshot after a relaunch changed the window id. It now
  falls back to the tmux session name and answers
  `resolved_via: "tmux_session"`.
- The `tm-session-resume` skill no longer tells a resumed PM to
  `tmux select-window` the recorded window, which a relaunch has already
  replaced.
