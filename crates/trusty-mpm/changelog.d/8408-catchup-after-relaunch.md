Fixed
- `session_context_catchup` called with `tmux_window` and the new
  `tmux_session_created` (tmux `#{session_created}`) resolves the caller's
  prior snapshot after a relaunch changed the window id. It falls back to the
  tmux session name, only for snapshots paused after that session was
  created, and answers `resolved_via: "tmux_session"`. A fresh `tm-<folder>`
  session reusing the name does not resolve its predecessor's snapshot, and
  without `tmux_session_created` the session-name route resolves nothing.
- The catch-up digest no longer marks every snapshot sharing the caller's tmux
  session name `owned`. The session-name route owns only the one snapshot it
  resolved.
- The `tm-session-resume` skill no longer tells a resumed PM to
  `tmux select-window` the recorded window, which a relaunch has already
  replaced, and now passes `tmux_session_created`.
