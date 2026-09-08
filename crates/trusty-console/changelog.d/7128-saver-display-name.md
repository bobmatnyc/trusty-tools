Changed
- The screen saver's tile in System Settings > Screen Saver > Other reads
  `Trusty Console` instead of `TrustyConsole`. The bundle's `Info.plist` now
  sets `CFBundleDisplayName` and `CFBundleName` to the spaced form; the bundle
  identifier `com.trusty.console.saver`, the `TrustyConsole.saver` filename, the
  `CFBundleExecutable` and the principal class are unchanged, so launchd, the
  `log` subsystem predicate and Settings' selected-saver state still resolve
  ([#7128](https://github.com/bobmatnyc/trusty-tools/issues/7128)).
