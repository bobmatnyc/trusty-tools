Fixed
- `trusty-analyze service uninstall` reports a unit it could not clear and exits
  non-zero. It used to fold "no plist" and "removal failed" into one `false`, so
  a surviving file rendered as evicted or absent and the command exited 0 while
  launchd still reloaded the unit at next login. It now delegates the eviction
  to `LaunchdConfig::evict_legacy_detailed` — the workspace's one
  implementation, which also verifies launchd actually let go rather than
  trusting `bootout`'s exit code — and reports its `EvictionOutcome` per label
  (#6350).
- `--help` no longer advertises `service install`, `service status` and
  `service logs`; all three were removed from the CLI and each exited 2. The
  retired-`--port` warning points at `service uninstall` rather than the
  `service install` that no longer exists (#6350).
