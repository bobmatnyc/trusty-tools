Fixed

- A forged `SessionEnd` no longer releases another session's live delegation
  records. The daemon stales a session's live records on `SessionEnd` only
  when the event arrives over the daemon socket from under that session's own
  bound `claude`; an HTTP `SessionEnd`, or one from any other process, stales
  nothing. `tm hook SessionEnd` now reaches the daemon over its socket. A
  session that was never bound, or was resumed with `claude --resume`, keeps
  its records until the 6 h stale path or `tm repair delegation` ends them
  (#8980).
- `PATCH /sessions/{id}/pid` and `mpm.sessions.set_pid` refuse a session that
  a `SessionStart` announced or that owns delegation records. A dead pid
  written there made the session read as gone, so a sibling could end its
  live records. A launcher's own session still accepts its pid (#8980).
- The session reaper no longer removes a session that a `SessionStart`
  announced, or stales its live delegations. Such a record's tmux name is
  never live, so a forged `SessionStart`, or a `compact`/`clear`/`resume`
  one, used to stale the session's own agents within a minute. While the
  session-claude registry is sealed the reaper skips every session (#8980).
