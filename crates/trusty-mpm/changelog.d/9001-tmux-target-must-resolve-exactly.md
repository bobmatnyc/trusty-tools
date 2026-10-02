Fixed

- `tm hook --pm-guard` refuses a tmux `send-keys`, `kill-*`, `respawn-*` or other
  pane-changing command whose `-t`/`-s` target does not resolve exactly to an
  existing session, window or pane. tmux matches an unknown session name by
  prefix, so `-t nosuch:0` could reach a live `nosuch-…` session. A prefix-only
  name, a missing target, a target the shell expands, and a server the guard
  cannot list (no tmux, a query error) are all refused, naming the target. The
  floor binds every caller under every bypass. Read verbs such as
  `capture-pane` and `has-session` are not affected.
- The same floor refuses a tmux command it cannot read, with or without a live
  Architect, and names the token or the reason: a `TMUX`/`TMUX_TMPDIR` change,
  `sudo`, `env -i`, `exec -c`, a relative `-S` socket, a program word the shell
  expands (`T=tmux; $T …`), and an option it does not know.
- A tmux command after shell grammar is now found: `{ …; }`, `( … )`, `!`,
  `if`/`then`/`else`, `for`/`while`/`until … do`, `case` arms, and a function
  body (`f() { tmux …; }`). `kill-session -a -t X` now checks `X`.
- A pane-position word (`top`, `bottom-left`, …, any case) is no exact target
  for a pane verb, because tmux reads it as a pane of the caller's window.
- The guard's tmux listing gives up after 2 seconds and refuses, so a stopped
  tmux server no longer hangs the hook.
