Fixed

- `service install` evicts the launchd labels earlier installs registered.
  Removing the Makefile's `launchctl unload` + `rm -f` of
  `com.trusty.trusty-memory` without moving that job into the installer would
  have LOST an eviction this crate already had, leaving a stale unit for a later
  bootstrap to find. `deploy` also boots out the legacy label before
  `cargo install` (#4868)
