Fixed

- `tm hook --pm-guard` refuses a tmux `send-keys`, `kill-*`, `respawn-*` or other
  pane-changing command whose `-t`/`-s` target does not resolve exactly to an
  existing session, window or pane. tmux matches an unknown session name by
  prefix, so `-t nosuch:0` could reach a live `nosuch-…` session. A prefix-only
  name, a missing target, a target the shell expands, and a server the guard
  cannot list (no tmux, a query error) are all refused, naming the target. The
  floor binds every caller under every bypass. Read verbs such as
  `capture-pane` and `has-session` are not affected.
