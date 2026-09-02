Fixed

- The stdio bridge no longer spawns an unsupervised daemon onto the production
  socket while launchd is restarting the unit. `ensure_daemon_running`'s
  single-flight `flock` (#5267/#6286) coordinates bridges with each other and
  cannot see launchd, so a bridge that probed during a `bootout`/`bootstrap`
  window read the transiently unserved socket as "nothing is running" and started
  its own daemon — without the plist's `FASTEMBED_CACHE_DIR` /
  `FASTEMBED_CACHE_PATH` — on the path launchd's own instance wanted. launchd's
  process then found the socket held and exited 0 ("another instance is already
  running"), reporting success while a misconfigured orphan owned the socket
  (#6619). The guard now asks whether a launchd unit owns the path and waits for
  it, bounded by the termination grace, erroring with the unit's label instead of
  spawning.
- A daemon startup refuses the production socket outright when a launchd unit is
  registered for it and launchd positively reports it does not run this process.
  This is the callee-side half, and it holds for a daemon started by anything —
  by hand, by a script, by an older bridge. It refuses only on a positive
  `NotSupervised`: `Unknown` means launchd could not be asked, and refusing on
  that would take the daemon down on every host with an unreadable `launchctl`.
- Both guards apply only to the canonical production socket. A daemon under a
  `TRUSTY_DATA_DIR_OVERRIDE` sandbox, or on a host that never installed the
  service, keeps the on-demand spawn unchanged.
