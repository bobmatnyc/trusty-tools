Fixed
- `trusty-search start` (background) now starts the daemon in its own session, so a group kill or Ctrl-C aimed at the caller no longer takes the daemon down with it. The daemon auto-started by `tga audit` gets the same fix through `trusty-common` (#8783).
