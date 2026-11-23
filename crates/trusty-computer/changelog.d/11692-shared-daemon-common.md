Changed
- `peer`, `private`, `codesign`, `launchd` and `audit` now use the shared `trusty-daemon-common` crate instead of copies of the trusty-browser code; public items keep their paths and signatures. The plist writer now normalises `\r` in arguments as plistlib does, and the plist is published through a random temp name (#11692).
