Added

- `tm doctor` has a `session_claudes` row. It warns when
  `~/.trusty-mpm/session-claudes.json` is unreadable, corrupt, owned by
  another user, or open to other users. Such a file seals the session-claude
  registry: no session may repair its own delegation records, `set_pid` is
  refused, and the reaper skips every session. The row says to fix or remove
  the file and restart the daemon, which stays sealed until restart (#8980).
