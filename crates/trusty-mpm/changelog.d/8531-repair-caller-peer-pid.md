Fixed

- `tm repair delegation` no longer trusts a caller-written session id. The
  owning session's right to clear its own live record is granted only over
  the daemon socket, when the kernel-reported peer pid runs under that
  session's own `claude` process. The `x-tm-caller-session` header and the
  socket `caller_session` param are ignored, so a sibling session can no
  longer impersonate the owner. A caller whose process, ancestry or owner
  process cannot be read is refused (#8531).
- The owner's `claude` is no longer found by the session's tmux name or its
  `pid` field. A sibling could squat the tmux name or set the pid through
  `PATCH /sessions/{id}/pid`. `tm hook SessionStart` now reaches the daemon
  over its socket, and the daemon binds the session to the `claude` above the
  hook process, by pid and start time, once. A session announced over HTTP,
  or one that already owns delegation records, stays unbound and cannot
  clear its own live records (#8531).
- A daemon restart no longer lets a sibling claim a live session's id. The
  first `SessionStart` of each session id is kept in
  `~/.trusty-mpm/session-claudes.json` (mode `0600`, replaced atomically), so
  a later `SessionStart` naming the same id never rebinds it, and the owner is
  still granted after a restart while its `claude` (same pid and start time)
  runs. A binding whose `claude` has exited grants nothing. An id whose first
  `SessionStart` arrived over HTTP or could not be bound is never bound. An
  unreadable, unparseable or foreign-owned file, or one other users can
  access, makes the daemon grant no owner until it is fixed or removed
  (#8531).
- A repair or `SessionStart` whose peer pid names a process started after
  the connection was accepted, which is a reused pid, is refused. The accept
  instant is stamped before the request is read (#8531).
- `tm repair delegation` now always reaches the daemon over its socket. The
  HTTP repair routes still end records whose owner is gone, stale or forced,
  but never establish an owner (#8531).
