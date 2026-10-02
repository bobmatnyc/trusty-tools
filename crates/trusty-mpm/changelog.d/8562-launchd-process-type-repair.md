Fixed

- `tm doctor --fix --yes` now sets `ProcessType=Interactive` in the
  `com.trusty.mpm` daemon and `com.trusty.mpm.supervisor` LaunchAgent plists
  (#8562), which replaces the manual `plutil` step. It replaces an existing
  value or adds the key, writes atomically, and refuses binary or symlinked
  plists. It rewrites the plist file only and never runs `launchctl`. launchd
  applies the new class when it next loads the label: at login, or on
  `launchctl bootout` then `bootstrap`. A `launchctl kickstart` or a crash
  respawn does not re-read the plist, and restarting the tm daemon does not
  reload the supervisor. Each step and the row's findings say this for their
  own label.
- The `launchd_process_type` row no longer shows green before launchd has
  loaded the new class. It does not read the loaded class, because
  `launchctl print` prints the job's `EnvironmentVariables`, so an
  `Interactive` plist changed since this boot warns "written; pending reload"
  and passes after the next boot.
