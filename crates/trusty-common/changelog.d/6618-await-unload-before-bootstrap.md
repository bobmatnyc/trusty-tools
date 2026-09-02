Added

- `launchd_restart` — `await_unload` polls a label out of launchd, and
  `restart_sequence` orders a live bounce around that wait: quiesce, boot out,
  wait, bootstrap, and one retry after a second wait. `launchctl bootout` returns
  when the unload is ACCEPTED, not when the job is gone, so a bootstrap issued
  immediately afterwards raced its own bootout and launchd refused it with
  `Bootstrap failed: 5: Input/output error` (#6618). Every effect is injected, so
  the ordering is asserted rather than described.
- `LaunchdConfig::restart_gracefully` binds that sequence to the real
  `launchctl`, the real SIGTERM, and a one-second clock. Its wait budget is
  `shutdown::termination_grace()` — launchd cannot deregister a job before the
  process it is terminating has gone, so a shorter budget would give up
  mid-unload.
