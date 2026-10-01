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
  the record only. The refusal is an HTTP 409 whose message gives the reason,
  says whether the record's pane was already signalled, and names the ways
  out: end the session yourself and rerun, or `tm session delete --force <id>`
  to drop the record only.
- A stop no longer snapshots a tmux session it has not proved is the
  record's own: the scrollback is read from the record's own pane, and a
  session that took the name is never written into the stale record's
  workspace.
- `tm session delete` of a stale record whose name a different live session
  now uses no longer needs `--force`; a delete still never touches tmux.
  `tm session delete --force` now succeeds when tmux is absent or its session
  probe fails, and says so.
- The MCP `session_stop` tool and the `sm.sessions.stop` stdio method now
  return `runtime_left_running`: the reason a live session was left running,
  or `null`.
