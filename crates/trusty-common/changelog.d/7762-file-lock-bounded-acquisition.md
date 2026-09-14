Fixed

- `file_lock` acquisition is bounded instead of blocking forever. A holder that
  is wedged rather than dead — SIGSTOP'd, stopped in a debugger, blocked on a
  network-mounted `$HOME` — used to hang every writer of the guarded file with
  no output; acquisition now retries up to `DEFAULT_LOCK_TIMEOUT` (10s) and then
  fails with an `io::ErrorKind::TimedOut` error wrapping the new `LockTimeout`,
  which names the sidecar and, when the sidecar records one, the holder's pid.
  New `with_exclusive_lock_timeout(path, timeout, f)` takes the bound from the
  caller for non-interactive writers; `with_exclusive_lock` delegates to it at
  the default, so no caller needs a change (#7762).
