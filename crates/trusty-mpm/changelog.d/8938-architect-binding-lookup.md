Fixed

- `tm fleet status`, run from inside the bound Architect's own session, now
  reports `ok binding` and `ok this_session` instead of `UNBOUND`. The
  `claude` lookup under a tmux pane reads its child list from the process
  table instead of `pgrep -P`, which on macOS drops its own ancestors. The
  `this_session` check walks up to three process hops to the `claude` above
  the Bash tool's shell; the pm-guard hook keeps its one-hop walk.
