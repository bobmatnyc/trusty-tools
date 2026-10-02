Fixed
- A session record whose tmux pane id was captured before a tmux server
  restart no longer sends keys to, reads, or resumes into the live pane that
  reuses its id. `tm session send`, inject, answer, observe, the activity and
  idle-reaper captures, and resume now act on a pane only when its tmux
  server matches the one the record captured it on; any other answer refuses
  and touches no pane (#9101).
- A record with no pane id, or with no tmux server identity (written before
  #9004), now refuses those operations instead of falling back to the tmux
  session's active pane. The record becomes usable again once its tmux
  session ends and `tm session resume` creates a new one, which captures the
  pane with its server (#9101).
- Daemon shutdown now stops only managed sessions it proves it owns: it
  signals each owned pane, waits the grace window, re-checks ownership and
  kills by `$N` session id. Any other session is skipped and logged, and
  legacy-registry sessions, which carry only a name, are left running
  (#9101).
- `tm session resume` no longer kills a session by name before it recreates
  the pane (#9101).
