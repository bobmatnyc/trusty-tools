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
- A `SessionStart` that settles an id as unproven while the registry file
  cannot be written (for example, a full disk) still closes the id to a
  later announcer for the daemon's lifetime, and the failed save is logged
  as a warning. A repair refused because the registry is sealed now says so,
  instead of saying the owner never announced its `claude` (#8531).
- Known limits. Three cases leave an owner unable to clear its own records
  until the 6 h stale or owner-gone repair path ends them: sessions already
  running at the first daemon start on this version, which has no registry
  file yet; a session whose first `SessionStart` fell back to HTTP, which
  stays unproven; and a resumed session (`claude --resume <id>`, which tm
  uses to relaunch), whose id is settled while its earlier `claude` has
  exited. Separately, a session id whose `SessionStart` never reached the daemon (daemon
  down at launch, project missing the tm `SessionStart` hook — see the
  `hooks_missing_tm_group` doctor check — or a session started before the
  upgrade) can be claimed by a sibling that knows the id and announces it
  first (#8531).
