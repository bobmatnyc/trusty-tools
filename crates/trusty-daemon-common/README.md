# trusty-daemon-common

Shared plumbing for the `trusty-browser` and `trusty-computer` broker daemons
(issue #11692):

- `private` — owner-only (0700 / 0600) directory and file checks.
- `peer` — the uid the kernel recorded for a Unix-socket peer.
- `codesign` — Developer ID signing and verification against a per-product
  `SignSpec`.
- `launchd` — the per-user job plist, a `launchctl` seam, and file writes that
  never overwrite a file the installer did not write.
- `audit_file` — append one line to an owner-only log and rotate it at 5 MiB.

Each consumer keeps its own refusal codes, audit line format and install
verbs; this crate holds only the parts both share.
