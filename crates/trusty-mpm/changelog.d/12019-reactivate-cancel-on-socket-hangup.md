Fixed

- An operator abort during an in-place reactivate over the daemon socket now
  rolls the session record back off `Active` in the route itself, instead of
  leaving it `Active` until the runtime reaper marks it stopped.
  `mpm.managed.reactivate` is marked cancel-on-hangup, and the mark and its
  rollback task now start in one task so no drop point can strand the record.
  The rollback for a departed caller no longer runs inside a `tracing::warn!`
  argument, where a disabled WARN level skipped it.
- The rollback runs as soon as the caller goes, not when the index steps end,
  so the runtime reaper never records a death for an aborted reactivate. A
  rollback only undoes its own attempt: a later reactivate of the same record
  is left `Active`.
- `tm` prints one line before an in-place reactivate that may wait for the
  daemon to restore the project index, so a long wait does not look like a
  hang (Refs #12019).
