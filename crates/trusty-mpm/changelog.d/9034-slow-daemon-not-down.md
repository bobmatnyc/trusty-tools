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
- Guided autostart reads the `state =` line of `launchctl print`. A loaded
  launchd job that is not running (for example after `tm stop`) is now
  started with `launchctl kickstart` instead of being awaited as if it were
  running. The stop message names the host's restart command (#9034).
- Daemon process discovery now reads each process's command line, so
  `tm stop` finds a running daemon again. Autostart deletes `daemon.lock`
  only when the pid it names is dead. A live pid that is not confirmed as a
  tm daemon (another program, or a command line `tm` cannot read) is never
  deleted: bare `tm` stops and tells you to remove the lock if no tm daemon
  is running, then run `tm start`. A spawned daemon that is still starting
  when the 5 s health poll ends is reported as slow, not as down (#9034).
- `tm stop` and the autostart lock check count a process as the daemon only
  when `daemon` is its subcommand, so `tm build-lease -- cargo test daemon`
  or `tm doctor daemon` is never stopped as a daemon. A lock pid owned by
  another user (`kill` answers EPERM) now counts as alive, so its lock is
  kept instead of deleted (#9034).
