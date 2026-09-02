Fixed

- `install_and_activate` now verifies the ACTIVE launchd unit's shutdown grace
  before the bootout it performs, and stops the daemon itself when that grace is
  too short (#6590). The corrected 60 s `ExitTimeOut` #4393 added to
  `render_plist` could never govern the shutdown that matters: launchd applies
  the `ExitTimeOut` of the job it has LOADED, and re-reads the plist file only
  at `bootstrap` — which happens AFTER `bootstrap`'s own bootout. A host whose
  loaded unit predated #4393 was therefore still granted launchd's 5 s default,
  so a daemon flushing 50 index snapshots was SIGKILLed mid-write and
  `KeepAlive` respawned the old binary as an orphan holding the port.
- The new `launchd_grace` module reads the window launchd will really grant —
  the loaded job's `exit timeout` first, falling back to the installed plist,
  where an absent `ExitTimeOut` key resolves to the 5 s system default rather
  than to "unknown". When it is shorter than
  `shutdown::TERMINATION_GRACE_SECS`, the daemon is sent SIGTERM directly, which
  launchd does not bound, and waited for; the bootout that follows then unloads
  an already-exited job. An unreadable unit changes nothing, and a quiesce that
  fails falls through to the bootout as before.
