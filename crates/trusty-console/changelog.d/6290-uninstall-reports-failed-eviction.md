Fixed

- `trusty-console service uninstall` reports a stale LaunchAgent it could not
  clear (#6290). It read only `evict_legacy`'s evicted-label list, so a failed
  bootout or plist deletion printed nothing at all.
