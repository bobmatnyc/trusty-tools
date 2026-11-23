Changed
- `peer`, `private`, `codesign`, `launchd`, `install_host` and `audit` now use the shared `trusty-daemon-common` crate for the peer uid read, owner-only file checks, signing, the plist writer, owned-file writes and audit rotation; public items keep their paths and signatures, and `write_owned` now fsyncs before linking (#11692).
