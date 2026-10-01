Added

- `tm doctor` has a `session_claudes` row. It warns when
  `~/.trusty-mpm/session-claudes.json` is unreadable, corrupt, owned by
  another user, or open to other users. Such a file seals the session-claude
  registry: no session may repair its own delegation records, `set_pid` is
  refused, and the reaper skips every session. The row says to fix or remove
  the file and restart the daemon, which stays sealed until restart (#8980).
- The daemon's doctor route reports the running daemon's own seal in the
  `session_claudes` row, so a file fixed or removed without a restart no
  longer reads `Ok` while the daemon stays sealed. The daemonless `tm doctor`
  fallback still reads only the file, and its `Ok` text says so (#8980).
