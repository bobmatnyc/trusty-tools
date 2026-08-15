Fixed
- Stopped the doctor `checks` module doc from linking to the macOS-gated
  `check_launchd_plist`, which rustdoc cannot resolve on Linux. docs.rs builds
  on Linux once per release and never rebuilds.
