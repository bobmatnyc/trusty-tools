Fixed
- `trusty-analyze start` now spawns the daemon through the shared detached helper, so it starts in its own session and survives a group kill or Ctrl-C aimed at the caller (#8783).
