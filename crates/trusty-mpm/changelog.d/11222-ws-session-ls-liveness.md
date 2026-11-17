Fixed
- `tm session ls` and `tm session resume` read a session's liveness on its own `tm-ws-*` workspace tmux server, so a live per-workspace session no longer reads `stopped` (#11222).
