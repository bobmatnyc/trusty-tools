Added

- `tm hook --pm-guard` denies, while a live Architect launch record exists,
  every tmux command from a session other than the Architect's main thread
  that types into, kills, respawns, swaps, joins, breaks, moves, links,
  unlinks or clears the history of the Architect's pane, window or session:
  `send-keys`, `send-prefix`, `paste-buffer`, `pipe-pane`, `kill-pane`,
  `kill-window`, `kill-session`, `kill-server`, `respawn-pane`,
  `respawn-window`, `swap-pane`, `swap-window`, `join-pane`, `move-pane`,
  `break-pane`, `move-window`, `link-window`, `unlink-window`,
  `clear-history` and `new-window -k`, with their aliases and unique
  prefixes. The Architect's pane is resolved from its launch record (the
  pane running the recorded `claude`) and the `tm-architect` session, by
  `=name`, prefix name, `%N`, `@N` and `$N` targets. A target the guard
  cannot resolve — a shell variable, `$(…)`, a glob, a special token, an
  unknown current pane — an unknown tmux command or option, `source-file`
  and control mode are denied too. The rule holds under
  `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`; read verbs
  such as `capture-pane`, and geometry verbs such as `resize-pane` and
  `select-layout`, are not affected (#8902).
