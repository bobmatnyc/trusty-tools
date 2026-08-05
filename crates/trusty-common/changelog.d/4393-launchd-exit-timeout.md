Fixed

- Generated LaunchAgent plists now declare `ExitTimeOut`. Without the key
  launchd applies its "system-defined" default, which measures 5 s on macOS —
  SIGTERM, then SIGKILL 5 s later on every `launchctl bootout`, `kickstart -k`,
  logout and reboot. That is shorter than the shutdown work several trusty-*
  daemons do; trusty-search's index flush alone floors at 30 s per index, so it
  was cut off mid-write every time. The rendered window comes from the new
  `shutdown::TERMINATION_GRACE_SECS` (60 s), which `trusty-search stop` and its
  orphan reaper now wait as well, so the window a daemon plans for and the
  window its terminator grants cannot drift apart. **Already-installed agents
  keep launchd's 5 s default until their plist is regenerated** (#4393)
