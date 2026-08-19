Fixed

- A tmux liveness probe that fails now refuses `tm session delete`, `tm session
  prune`/`decommission`, and `tm session resume` instead of reading the session
  as dead. Previously a transient `list-sessions` failure (or the no-op driver
  installed when tmux cannot be discovered) let an unforced prune tear down a
  live session and let resume kill and rebuild a live pane (#5859).
