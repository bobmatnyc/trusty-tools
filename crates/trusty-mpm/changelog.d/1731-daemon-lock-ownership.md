Fixed

- `~/.trusty-mpm/daemon.lock` is no longer deleted by a process that does not
  own it (closes [#1731](https://github.com/bobmatnyc/trusty-tools/issues/1731)).
  The daemon has written the file at startup since #1731 first closed, but the
  `daemon::lock` unit test called `write_lock` then `remove_lock` against the
  real path to prove they do not panic — so every `cargo test -p trusty-mpm` run
  overwrote a live daemon's record with the test process's PID and then removed
  it, leaving a running daemon with no lock file. Removal is now
  ownership-checked in both the daemon's shutdown handler and `tm stop`, so an
  outgoing daemon cannot delete a successor's record either, and that test is
  hermetic.
