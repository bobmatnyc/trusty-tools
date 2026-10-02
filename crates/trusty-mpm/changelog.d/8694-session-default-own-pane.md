Fixed

- `tm issue epic create`, `tm pr open`, `tm issue seed-labels` and
  `tm issue standard` without `--session` now read the tmux session of the
  caller's own pane (`tmux display-message -t "$TMUX_PANE"`). They used to get
  the most recently attached client's session and label work `ws/<other>`.
  When `$TMUX_PANE` is unset or the lookup fails, `epic create` and `pr open`
  refuse and ask for `--session`.
