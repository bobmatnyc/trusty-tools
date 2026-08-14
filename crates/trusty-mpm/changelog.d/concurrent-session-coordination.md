Added

- `tm-workflow` now carries a "Concurrent Sessions on One Repo" protocol: use
  `session_list` to find the other session on this repo and `session_activity` to
  read its raw tmux pane, claim work on the tracker before starting, and verify
  every `session_send` with `session_activity`. `session_send` and
  `session_proxy_message` inject characters into a pane and do not deliver a
  message — an observed long single-line send landed as a bracketed paste, the
  submit never fired, and the text sat unsent in the target's prompt buffer. The
  tools existed and were documented nowhere, so every cross-session handoff went
  through the human owner by hand.
