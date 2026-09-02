Fixed
- `ensure_daemon_running` no longer races a launchd-supervised
  `com.trusty.analyze` unit onto its own socket. The PID-file check that
  coordinates this daemon's own bridges cannot see launchd, so during a
  bootout/bootstrap window left by a pre-#6350 install the socket read as
  "nothing is running" and a bridge would spawn a second, unsupervised
  process. The guard now asks `trusty_common::launchd_claim` first and waits
  for the unit instead of spawning — a no-op on the ordinary host, where
  ADR-0032 means no plist is installed at all (#6624).
