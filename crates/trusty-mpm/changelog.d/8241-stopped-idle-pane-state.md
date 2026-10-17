Fixed
- `tm ls` and `tm session ls --json` no longer report a stopped session as `active` or `attached` when its tmux pane is alive but holds only an idle shell. The runtime reaper leaves that pane alive after the runtime exits, and the listing now applies the same idle-shell test, so `state` agrees with `persisted_state` and MCP `session_list` (#8241).
