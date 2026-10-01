Fixed

- Stopping, deleting, pruning or decommissioning a stale session record no
  longer kills a live tmux session that has since taken its name. The
  teardown now signals and kills only a session that still holds the record's
  own tmux pane (`%N` id), and signals that pane's `claude`, not the
  session's active pane — the Ctrl-C fallback, used when no `claude` pid is
  found, goes to that pane too. When another session holds the name, the
  record moves and nothing is killed. `tm session stop`, `tm session delete`
  and the `tm ls` delete say "record only" and name the session they left
  running.
- When tmux cannot prove whose the live session is — the record has no pane
  id, or the pane list or the session probe fails — decommission now refuses
  and leaves the workspace and the record as they were, and the idle reaper
  skips the session instead of marking it stopped. A plain stop still moves
  the record only.
- `tm session delete` of a stale record whose name a different live session
  now uses no longer needs `--force`; a delete still never touches tmux.
  `tm session delete --force` now succeeds when tmux is absent or its session
  probe fails, and says so.
- The MCP `session_stop` tool and the `sm.sessions.stop` stdio method now
  return `runtime_left_running`: the reason a live session was left running,
  or `null`.
