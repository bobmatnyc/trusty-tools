Fixed

- `launchd_labels` is now the one definition of every trusty-* LaunchAgent's
  label, and each daemon crate, the installer, and `tctl` read it instead of
  restating their own literal. They had drifted: `trusty-search service install`
  wrote and bootstrapped `com.trusty.trusty-search` while the unit launchd
  actually had loaded was `com.trusty.search`, so the install evicted nothing,
  started a second daemon contending for :7878 and the index locks (#2938), and
  left #4393's `ExitTimeOut` in a plist launchd never reads. `trusty-console`
  had the same divergence (`com.trusty.trusty-console` in code,
  `com.trusty.console` loaded). The canonical form is `com.trusty.<member with
  its `trusty-` prefix stripped>`, which every loaded unit on a real host obeys;
  `canonical_label` is that rule as code and the registry table is checked
  against it. Correcting one literal is what was done for #2827, and the defect
  came back — so the second copy is gone rather than corrected (#4868)
- `LaunchdConfig::install_and_activate` replaces the bare `install()` +
  `bootstrap()` pair for service installs. It boots out the service's recorded
  legacy labels and deletes their plists first, so an upgrade cannot leave the
  old unit running beside the new one; skips the reload entirely when the
  rendered plist matches what is installed and the label is already loaded,
  which is where the ~1 minute of release downtime came from; verifies the label
  actually came up rather than trusting `launchctl bootstrap`'s exit code
  (#2498); and restores plus re-bootstraps the previous plist if activation
  fails, so a failed install no longer leaves the service down (#4868)
- A workspace-scanning test now fails on any `com.trusty.*` / `com.bobmatnyc.*`
  label literal in production source that the registry does not own. Codesign
  identifiers (`macos_signing`) are exempt — they are a different namespace and
  renaming one invalidates a binary's designated requirement (#2558) (#4868)
