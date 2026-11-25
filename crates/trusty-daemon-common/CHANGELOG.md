# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.1.0] — 2026-11-25

### Added

- New crate `trusty-daemon-common` 0.1.0 with the code `trusty-browser` and `trusty-computer` shared by copy: `private` (owner-only file checks), `peer` (kernel peer uid), `codesign` (Developer ID signing per `SignSpec`), `launchd` (plist, `launchctl` seam, owned-file writes) and `audit_file` (fail-closed append and 5 MiB rotation) (#11692).

