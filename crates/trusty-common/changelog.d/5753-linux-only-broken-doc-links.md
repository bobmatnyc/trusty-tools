Fixed
- Stopped six doc comments in non-gated code from linking to macOS-gated items
  (`physical_footprint_mb`, `launchd`), which rustdoc could not resolve on
  Linux. #5744 measured the link count on macOS only, so these never appeared
  in its baseline and left the pre-publish contract gate red on `main`.
