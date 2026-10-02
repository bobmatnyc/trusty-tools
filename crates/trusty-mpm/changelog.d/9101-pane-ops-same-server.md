Fixed
- A session record whose tmux pane id was captured before a tmux server
  restart no longer sends keys to, reads, or resumes into the live pane that
  reuses its id. `tm session send`, inject, answer, observe, the activity and
  idle-reaper captures, and resume now act on a pane only when its tmux
  server matches the one the record captured it on; any other answer refuses
  and touches no pane (#9101).
- A record with no pane id, or with no tmux server identity (written before
  #9004), now refuses those operations instead of falling back to the tmux
  session's active pane. The refusal names the recovery: if the session is
  the record's, end it with `tmux kill-session -t '=<name>'`, then run
  `tm session resume <id>`, which creates a new session and captures its
  pane with the server (#9101).
- The runtime-exit reap no longer binds a record with no pane id to the
  server of whatever session holds its name. It backfills the pane id only,
  so the record stays unproven and the auto-resume that follows refuses
  instead of typing `claude --resume` into that session (#9101).
- Daemon shutdown now stops only managed sessions it proves it owns: it
  signals each owned pane, waits the grace window, re-checks ownership and
  kills by `$N` session id. Any other session is skipped and logged, and
  legacy-registry sessions, which carry only a name, are left running
  (#9101).
- `tm session resume` no longer kills a session by name before it recreates
  the pane, and its create no longer attaches to a session that took the
  name in between: the create is exclusive and refuses a taken name (#9101).
- `POST /claude-config/restart`, the `--task` injection readiness poll, and
  both resume spawns (`tm session resume` and the auto-resume relaunch) now
  act on a record's pane only once it is proven on the live tmux server. Any
  other answer refuses and touches no pane; the restart route answers 409
  (#9101).
- `tm session send`'s submit check reads back the pane the send was proven
  for, instead of looking the pane up again (#9101).
- `tm session rename` renames only the record when the live session with its
  name is on a restarted tmux server, and refuses when ownership cannot be
  proved, including for a record with no pane id. A live rename proves
  ownership before it takes the store lock and renames the session by its
  `$N` id (#9101).
- The bare-`tm` in-place relaunch no longer reactivates a stale record whose
  pane id the current pane reuses after a tmux server restart; the daemon's
  reactivate answers 409 (#9101).
