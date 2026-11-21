Fixed

- `trusty-browser install-service` restarts an already-loaded broker (`launchctl kickstart -k`), so an upgrade serves the new binary and extension asset stamp. A failed restart exits non-zero with the launchctl error.
