Fixed

- `trusty-search service install` now installs the unit launchd actually has
  loaded. `LAUNCHD_LABEL` was `com.trusty.trusty-search`; the live agent is
  `com.trusty.search`. So install wrote a plist under the wrong name,
  bootstrapped a SECOND daemon contending for :7878 and the index locks, booted
  out nothing, and left #4393's `ExitTimeOut` fix in a file launchd never reads
  — the corrected plist was written but never activated. The label now comes
  from `trusty_common::launchd_labels::SEARCH`, and install evicts the labels
  earlier installers registered (`com.trusty.trusty-search`,
  `com.bobmatnyc.trusty-search`) so #2938's stranded duplicate is cleaned up
  rather than resurrected (#4868)
- Re-running `service install` with no configuration change no longer restarts
  the daemon: the unit is reloaded only when the rendered plist differs from
  what is installed or the label is not loaded. A failed activation restores and
  re-bootstraps the previous plist instead of leaving search down (#4868)
- The log-rotation agent's label is derived from the daemon's rather than
  restated, and install evicts the orphaned
  `com.trusty.trusty-search.logrotate` a prior version left loaded (#4868)
- `make deploy` no longer declares `com.bobmatnyc.trusty-search` canonical — a
  third label family that has never existed on a host. Every deploy therefore
  unloaded a missing file, killed the daemon, failed to load it back, and fell
  through to `trusty-search start`, leaving it unsupervised and CLI-detached for
  the whole `cargo install`. The target now defers to `service install` (#4868)
