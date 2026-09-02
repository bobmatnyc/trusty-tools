Fixed

- `tctl restart <service>` waits for launchd to finish the bootout before it
  bootstraps. The Restart arm called `bootout()` then `bootstrap()` back to back;
  because `launchctl bootout` returns when the unload is accepted rather than
  when the job is gone, the bootstrap reached launchd with the label still
  registered and was refused — `trusty-memory: booted out successfully;
  bootstrap failed: … Bootstrap failed: 5: Input/output error` — while a plain
  retry seconds later succeeded (#6618). It now routes through
  `trusty_common::launchd::LaunchdConfig::restart_gracefully`, which quiesces a
  unit whose loaded `ExitTimeOut` is shorter than the daemon's flush (the #6590
  guard `service install` already ran), waits the label out of launchd, and
  retries the bootstrap once after a second wait. A restart that still fails
  names the label, both waits, and launchd's own reason.
- The success line reports the wait it paid and how many bootstrap attempts it
  took, so a race that recurs is visible instead of silently absorbed.
