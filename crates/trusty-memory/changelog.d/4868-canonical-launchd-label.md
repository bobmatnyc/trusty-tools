Fixed

- `make deploy` no longer declares `com.bobmatnyc.trusty-memory` canonical — a
  label no host has ever had — nor unloads a `com.trusty.trusty-memory` the
  daemon does not use. The real unit is `com.trusty.memory`. The target now
  defers to `trusty-memory service install`, which evicts old labels under
  launchd with a rollback, instead of a shell `unload`/`load` pair that could
  fail silently and leave the daemon running CLI-detached (#4868)
