Fixed

- `tm doctor --fix --yes` now sets `ProcessType=Interactive` in the
  `com.trusty.mpm` daemon and `com.trusty.mpm.supervisor` LaunchAgent plists
  (#8562), which clears the `launchd_process_type` row without a manual
  `plutil` step. It replaces an existing value or adds the key, writes
  atomically, and refuses binary or symlinked plists. It rewrites the plist
  file only: launchd is not reloaded and the running daemon is not touched, so
  the new class takes effect at the next daemon restart. The row's findings
  name this command first.
