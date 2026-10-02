Fixed

- Bare `tm` no longer reads a slow daemon as a down one. A timed-out session
  listing, an HTTP error reply, or a refused connection while `daemon.lock`
  names a live pid for that address now stops with a "not responding" error.
  It no longer auto-starts a second daemon or redirects the pane to a managed
  clone. Only a refused connection with no live pid starts the daemon, and
  only a down daemon that cannot be started takes the offline fallback
  (#9034).
- Guided autostart no longer deletes `~/.trusty-mpm/daemon.lock` while the
  launchd service is loaded or the lock's pid is alive. A `launchctl
  bootstrap` that fails with exit 5 on a loaded service now counts as already
  loaded, so autostart waits for that daemon instead of spawning another
  (#9034).
